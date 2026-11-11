//! 由 launchd 在当前控制台的 Aqua 或 LoginWindow 域启动 Desk worker。
//!
//! 本插件不注册 LaunchDaemon；系统安装、`/Library` 下的 plist 和 root 目录由
//! webservice 宿主的 `macos_service` 负责。这里只负责在已有 LaunchAgent/Daemon
//! 身份下选择控制台会话、拉起本插件 worker 进程并校验 IPC 对端。
#![allow(unsafe_code)]
use crate::{business::BusinessHub, desk::engine::rpc, unix_worker::UnixEndpoint};
use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

// 与 unix_worker/windows_worker 一致接收借用路径，由调用方保留所有权。
pub(crate) fn run_worker(path: &str, parent_pid: u32) -> io::Result<()> {
    if std::env::var_os("YOODESK_MACOS_LOGINWINDOW").is_none() {
        std::env::set_var("YOODESK_MACOS_AQUA_WORKER", "1");
    }
    // RustDesk 把键鼠事件排到主队列。保留实际的主线程和 GUI run loop，
    // 避免 dispatch_main 退出调用线程后由后台线程执行 CoreGraphics 输入。
    let path = path.to_owned();
    std::thread::Builder::new()
        .name("desk-ipc".into())
        .spawn(move || {
            let status = match crate::unix_worker::run_worker(path, parent_pid) {
                Ok(()) => 0,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => 0,
                Err(error) => {
                    eprintln!("macOS Desk worker: {error}");
                    1
                }
            };
            std::process::exit(status);
        })?;
    run_main_loop()
}

fn run_main_loop() -> io::Result<()> {
    use std::{ffi::c_void, ptr};

    type Ref = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: Ref;
        fn CFAbsoluteTimeGetCurrent() -> f64;
        fn CFRunLoopGetMain() -> Ref;
        fn CFRunLoopTimerCreate(
            allocator: Ref,
            fire_date: f64,
            interval: f64,
            flags: usize,
            order: isize,
            callback: extern "C" fn(Ref, *mut c_void),
            context: Ref,
        ) -> Ref;
        fn CFRunLoopAddTimer(run_loop: Ref, timer: Ref, mode: Ref);
        fn CFRunLoopRun();
        fn CFRelease(value: Ref);
    }
    extern "C" fn keep_alive(_: Ref, _: *mut c_void) {}

    // Without a source or timer, CoreFoundation can return from an empty run loop.
    unsafe {
        let interval = 3600.0;
        let timer = CFRunLoopTimerCreate(
            ptr::null(),
            CFAbsoluteTimeGetCurrent() + interval,
            interval,
            0,
            0,
            keep_alive,
            ptr::null(),
        );
        if timer.is_null() {
            return Err(io::Error::other(
                "could not create macOS Desk run loop timer",
            ));
        }
        CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopDefaultMode);
        CFRelease(timer);
        CFRunLoopRun();
    }
    Err(io::Error::other("macOS Desk main run loop stopped"))
}

#[derive(Debug, PartialEq, Eq)]
struct Session {
    domain: String,
    // 审计会话号用于诊断；launchctl 的域字符串已经唯一标识会话。
    #[allow(dead_code)]
    asid: u32,
    uid: u32,
    gid: u32,
    kind: &'static str,
}

fn launchctl(args: &[&str]) -> io::Result<Output> {
    Command::new("/bin/launchctl").args(args).output()
}

fn checked(result: Output) -> io::Result<String> {
    if !result.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&result.stderr).trim(),
        ));
    }
    String::from_utf8(result.stdout).map_err(io::Error::other)
}

fn field_number(text: &str, key: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let (name, value) = line.trim().split_once(" = ")?;
        (name == key).then(|| value.parse::<u32>().ok()).flatten()
    })
}

fn active_session() -> io::Result<Session> {
    let uid = fs::metadata("/dev/console")?.uid();
    if uid == 0 {
        let info = checked(launchctl(&["print", "user/0"])?)?;
        let asid = field_number(&info, "asid")
            .filter(|n| *n != 0)
            .ok_or_else(|| io::Error::other("LoginWindow audit session is not ready"))?;
        Ok(Session {
            domain: "LoginWindow".into(),
            asid,
            uid,
            gid: 0,
            kind: "LoginWindow",
        })
    } else {
        let domain = format!("gui/{uid}");
        let info = checked(launchctl(&["print", &domain])?)?;
        let asid = field_number(&info, "asid")
            .filter(|n| *n != 0)
            .ok_or_else(|| io::Error::other("Aqua audit session is not ready"))?;
        let gid = checked(
            Command::new("/usr/bin/id")
                .args(["-g", &uid.to_string()])
                .output()?,
        )?
        .trim()
        .parse::<u32>()
        .map_err(io::Error::other)?;
        Ok(Session {
            domain,
            asid,
            uid,
            gid,
            kind: "Aqua",
        })
    }
}

enum Agent {
    Aqua(String),
    LoginWindow(PathBuf),
}

impl Drop for Agent {
    fn drop(&mut self) {
        let Self::Aqua(service) = self else {
            if let Self::LoginWindow(path) = self {
                let _ = fs::remove_file(path);
            }
            return;
        };
        if let Ok(result) = launchctl(&["bootout", service]) {
            if !result.status.success() && result.status.code() != Some(113) {
                tracing::warn!(service = %service, "无法停止 macOS Desk worker");
            }
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct LoginWindowHandoff {
    worker: u32,
    parent: u32,
    socket: String,
}

fn loginwindow_handoff() -> PathBuf {
    Path::new("/private/var/run")
        .join(format!("com.{}-desk-loginwindow", env!("PRODUCT_ID")))
        .join("worker.json")
}

unsafe extern "C" {
    fn geteuid() -> u32;
}

fn require_root() -> io::Result<()> {
    if unsafe { geteuid() } != 0 {
        return Err(io::Error::other("macOS LoginWindow worker 需要管理员权限"));
    }
    Ok(())
}

// 由系统 LoginWindow LaunchAgent 调用。等待根服务分配本代私有套接字。
pub(crate) fn run_loginwindow_agent() -> io::Result<()> {
    require_root()?;
    // 在启动任何线程前标记受控角色；RustDesk 仅在此角色选登录窗口输入。
    std::env::set_var("YOODESK_MACOS_LOGINWINDOW", "1");
    let path = loginwindow_handoff();
    while fs::metadata("/dev/console")?.uid() == 0 {
        match fs::read(&path) {
            Ok(bytes) => {
                let handoff: LoginWindowHandoff = serde_json::from_slice(&bytes)?;
                if handoff.worker == std::process::id() {
                    return run_worker(&handoff.socket, handoff.parent);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "不安全的安装目录: {}",
            path.display()
        )));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write;
    let temporary = path.with_file_name(format!(".yoodesk-{}", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// LoginWindow 的 job 不能可靠地用 launchctl print 查询。按上游做法识别
// root、安装路径和固定角色，再用 Unix 套接字内核凭据校验实际对端 PID/UID。
fn loginwindow_agent_pid() -> io::Result<u32> {
    let executable = std::env::current_exe()?;
    let expected = format!("{} --macos-loginwindow-agent", executable.display());
    let processes = checked(
        Command::new("/bin/ps")
            .args(["-axww", "-o", "pid=,uid=,command="])
            .output()?,
    )?;
    let mut matching = processes.lines().filter_map(|line| {
        let (pid, tail) = line.trim_start().split_once(char::is_whitespace)?;
        let (uid, command) = tail.trim_start().split_once(char::is_whitespace)?;
        if uid == "0" && command.trim_start() == expected {
            return pid.parse::<u32>().ok();
        }
        None
    });
    let pid = matching
        .next()
        .ok_or_else(|| io::Error::other("LoginWindow LaunchAgent is not running"))?;
    if matching.next().is_some() {
        return Err(io::Error::other("more than one LoginWindow LaunchAgent"));
    }
    Ok(pid)
}

fn start_loginwindow(session: &Session) -> io::Result<(Agent, Arc<dyn rpc::Transport>)> {
    let pid = loginwindow_agent_pid()?;
    let endpoint = UnixEndpoint::new_for_user(0, 0)?;
    let path = loginwindow_handoff();
    let agent = Agent::LoginWindow(path.clone());
    private_dir(path.parent().unwrap()).map_err(io::Error::other)?;
    let handoff = LoginWindowHandoff {
        worker: pid,
        parent: std::process::id(),
        socket: endpoint.name().into(),
    };
    atomic_write(&path, &serde_json::to_vec(&handoff)?, 0o600).map_err(io::Error::other)?;
    // LaunchAgent 每 200ms 轮询交接文件，root 侧给足启动与 IPC 握手时间。
    let transport = endpoint.accept(pid, 0, Duration::from_secs(30))?;
    if active_session()? != *session {
        return Err(io::Error::other("console session changed while connecting"));
    }
    Ok((agent, transport))
}

fn start(session: &Session) -> io::Result<(Agent, Arc<dyn rpc::Transport>)> {
    if session.uid == 0 {
        return start_loginwindow(session);
    }
    let endpoint = UnixEndpoint::new_for_user(session.uid, session.gid)?;
    let executable = std::env::current_exe()?;
    let program = executable
        .to_str()
        .ok_or_else(|| io::Error::other("invalid worker path"))?;
    let label = format!("com.{}-desk-worker", env!("PRODUCT_ID"));
    let target = format!("{}/{}", session.domain, label);
    // 服务异常退出时，清理上次同一登录域注册的 job 后再绑定新 PID。
    let previous = launchctl(&["print", &target])?;
    if previous.status.success() {
        checked(launchctl(&["bootout", &target])?)?;
    }
    let plist = Path::new(endpoint.name()).with_file_name("worker.plist");
    let args = [
        program.to_owned(),
        "--rustdesk-worker".into(),
        "--pipe".into(),
        endpoint.name().into(),
        "--parent-pid".into(),
        std::process::id().to_string(),
    ];
    let args: String = args
        .iter()
        .map(|arg| format!("<string>{}</string>", xml(arg)))
        .collect();
    let journal = format!(
        "<key>EnvironmentVariables</key><dict><key>YOODESK_ENGINE_SERVICE_NAME</key><string>{label}</string></dict>"
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array>{args}</array>
<key>LimitLoadToSessionType</key><string>{}</string>
<key>RunAtLoad</key><true/>
{journal}
</dict></plist>
"#,
        session.kind
    );
    use std::io::Write;
    let result = (|| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&plist)?
            .write_all(document.as_bytes())?;
        checked(launchctl(&[
            "bootstrap",
            &session.domain,
            plist.to_str().unwrap(),
        ])?)?;
        Ok::<_, io::Error>(Agent::Aqua(target.clone()))
    })();
    let _ = fs::remove_file(&plist);
    let agent = result?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        let description = checked(launchctl(&["print", &target])?)?;
        if let Some(pid) = field_number(&description, "pid").filter(|n| *n != 0) {
            break pid;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("launchd did not start the desktop worker"));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let transport = endpoint.accept(pid, session.uid, Duration::from_secs(5))?;
    if active_session()? != *session {
        return Err(io::Error::other("console session changed while connecting"));
    }
    Ok((agent, transport))
}

pub(crate) async fn supervise(hub: Arc<BusinessHub>, mut stop: watch::Receiver<bool>) {
    let mut active = None::<(Session, Agent)>;
    let mut failure = None;
    while !*stop.borrow() {
        let current = active_session();
        if active.as_ref().is_some_and(|(session, _)| {
            current.as_ref().ok() != Some(session) || !rpc::worker_healthy()
        }) {
            rpc::set_transport(None);
            hub.desk.close_all("desktop_session_changed");
            active.take();
        }
        if active.is_none() {
            let result = match current {
                Ok(session) => tokio::task::spawn_blocking(move || {
                    let (agent, transport) = start(&session)?;
                    Ok::<_, io::Error>((session, agent, transport))
                })
                .await
                .unwrap_or_else(|error| Err(io::Error::other(error))),
                Err(error) => Err(error),
            };
            match result {
                Ok((session, agent, transport)) => {
                    tracing::info!(domain = %session.domain, "macOS Desk worker 已连接");
                    rpc::set_transport(Some(transport));
                    active = Some((session, agent));
                    failure = None;
                }
                Err(error) => {
                    let message = error.to_string();
                    if failure.as_ref() != Some(&message) {
                        tracing::warn!(%error, "macOS Desk worker 未就绪");
                        failure = Some(message);
                    }
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = stop.changed() => break,
        }
    }
    rpc::set_transport(None);
}
