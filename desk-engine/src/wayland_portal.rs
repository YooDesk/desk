//! YooDesk管理RemoteDesktop授权及持久token，RustDesk只消费已建立的公开PipeWire会话缓存。
//! root DRM角色不进入此模块；等待用户授权在专用线程，不占桌面IPC工作线程。
use dbus::{
    arg::{OwnedFd, PropMap, RefArg, Variant},
    blocking::{stdintf::org_freedesktop_dbus::Properties, SyncConnection},
    message::MatchRule,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
const SCREEN: &str = "org.freedesktop.portal.ScreenCast";
static PHASE: AtomicU8 = AtomicU8::new(0);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SESSION: OnceLock<Session> = OnceLock::new();
// libdbus阻塞调用与process共享IO；显式串行，避免保活线程持续抢占读取锁使请求超时。
pub(super) static IO: Mutex<()> = Mutex::new(());
pub(super) struct Session {
    pub conn: Arc<SyncConnection>,
    pub path: dbus::Path<'static>,
    pub clipboard: bool,
    pub devices: u32,
    pub geometry: Vec<[i32; 4]>,
}
/// 公开简单能力位供宿主查询，不传递Portal token或内部对象路径。
pub(super) fn flags() -> u32 {
    if !enabled() {
        return 0;
    }
    let mut flags = 1;
    if PHASE.load(Ordering::Acquire) == 2 {
        flags |= 2;
    }
    if PHASE.load(Ordering::Acquire) == 3 {
        flags |= 64;
    }
    if let Some(session) = SESSION.get() {
        flags |= (session.devices & 3) << 2;
        if session.clipboard {
            flags |= 16;
        }
    }
    flags
}

/// 只在本产品用户态Wayland角色使用Portal，DRM profile始终不发起Portal请求。
pub(super) fn enabled() -> bool {
    std::env::var("RUSTDESK_FORCED_DISPLAY_SERVER").as_deref() == Ok("wayland")
        && std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some()
        && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() != Ok("1")
}
/// 状态查询必须立即返回；失败不会自动重复弹窗，显式重建worker后才再次请求。
pub(super) fn ready() -> bool {
    !enabled() || PHASE.load(Ordering::Acquire) == 2
}
/// 只读Start已授权的几何；枚举不创建额外GStreamer管线或新的PipeWire连接。
pub(super) fn geometry() -> Result<Option<Vec<[i32; 4]>>, String> {
    if !enabled() {
        return Ok(None);
    }
    if !ready() {
        return Err("Wayland authorization is pending or closed".into());
    }
    SESSION
        .get()
        .map(|s| Some(s.geometry.clone()))
        .ok_or_else(|| "Portal session unavailable".into())
}
/// 仅Portal输入复用D-Bus连接；uinput及X11路径不取得该锁。
pub(super) fn input_io() -> Option<std::sync::MutexGuard<'static, ()>> {
    (enabled() && std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() != Ok("1"))
        .then(|| IO.lock().unwrap())
}
/// 已授权的Portal剪贴板保持与画面同一session；不读取上游随机目录中的token。
pub(super) fn clipboard(value: &Value) -> Result<Value, (u16, String)> {
    if !ready() {
        return Err((
            403,
            "Wayland sharing is closed or authorization is pending".into(),
        ));
    }
    let session = SESSION
        .get()
        .ok_or((409, "Wayland sharing authorization is pending".into()))?;
    super::portal_clipboard::endpoint(session, value)
}
/// 在独立线程启动一次授权，保留标准三分钟人机交互期限而不扩大IPC超时。
pub(super) fn start() {
    if !enabled()
        || PHASE
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("wayland-portal".into())
        .spawn(|| {
            if let Err(error) = initialize() {
                log::warn!("Wayland Portal initialization failed: {error}");
                PHASE.store(3, Ordering::Release);
            }
        })
    {
        log::warn!("Wayland Portal thread failed: {error}");
        PHASE.store(3, Ordering::Release);
    }
}
/// 固定用户/产品/服务目录，与每代随机IPC名称解耦；token不进入HTTP响应或日志。
fn token_path() -> Result<PathBuf, String> {
    if unsafe { hbb_common::libc::geteuid() } == 0 {
        return Err("Portal state must belong to graphical user".into());
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("missing graphical HOME")?);
    let service =
        std::env::var("YOODESK_ENGINE_SERVICE_NAME").map_err(|_| "missing service identity")?;
    if !home.is_absolute()
        || service.is_empty()
        || service.len() > 80
        || matches!(service.as_str(), "." | "..")
        || !service
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err("invalid Portal state identity".into());
    }
    let dir = home
        .join(".local/share")
        .join(env!("PRODUCT_ID"))
        .join("web-service")
        .join(service)
        .join("portal");
    fs::create_dir_all(&dir).map_err(|_| "cannot create Portal state directory")?;
    let meta = fs::symlink_metadata(&dir).map_err(|_| "cannot inspect Portal state directory")?;
    if !meta.is_dir() || meta.uid() != unsafe { hbb_common::libc::geteuid() } {
        return Err("untrusted Portal state directory".into());
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot protect Portal state directory")?;
    Ok(dir.join("remote-desktop.json"))
}
/// 只读本用户的私有普通文件，限制长度且拒绝符号链接。
fn load_token() -> Result<String, String> {
    let path = token_path()?;
    load_token_at(&path)
}
/// 从已确定的私有路径读取token，便于以隔离目录测试轮换而不操作真实配置。
fn load_token_at(path: &std::path::Path) -> Result<String, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(hbb_common::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(_) => return Err("cannot read Portal state".into()),
    };
    let m = file.metadata().map_err(|_| "cannot inspect Portal state")?;
    if !m.is_file()
        || m.uid() != unsafe { hbb_common::libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.len() > 16384
    {
        return Err("untrusted Portal state file".into());
    }
    let mut data = String::new();
    file.read_to_string(&mut data)
        .map_err(|_| "cannot read Portal state")?;
    let value: Value = serde_json::from_str(&data).map_err(|_| "invalid Portal state")?;
    if value["v"] != 1 {
        return Err("unsupported Portal state version".into());
    }
    let token = value["restore_token"]
        .as_str()
        .filter(|s| s.len() <= 8192)
        .ok_or("invalid Portal token")?;
    Ok(token.to_owned())
}
/// 原子替换单次恢复token；0600权限，失败不报告已持久保存。
fn save_token(token: &str) -> Result<(), String> {
    let path = token_path()?;
    save_token_at(&path, token)
}
/// 以指定隔离路径执行原子token替换，目录身份由调用方事先校验。
fn save_token_at(path: &std::path::Path, token: &str) -> Result<(), String> {
    if token.is_empty() || token.len() > 8192 {
        return Err("invalid Portal restore token".into());
    }
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid clock")?
            .as_nanos()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| "cannot create Portal state")?;
        file.write_all(json!({"v":1,"restore_token":token}).to_string().as_bytes())
            .map_err(|_| "cannot save Portal state")?;
        file.sync_all().map_err(|_| "cannot flush Portal state")?;
        fs::rename(&temporary, &path).map_err(|_| "cannot publish Portal state")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
/// 构造标准Portal vardict，不记录传入的授权token。
fn options(values: impl IntoIterator<Item = (&'static str, Variant<Box<dyn RefArg>>)>) -> PropMap {
    values.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}
/// 动态值装箱，保持D-Bus选项的原生类型。
fn variant<T: RefArg + 'static>(value: T) -> Variant<Box<dyn RefArg>> {
    Variant(Box::new(value))
}
/// 注册精确响应path后再调用Portal，避免快速恢复响应先于订阅的竞态。
fn request(
    conn: &SyncConnection,
    owner: &str,
    interface: &str,
    method: &str,
    session: Option<&dbus::Path<'static>>,
    mut args: PropMap,
) -> Result<PropMap, String> {
    let token = format!(
        "yd_{}_{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    args.insert("handle_token".into(), variant(token.clone()));
    let path = format!(
        "/org/freedesktop/portal/desktop/request/{}/{}",
        conn.unique_name().trim_start_matches(':').replace('.', "_"),
        token
    );
    let rule =
        MatchRule::new_signal("org.freedesktop.portal.Request", "Response").with_path(path.clone());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let expected = owner.to_owned();
    let signal = conn
        .add_match::<(u32, PropMap), _>(rule, move |reply, _, message| {
            if message
                .sender()
                .map(|s| s.to_string() != expected)
                .unwrap_or(true)
            {
                return true;
            }
            let _ = tx.try_send(reply);
            false
        })
        .map_err(|_| format!("{method}: cannot subscribe to Portal response"))?;
    let portal = scrap::wayland::pipewire::get_portal(conn);
    let sent: Result<(dbus::Path<'static>,), dbus::Error> = match (method, session) {
        ("CreateSession", _) => portal.method_call(interface, method, (args,)),
        ("Start", Some(session)) => {
            portal.method_call(interface, method, (session.clone(), "", args))
        }
        (_, Some(session)) => portal.method_call(interface, method, (session.clone(), args)),
        _ => {
            let _ = conn.remove_match(signal);
            return Err("invalid Portal request stage".into());
        }
    };
    let result = (|| {
        let (actual,) = sent.map_err(|_| format!("{method}: Portal method failed"))?;
        if actual.to_string() != path {
            return Err(format!("{method}: unexpected Portal request path"));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            if let Ok((code, value)) = rx.try_recv() {
                return if code == 0 {
                    Ok(value)
                } else {
                    Err(format!(
                        "{method}: authorization cancelled or denied ({code})"
                    ))
                };
            }
            conn.process(Duration::from_millis(100))
                .map_err(|_| format!("{method}: Portal connection failed"))?;
        }
        Err(format!("{method}: authorization timed out"))
    })();
    let _ = conn.remove_match(signal);
    result
}
/// 将标准Portal stream数组转换成RustDesk公开的描述构造接口，不接管其采集或编码算法。
fn streams(value: &PropMap) -> Result<Vec<scrap::wayland::pipewire::PwStreamInfo>, String> {
    let entries = value
        .get("streams")
        .and_then(|v| v.0.as_iter())
        .ok_or("Portal returned no streams")?;
    let mut result = Vec::new();
    for entry in entries {
        let mut tuple = entry.as_iter().ok_or("invalid Portal stream")?;
        let node = tuple
            .next()
            .and_then(RefArg::as_u64)
            .filter(|v| *v > 0)
            .ok_or("invalid PipeWire node")?;
        let mut fields = tuple
            .next()
            .and_then(RefArg::as_iter)
            .ok_or("invalid Portal stream properties")?;
        let mut size = None;
        let mut position = (0, 0);
        let mut source_type = 1;
        while let Some(key) = fields.next() {
            let value = fields.next().ok_or("invalid Portal property pair")?;
            match key.as_str() {
                Some("size") | Some("position") => {
                    let unwrapped = value
                        .as_iter()
                        .and_then(|mut it| it.next())
                        .unwrap_or(value);
                    let pair = unwrapped
                        .as_iter()
                        .ok_or("invalid Portal coordinate")?
                        .filter_map(RefArg::as_i64)
                        .collect::<Vec<_>>();
                    if pair.len() != 2 {
                        return Err("invalid Portal coordinate pair".into());
                    }
                    if key.as_str() == Some("size") {
                        size = Some((pair[0], pair[1]));
                    } else {
                        position = (pair[0] as i32, pair[1] as i32);
                    }
                }
                Some("source_type") => {
                    source_type = value.as_u64().unwrap_or(1);
                }
                _ => {}
            }
        }
        let (w, h) = size
            .filter(|(w, h)| *w > 0 && *h > 0 && *w <= 16384 && *h <= 16384)
            .ok_or("invalid Portal stream size")?;
        result.push(scrap::wayland::pipewire::PwStreamInfo::from_parts(
            node,
            source_type,
            position,
            (w as usize, h as usize),
        ));
    }
    if result.is_empty() {
        return Err("Portal returned no selected monitor".into());
    }
    Ok(result)
}
/// 建立RemoteDesktop+ScreenCast组合会话；持久选项仅交给RemoteDesktop.SelectDevices。
fn initialize() -> Result<(), String> {
    gstreamer::init().map_err(|_| "GStreamer initialization failed")?;
    let conn = Arc::new(
        SyncConnection::new_session().map_err(|_| "cannot connect to graphical session bus")?,
    );
    let portal = scrap::wayland::pipewire::get_portal(&conn);
    let version: u32 = portal
        .get(DESKTOP, "version")
        .map_err(|_| "RemoteDesktop Portal unavailable")?;
    let (owner,): (String,) = conn
        .with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(2),
        )
        .method_call(
            "org.freedesktop.DBus",
            "GetNameOwner",
            ("org.freedesktop.portal.Desktop",),
        )
        .map_err(|_| "cannot identify Portal service")?;
    let created = request(
        &conn,
        &owner,
        DESKTOP,
        "CreateSession",
        None,
        options([(
            "session_handle_token",
            variant(format!("yoodesk_{}", std::process::id())),
        )]),
    )?;
    let path = dbus::Path::new(
        created
            .get("session_handle")
            .and_then(|v| v.0.as_str())
            .ok_or("Portal returned no session")?
            .to_owned(),
    )
    .map_err(|_| "invalid Portal session")?;
    let expected = owner.clone();
    conn.add_match::<(), _>(
        MatchRule::new_signal("org.freedesktop.portal.Session", "Closed").with_path(path.clone()),
        move |_, _, message| {
            if message.sender().is_some_and(|s| s.to_string() == expected) {
                PHASE.store(3, Ordering::Release);
            }
            true
        },
    )
    .map_err(|_| "cannot monitor Portal session closure")?;
    let mut devices = options([("types", variant(3u32))]);
    if version >= 2 {
        devices.insert("persist_mode".into(), variant(2u32));
        let token = load_token()?;
        if !token.is_empty() {
            devices.insert("restore_token".into(), variant(token));
        }
    }
    request(
        &conn,
        &owner,
        DESKTOP,
        "SelectDevices",
        Some(&path),
        devices,
    )?;
    request(
        &conn,
        &owner,
        SCREEN,
        "SelectSources",
        Some(&path),
        options([("types", variant(1u32)), ("multiple", variant(true))]),
    )?;
    let clipboard_version: Result<u32, dbus::Error> =
        portal.get("org.freedesktop.portal.Clipboard", "version");
    if clipboard_version.is_ok_and(|version| version >= 1) {
        let result: Result<(), dbus::Error> = portal.method_call(
            "org.freedesktop.portal.Clipboard",
            "RequestClipboard",
            (path.clone(), PropMap::new()),
        );
        result.map_err(|_| "Portal clipboard permission request failed")?;
        super::portal_clipboard::listen(conn.clone(), path.clone(), &owner)?;
    }
    let started = request(&conn, &owner, DESKTOP, "Start", Some(&path), HashMap::new())?;
    if version >= 2 {
        if let Some(token) = started.get("restore_token").and_then(|v| v.0.as_str()) {
            save_token(token)?;
        }
    }
    let selected = streams(&started)?;
    let geometry = selected
        .iter()
        .map(|stream| {
            let (w, h) = stream.get_size();
            let (x, y) = stream.get_position();
            [w as i32, h as i32, x, y]
        })
        .collect();
    let (fd,): (OwnedFd,) = portal
        .method_call(SCREEN, "OpenPipeWireRemote", (path.clone(), PropMap::new()))
        .map_err(|_| "cannot open authorized PipeWire stream")?;
    let clipboard = started
        .get("clipboard_enabled")
        .and_then(|v| v.0.as_u64())
        .is_some_and(|v| v != 0);
    let devices = started
        .get("devices")
        .and_then(|v| v.0.as_u64())
        .unwrap_or(0) as u32;
    let info = scrap::wayland::pipewire::RdpSessionInfo {
        conn: conn.clone(),
        streams: selected,
        fd,
        session: path.clone(),
        is_support_restore_token: version >= 2,
        resolution: Arc::new(Mutex::new(None)),
    };
    *scrap::wayland::pipewire::RDP_SESSION_INFO
        .lock()
        .map_err(|_| "PipeWire session cache poisoned")? = Some(info);
    let _ = SESSION.set(Session {
        conn: conn.clone(),
        path,
        clipboard,
        devices,
        geometry,
    });
    PHASE
        .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "Portal session was closed before admission")?;
    // 保活Portal连接并派发后续会话/剪贴板信号，生命周期由用户worker进程持有。
    while PHASE.load(Ordering::Acquire) == 2 {
        {
            let _io = IO.lock().map_err(|_| "Portal IO lock poisoned")?;
            conn.process(Duration::from_millis(10))
                .map_err(|_| "Portal session disconnected")?;
        }
        // 给同步调用线程取得IO锁的机会；不持锁等待，避免libdbus读写饥饿。
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("Portal session closed or permission revoked".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 锁住上游捕获缓存并关闭后续D-Bus连接，真实枚举FFI仍应只读授权元数据。
    #[test]
    #[ignore = "dedicated graphical-user process: replaces Portal state and environment"]
    fn authorized_geometry_does_not_initialize_pipewire() {
        let conn = Arc::new(SyncConnection::new_session().unwrap());
        std::env::set_var("RUSTDESK_FORCED_DISPLAY_SERVER", "wayland");
        std::env::set_var(
            "YOODESK_ENGINE_NAMESPACE",
            "yoodeskDeskEngine-geometry-test",
        );
        std::env::set_var("YOODESK_ENGINE_DRM_ENABLED", "0");
        std::env::remove_var("YOODESK_ENGINE_USER_UINPUT");
        assert!(SESSION
            .set(Session {
                conn,
                path: dbus::Path::new("/test/session").unwrap(),
                clipboard: false,
                devices: 0,
                geometry: vec![[1920, 1080, 0, 0], [1280, 720, 1920, 0]],
            })
            .is_ok());
        PHASE.store(2, Ordering::Release);
        std::env::set_var(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-yoodesk-portal",
        );
        let capture = scrap::wayland::pipewire::RDP_SESSION_INFO.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut displays = [[0; 5]; 4];
            let count = crate::yd_engine_list_displays(displays.as_mut_ptr(), displays.len());
            let _ = tx.send((count, displays));
        });
        let result = rx.recv_timeout(Duration::from_secs(1));
        drop(capture);
        worker.join().unwrap();
        let (count, displays) =
            result.expect("display enumeration must not enter the capture cache");
        assert_eq!(count, 2);
        assert_eq!(displays[0], [0, 1920, 1080, 0, 0]);
        assert_eq!(displays[1], [1, 1280, 720, 1920, 0]);
    }
    /// 用实际D-Bus vardict测试公开流描述边界，避免只验证镜像解析代码。
    #[test]
    fn standard_portal_stream_is_injected_without_a_second_authorization() {
        let props = options([
            ("size", variant((2358i32, 1238i32))),
            ("position", variant((-100i32, 0i32))),
            ("source_type", variant(1u32)),
        ]);
        let response = options([("streams", variant(vec![(42u32, props)]))]);
        let streams = streams(&response).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].path, 42);
        assert_eq!(streams[0].get_size(), (2358, 1238));
        assert_eq!(streams[0].get_position(), (-100, 0));
    }
    /// 非法PipeWire节点和空尺寸必须失败，不能伪造显示器。
    #[test]
    fn malformed_portal_geometry_is_rejected() {
        for node in [0u32, 42] {
            let props = options([("size", variant((0i32, 1238i32)))]);
            assert!(streams(&options([("streams", variant(vec![(node, props)]))])).is_err());
        }
    }
    /// 单次token替换保持0600权限；不触碰真实用户的Portal目录。
    #[test]
    fn restore_token_rotates_atomically_and_refuses_public_state_file() {
        let dir = std::env::temp_dir().join(format!(
            "yoodesk-portal-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("state.json");
        save_token_at(&path, "synthetic-first-token").unwrap();
        assert_eq!(load_token_at(&path).unwrap(), "synthetic-first-token");
        save_token_at(&path, "synthetic-second-token").unwrap();
        assert_eq!(load_token_at(&path).unwrap(), "synthetic-second-token");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_token_at(&path).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
