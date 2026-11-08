//! Linux沿用C++职责：root子仅DRM，用户子承担桌面/音频/uinput。
use std::{
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::PathBuf,
    time::{Duration, Instant},
};
#[path = "drm_probe.rs"]
mod drm_probe;
#[path = "user_input.rs"]
mod user_input;
pub(crate) use user_input::{refresh_user_input, start_user_input};

/// DRM和Portal均用已建立会话的元数据枚举；原生X11才交给Display::all。
pub(crate) fn capture_geometry() -> Result<Option<Vec<[i32; 4]>>, String> {
    if let Some(geometry) = drm_geometry()? {
        return Ok(Some(geometry));
    }
    if let Some(geometry) = crate::wayland_portal::geometry()? {
        user_input::refresh_geometry(&geometry)?;
        return Ok(Some(geometry));
    }
    Ok(None)
}

/// 已获helper支持的DRM只取成熟consumer几何，不得在枚举/码率阶段先进入Portal。
pub(crate) fn drm_geometry() -> Result<Option<Vec<[i32; 4]>>, String> {
    if std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() != Ok("1") {
        return Ok(None);
    }
    let displays = librustdesk::yoodesk_drm_display_infos()
        .filter(|displays| !displays.is_empty())
        .ok_or("DRM helper geometry unavailable")?;
    let geometry = displays
        .into_iter()
        .map(|display| [display.width, display.height, display.x, display.y])
        .collect::<Vec<_>>();
    if geometry
        .iter()
        .any(|display| display[0] <= 0 || display[1] <= 0)
    {
        return Err("invalid DRM helper geometry".into());
    }
    let rect = scrap::wayland::display::get_layout_for_uinput_live()
        .map(|(rect, _)| rect)
        .unwrap_or((
            geometry.iter().map(|d| d[2]).min().unwrap(),
            geometry
                .iter()
                .map(|d| d[2].saturating_add(d[0]))
                .max()
                .unwrap(),
            geometry.iter().map(|d| d[3]).min().unwrap(),
            geometry
                .iter()
                .map(|d| d[3].saturating_add(d[1]))
                .max()
                .unwrap(),
        ));
    user_input::refresh_rect(rect)?;
    Ok(Some(geometry))
}
/// 品牌私有共享命名空间，保留Unix路径长度预算。
pub(crate) fn configure_namespace() -> Result<(), String> {
    if let Some(value) = std::env::var_os("YOODESK_ENGINE_NAMESPACE") {
        let value = value.into_string().map_err(|_| "invalid namespace")?;
        if !value.starts_with(&format!("{}DeskEngine-", env!("PRODUCT_ID")))
            || value.len() > 74
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid namespace".into());
        }
        *hbb_common::config::APP_NAME.write().unwrap() = value;
    }
    Ok(())
}
/// DRM监听就绪后仅目标用户可连；构建补丁保证上游不会随后放宽600权限。
fn restrict_drm(uid: u32, gid: u32) -> Result<(), String> {
    let service = PathBuf::from(hbb_common::config::Config::ipc_path("_service"));
    let directory = service.parent().ok_or("invalid IPC directory")?;
    let socket = directory.join("ipc_drm");
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.symlink_metadata().is_err() {
        if Instant::now() >= deadline {
            return Err("DRM listener not ready".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let dm = directory.symlink_metadata().map_err(|e| e.to_string())?;
    let sm = socket.symlink_metadata().map_err(|e| e.to_string())?;
    if !dm.is_dir()
        || dm.uid() != 0
        || dm.mode() & 0o022 != 0
        || !sm.file_type().is_socket()
        || sm.uid() != 0
    {
        return Err("untrusted DRM IPC".into());
    }
    let change = |p: &std::path::Path, u, g| -> Result<(), String> {
        let p = std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        if unsafe { hbb_common::libc::lchown(p.as_ptr(), u, g) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    };
    change(&socket, uid, gid)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    change(directory, 0, gid)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o710))
        .map_err(|e| e.to_string())?;
    Ok(())
}
/// root控制握手后先实抓探测；probe-only不启动producer，常驻helper也不启动uinput/audio。
#[no_mangle]
pub extern "C" fn yd_engine_linux_privileged_init(uid: u32, gid: u32) -> i32 {
    if uid == 0
        || unsafe { hbb_common::libc::geteuid() } != 0
        || std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_none()
    {
        return crate::YD_ERR_STATE;
    }
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
    if configure_namespace().is_err() {
        return crate::YD_ERR_STATE;
    }
    if let Err(e) = drm_probe::capture() {
        log::warn!("DRM unsupported: {e}");
        return crate::YD_ERR_STATE;
    }
    if std::env::var_os("YOODESK_ENGINE_HELPER_DRM_PROBE_ONLY").is_some() {
        return crate::YD_OK;
    }
    // 上游完整service loop原本维护此缓存；嵌入宿主只启动DRM producer，
    // 必须补齐刷新职责，否则连接握手成功后首帧便因cache miss被逐帧鉴权拒绝。
    librustdesk::platform::linux::refresh_active_user_lookup_cache();
    if std::thread::Builder::new()
        .name("drm-active-seat".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_millis(500));
            librustdesk::platform::linux::refresh_active_user_lookup_cache();
        })
        .is_err()
    {
        return crate::YD_ERR_STATE;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("drm-producer".into())
        .spawn(librustdesk::ipc::start_drm)
    {
        log::error!("DRM producer: {e}");
        return crate::YD_ERR_STATE;
    }
    match restrict_drm(uid, gid) {
        Ok(()) => crate::YD_OK,
        Err(e) => {
            log::error!("DRM helper: {e}");
            crate::YD_ERR_STATE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 锁住Portal状态；DRM helper缺失应有界失败，不能暗中等待Portal初始化。
    #[test]
    #[ignore = "run in a dedicated user process: changes native backend environment"]
    fn enabled_drm_missing_helper_never_enters_portal() {
        std::env::set_var(
            "YOODESK_ENGINE_NAMESPACE",
            format!(
                "{}DeskEngine-test{}",
                env!("PRODUCT_ID"),
                std::process::id()
            ),
        );
        std::env::set_var("YOODESK_ENGINE_DRM_ENABLED", "1");
        std::env::set_var("RUSTDESK_FORCED_DISPLAY_SERVER", "wayland");
        std::env::set_var(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-yoodesk-portal",
        );
        configure_namespace().unwrap();
        assert!(!crate::wayland_portal::enabled());
        crate::wayland_portal::start();
        assert_eq!(crate::wayland_portal::flags(), 0);
        let portal = scrap::wayland::pipewire::RDP_SESSION_INFO.lock().unwrap();
        let (done, received) = std::sync::mpsc::channel();
        let request = std::thread::spawn(move || {
            let result = drm_geometry();
            let _ = done.send(result.is_err());
        });
        let result = received.recv_timeout(Duration::from_secs(4));
        drop(portal);
        request.join().unwrap();
        assert_eq!(
            result.unwrap(),
            true,
            "DRM admission must reject missing helper without Portal"
        );
    }
}
