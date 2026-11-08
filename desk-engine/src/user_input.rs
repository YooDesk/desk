//! 用户已获设备ACL，复用RustDesk的用户私有uinput服务和长期客户端运行时。
use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static BOUNDS: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
static SERVICE: OnceLock<Mutex<std::process::Child>> = OnceLock::new();
static PORTAL_INPUT: OnceLock<()> = OnceLock::new();
/// 用户服务线程只在需要uinput的会话启动，不要求DRM helper存在。
pub(crate) fn start_user_input() -> Result<(), String> {
    if std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() != Ok("1") {
        return Ok(());
    }
    if unsafe { hbb_common::libc::geteuid() } == 0 {
        return Err("uinput worker must use graphical user".into());
    }
    // 上游按独立service进程设计；若与Portal共进程，服务端会误读桌面RDP缓存，
    // 在未授予Portal输入时逐字符等待D-Bus超时。隔离进程只持有uinput，不持有Portal。
    let mut child = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .arg("--user-input-service")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(error) = wait_for_listeners(&mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    SERVICE
        .set(Mutex::new(child))
        .map_err(|_| "input service already started")?;
    Ok(())
}

/// 用户输入专用子进程；stdin由桌面worker独占，worker退出后EOF立即结束全部服务线程。
#[no_mangle]
pub extern "C" fn yd_engine_linux_user_input_service() -> i32 {
    if unsafe { hbb_common::libc::geteuid() } == 0
        || std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() != Ok("1")
        || super::configure_namespace().is_err()
    {
        return crate::YD_ERR_STATE;
    }
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
    for (name, start) in [
        (
            "uinput-control",
            librustdesk::uinput::service::start_service_control as fn(),
        ),
        (
            "uinput-keyboard",
            librustdesk::uinput::service::start_service_keyboard as fn(),
        ),
        (
            "uinput-mouse",
            librustdesk::uinput::service::start_service_mouse as fn(),
        ),
    ] {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(start)
            .unwrap_or_else(|_| std::process::exit(1));
    }
    let mut buffer = [0u8; 1];
    // 不接受控制命令；此管道仅表示父进程生命期。
    use std::io::Read;
    while std::io::stdin().read(&mut buffer).is_ok_and(|n| n != 0) {}
    crate::YD_OK
}

/// 等待真实监听器，子进程提前失败必须立即向宿主报告。
fn wait_for_listeners(child: &mut std::process::Child) -> Result<(), String> {
    let path = std::path::PathBuf::from(hbb_common::config::Config::ipc_path("_uinput_control"));
    let directory = path.parent().ok_or("invalid input IPC")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !["control", "keyboard", "mouse"]
        .iter()
        .all(|n| directory.join(format!("ipc_uinput_{n}.pid")).is_file())
    {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err("user input service exited before readiness".into());
        }
        if Instant::now() >= deadline {
            return Err("user uinput listeners not ready".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
/// 首次按真实布局建立uinput客户端；后续显示布局变化更新范围，runtime不提前销毁。
pub(crate) fn refresh_user_input(displays: &[scrap::Display]) -> Result<(), String> {
    let geometry = displays
        .iter()
        .map(|d| {
            [
                d.width() as i32,
                d.height() as i32,
                d.origin().0,
                d.origin().1,
            ]
        })
        .collect::<Vec<_>>();
    refresh_geometry(&geometry)
}

/// 使用授权元数据或真实Display几何更新输入，不为查询输入范围重复打开捕获管线。
pub(crate) fn refresh_geometry(displays: &[[i32; 4]]) -> Result<(), String> {
    if std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() != Ok("1") {
        // 无设备授权时，只有RemoteDesktop明确授予键鼠才安装上游公开Portal输入适配。
        if crate::wayland_portal::flags() & 15 == 15 && PORTAL_INPUT.get().is_none() {
            let _io = crate::wayland_portal::IO
                .lock()
                .map_err(|e| e.to_string())?;
            input_runtime()?
                .block_on(librustdesk::input_service::setup_rdp_input())
                .map_err(|e| e.to_string())?;
            let _ = PORTAL_INPUT.set(());
        }
        return Ok(());
    }
    let rect = scrap::wayland::display::get_layout_for_uinput_live()
        .map(|(r, _)| r)
        .or_else(|| {
            Some((
                displays.iter().map(|d| d[2]).min()?,
                displays.iter().map(|d| d[2].saturating_add(d[0])).max()?,
                displays.iter().map(|d| d[3]).min()?,
                displays.iter().map(|d| d[3].saturating_add(d[1])).max()?,
            ))
        })
        .filter(|(a, b, c, d)| a < b && c < d)
        .ok_or("real uinput display bounds unavailable")?;
    refresh_rect(rect)
}

/// DRM与Portal共享同一真实布局应用入口，不为DRM重新建立Portal捕获会话。
pub(crate) fn refresh_rect(rect: (i32, i32, i32, i32)) -> Result<(), String> {
    if std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() != Ok("1") {
        return Ok(());
    }
    if rect.0 >= rect.1 || rect.2 >= rect.3 {
        return Err("invalid uinput bounds".into());
    }
    let mut bounds = BOUNDS.lock().map_err(|e| e.to_string())?;
    if *bounds == Some(rect) {
        return Ok(());
    }
    let runtime = input_runtime()?;
    let (a, b, c, d) = rect;
    if bounds.is_none() {
        runtime
            .block_on(librustdesk::input_service::setup_uinput(a, b, c, d))
            .map_err(|e| e.to_string())?;
    } else {
        runtime
            .block_on(librustdesk::input_service::update_mouse_resolution(
                a, b, c, d,
            ))
            .map_err(|e| e.to_string())?;
    }
    *bounds = Some(rect);
    Ok(())
}

/// 客户端连接绑定长期runtime，不能在临时block_on结束时撤销其IO驱动。
fn input_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    if RUNTIME.get().is_none() {
        let r = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let _ = RUNTIME.set(r);
    }
    RUNTIME
        .get()
        .ok_or_else(|| "input runtime unavailable".into())
}
