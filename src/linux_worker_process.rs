//! C++ 式用户 worker 与可选 DRM-only root 子进程，身份/ACL/进程组/清单属于同一代。
use super::Desktop;
use crate::desk::engine::rpc::Transport;
use serde::{Deserialize, Serialize};
use std::{
    io,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Arc,
    time::{Duration, Instant},
};
#[derive(Serialize, Deserialize)]
struct Record {
    v: u32,
    helper: Option<u32>,
    user: Option<u32>,
    uid: u32,
    namespace: String,
}
pub(super) struct Worker {
    pub(super) desktop: Desktop,
    helper: Option<Child>,
    user: Option<Child>,
    pub(super) helper_control: Option<Arc<dyn Transport>>,
    record: PathBuf,
    namespace: String,
    drm: bool,
    x11: bool,
    device: Option<(u64, u64)>,
    _input: super::super::linux_uinput_access::Lease,
}
impl Worker {
    pub(super) fn prefers_x11(&self) -> bool {
        self.x11
    }
    pub(super) fn alive(&mut self) -> bool {
        self.user.as_mut().is_some_and(running)
            && self.helper.as_mut().map(running).unwrap_or(true)
            && self.device == device_identity()
    }
    pub(super) fn helper_id(&self) -> u32 {
        self.helper.as_ref().map(Child::id).unwrap_or(0)
    }
    pub(super) fn user_id(&self) -> u32 {
        self.user.as_ref().map(Child::id).unwrap_or(0)
    }
    // 记录在各角色收到 hello 之前持久化，子进程此时尚未初始化 native。
    fn save(&self, initial: bool) -> io::Result<()> {
        use std::io::Write;
        let value = Record {
            v: 1,
            helper: self.helper.as_ref().map(Child::id),
            user: self.user.as_ref().map(Child::id),
            uid: self.desktop.uid,
            namespace: self.namespace.clone(),
        };
        let bytes = serde_json::to_vec(&value).map_err(io::Error::other)?;
        let temporary = self.record.with_extension("updating");
        let path = if initial { &self.record } else { &temporary };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(0o400000)
            .open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if !initial {
            std::fs::rename(temporary, &self.record)?;
        }
        Ok(())
    }
    // root 候选只接 DRM，探测模式不启动 producer；完整会话环境只交给用户 worker。
    pub(super) fn start_helper(&mut self, socket: &str, probe: bool) -> io::Result<u32> {
        let mut command = base("--privileged-worker", socket)?;
        command
            .env("HOME", "/root")
            .env("USER", "root")
            .env("LOGNAME", "root")
            .env("YOODESK_ENGINE_NAMESPACE", &self.namespace)
            .env("YOODESK_ENGINE_HELPER_UID", self.desktop.uid.to_string())
            .env("YOODESK_ENGINE_HELPER_GID", self.desktop.gid.to_string());
        if probe {
            command.env("YOODESK_ENGINE_HELPER_DRM_PROBE_ONLY", "1");
        }
        identity(&mut command, None)?;
        self.helper = Some(command.spawn()?);
        self.save(false)?;
        Ok(self.helper_id())
    }
    // 短探测无论成功失败均必须退出；只有常驻 helper ready 后才对用户声明 DRM。
    pub(super) fn finish_probe(&mut self) {
        if let Some(control) = self.helper_control.take() {
            control.close();
        }
        if let Some(mut child) = self.helper.take() {
            stop_child(&mut child);
        }
        self.drm = false;
        let _ = self.save(false);
    }
    pub(super) fn enable_drm(&mut self, control: Arc<dyn Transport>) {
        self.helper_control = Some(control);
        self.drm = true;
    }
    // 权限来自已授予的目标 UID /dev/uinput ACL，用户进程不保留 root supplementary groups。
    pub(super) fn start_user(&mut self, socket: &str) -> io::Result<u32> {
        let mut command = base("--rustdesk-worker", socket)?;
        command
            .envs(&self.desktop.env)
            .env("YOODESK_ENGINE_NAMESPACE", &self.namespace)
            .env(
                "YOODESK_ENGINE_SERVICE_NAME",
                std::env::var("YOODESK_NATIVE_SERVICE_NAME").map_err(io::Error::other)?,
            )
            .env(
                "YOODESK_ENGINE_DRM_ENABLED",
                if self.drm { "1" } else { "0" },
            );
        if self._input.available()
            && self
                .desktop
                .env
                .get("XDG_SESSION_TYPE")
                .is_some_and(|kind| kind == "wayland")
        {
            command.env("YOODESK_ENGINE_USER_UINPUT", "1");
        }
        identity(&mut command, Some(&self.desktop))?;
        if self.x11 {
            command.env("RUSTDESK_FORCED_DISPLAY_SERVER", "x11");
        }
        self.user = Some(command.spawn()?);
        self.save(false)?;
        Ok(self.user_id())
    }
}
// 设备节点重建后旧ACL不再有效，触发同代重建与重新授权。
fn device_identity() -> Option<(u64, u64)> {
    std::fs::metadata("/dev/uinput")
        .ok()
        .map(|m| (m.dev(), m.ino()))
}
fn running(child: &mut Child) -> bool {
    child.try_wait().map(|s| s.is_none()).unwrap_or(false)
}
// EOF 给原生任务一次有界正常回收机会，挂起或失败进程再按独立进程组强制回收。
fn stop_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline && running(child) {
        std::thread::sleep(Duration::from_millis(10));
    }
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    unsafe {
        kill(-(child.id() as i32), 9);
    }
    let _ = child.kill();
    let _ = child.wait();
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(control) = self.helper_control.take() {
            control.close();
        }
        if let Some(user) = self.user.as_mut() {
            stop_child(user);
        }
        if let Some(helper) = self.helper.as_mut() {
            stop_child(helper);
        }
        match cleanup(&self.namespace, self.desktop.uid) {
            Ok(()) => {
                let _ = std::fs::remove_file(&self.record);
                let _ = std::fs::remove_file(self.record.with_extension("updating"));
            }
            Err(error) => tracing::warn!(%error,"native cleanup record retained"),
        }
    }
}
// 只传角色与认证 socket；环境从空集重建，任何 HTTP/密钥/动态链接环境都不继承。
fn base(role: &str, socket: &str) -> io::Result<Command> {
    let executable = std::env::current_exe()?;
    let mut command = Command::new(&executable);
    command.args([
        role,
        "--pipe",
        socket,
        "--parent-pid",
        &std::process::id().to_string(),
    ]);
    command.current_dir(
        executable
            .parent()
            .ok_or_else(|| io::Error::other("missing executable parent"))?,
    );
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .process_group(0);
    Ok(command)
}
// initgroups 的 NSS 解析在父中完成；fork 后仅系统调用，降权后重新建立 PDEATHSIG。
fn identity(command: &mut Command, desktop: Option<&Desktop>) -> io::Result<()> {
    let identity = desktop.map(|d| (d.uid, d.gid, d.groups.clone()));
    let parent = std::process::id();
    unsafe {
        command.pre_exec(move || {
            extern "C" {
                fn setgroups(size: usize, groups: *const u32) -> i32;
                fn setgid(gid: u32) -> i32;
                fn setuid(uid: u32) -> i32;
                fn geteuid() -> u32;
                fn getppid() -> i32;
                fn prctl(option: i32, ...) -> i32;
            }
            if let Some((uid, gid, groups)) = &identity {
                if *uid == 0
                    || setgroups(groups.len(), groups.as_ptr()) != 0
                    || setgid(*gid) != 0
                    || setuid(*uid) != 0
                    || geteuid() != *uid
                {
                    return Err(io::Error::last_os_error());
                }
            }
            if prctl(1, 9i32, 0usize, 0usize, 0usize) != 0 {
                return Err(io::Error::last_os_error());
            }
            if getppid() != parent as i32 {
                return Err(io::Error::other("parent exited"));
            }
            Ok(())
        });
    }
    Ok(())
}
// 32 字符品牌仍给 Unix sun_path 留出余量；24 个随机 hex 提供 96 位代次随机量。
pub(super) fn spawn(desktop: Desktop, records: &Path) -> io::Result<Worker> {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let namespace = format!("{}DeskEngine-{}", env!("PRODUCT_ID"), &nonce[..24]);
    let service = std::env::var("YOODESK_NATIVE_SERVICE_NAME").map_err(io::Error::other)?;
    let input = super::super::linux_uinput_access::acquire(desktop.uid, &desktop.groups, &service)?;
    let x11 = if desktop
        .env
        .get("XDG_SESSION_TYPE")
        .is_some_and(|v| v == "wayland")
    {
        probe_x11(&desktop)?
    } else {
        true
    };
    tracing::info!(x11_capture=x11, session=%desktop.session, "X11 actual frame probe completed");
    let worker = Worker {
        desktop,
        helper: None,
        user: None,
        helper_control: None,
        record: records.join(format!("generation-{nonce}.record")),
        namespace,
        drm: false,
        x11,
        device: device_identity(),
        _input: input,
    };
    worker.save(true)?;
    Ok(worker)
}

/// 只使用已匹配目标会话的DISPLAY/XAUTHORITY，不猜测其他用户的:0/:1。
fn probe_x11(desktop: &Desktop) -> io::Result<bool> {
    if !desktop.env.contains_key("DISPLAY") {
        return Ok(false);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--x11-capture-probe")
        .env_clear()
        .envs(&desktop.env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    identity(&mut command, Some(desktop))?;
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= deadline {
            stop_child(&mut child);
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
// 按可信 namespace 精确清理，root DRM 服务目录和用户 _pa/uinput 目录分别核对所有者。
fn cleanup(namespace: &str, uid: u32) -> io::Result<()> {
    if !namespace.starts_with(&format!("{}DeskEngine-", env!("PRODUCT_ID")))
        || namespace.len() > 80
        || !namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(io::Error::other("invalid native namespace"));
    }
    let temporary = Path::new("/tmp").symlink_metadata()?;
    if !temporary.is_dir() || temporary.uid() != 0 {
        return Err(io::Error::other("untrusted temporary root"));
    }
    for (suffix, owner) in [("service".to_owned(), 0), (uid.to_string(), uid)] {
        let path = Path::new("/tmp").join(format!("{namespace}-{suffix}"));
        let metadata = match path.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::other("untrusted native IPC directory"));
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("invalid native IPC name"))?;
            let base = name.strip_suffix(".pid").unwrap_or(name);
            if !matches!(
                base,
                "ipc_pa"
                    | "ipc_drm"
                    | "ipc_uinput_control"
                    | "ipc_uinput_keyboard"
                    | "ipc_uinput_mouse"
                    | "ready.json"
            ) {
                return Err(io::Error::other("unknown native IPC entry"));
            }
            let m = entry.path().symlink_metadata()?;
            if (m.uid() != owner && !(owner == 0 && m.uid() == uid))
                || m.file_type().is_symlink()
                || m.is_dir()
            {
                return Err(io::Error::other("untrusted native IPC entry"));
            }
            files.push(entry.path());
        }
        for file in files {
            std::fs::remove_file(file)?;
        }
        std::fs::remove_dir(path)?;
    }
    Ok(())
}
// Worker records belong to this plugin; validate/create their root-owned directory locally.
fn trusted_worker_directory(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut current = PathBuf::from("/");
    for component in path.components().skip(1) {
        current.push(component);
        match std::fs::create_dir(&current) {
            Ok(()) => std::fs::set_permissions(&current, std::fs::Permissions::from_mode(mode))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = current.symlink_metadata()?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::other(format!(
                "untrusted native worker directory: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn records() -> io::Result<PathBuf> {
    let name = std::env::var("YOODESK_NATIVE_SERVICE_NAME").map_err(io::Error::other)?;
    if name.is_empty()
        || matches!(name.as_str(), "." | "..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(io::Error::other("invalid service name"));
    }
    let directory = Path::new("/run")
        .join(format!("{}-native-workers", env!("PRODUCT_ID")))
        .join(name);
    trusted_worker_directory(&directory, 0o700)?;
    Ok(directory)
}
// 只消费本服务 root0700 目录的记录；任一记录 PID 仍存在时保守跳过，避免重用误清。
pub(super) fn recover(directory: &Path) -> io::Result<()> {
    use std::io::Read;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let m = entry.path().symlink_metadata()?;
        if !m.is_file() || m.uid() != 0 || m.mode() & 0o077 != 0 {
            return Err(io::Error::other("untrusted generation record"));
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other("invalid generation name"))?;
        let (stem, partial) = if let Some(s) = name.strip_suffix(".record") {
            (s, false)
        } else if let Some(s) = name.strip_suffix(".updating") {
            (s, true)
        } else {
            return Err(io::Error::other("invalid generation suffix"));
        };
        let nonce = stem
            .strip_prefix("generation-")
            .ok_or_else(|| io::Error::other("invalid generation prefix"))?;
        if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::other("invalid generation nonce"));
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0o400000)
            .open(entry.path())?;
        let mut bytes = Vec::new();
        file.by_ref().take(2048).read_to_end(&mut bytes)?;
        let record: Record = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if record.v != 1 || record.uid == 0 {
            return Err(io::Error::other("invalid generation record"));
        }
        if record
            .helper
            .into_iter()
            .chain(record.user)
            .any(|pid| Path::new("/proc").join(pid.to_string()).exists())
        {
            continue;
        }
        if !partial {
            cleanup(&record.namespace, record.uid)?;
        }
        std::fs::remove_file(entry.path())?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn maximum_brand_native_socket_fits_unix_path() {
        let namespace = format!("{}DeskEngine-{}", "a".repeat(32), "b".repeat(24));
        for suffix in ["service", "4294967295"] {
            let path = format!("/tmp/{namespace}-{suffix}/ipc_uinput_keyboard");
            assert!(path.len() < 108, "{}", path.len());
        }
    }
}
