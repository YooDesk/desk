//! YooDesk desk engine：把 RustDesk 的端侧能力封装成一个小而稳定的 C ABI。
//!
//! 设计约束：
//! - **传输/鉴权/UI 不属于本 crate**：这里只产出桌面采集、H.264 编码、输入注入、
//!   光标、剪贴板与音频，由 `web-service` 通过我们的 HTTP/WS 协议暴露。
//! - **C ABI 而非 Rust ABI**：RustDesk 的依赖图（其 fork 的 webrtc/tungstenite、静态
//!   ffmpeg）与 WebService 的依赖图必须隔离，否则版本解析互相冲突。两边唯一的耦合是
//!   下面这组 `extern "C"` 函数和一条有界回调。
//! - **许可**：RustDesk 主体为 AGPL-3.0；链接本库的发行物整体按 AGPL-3.0 分发。
//!
//! 线程模型：调用方在任意线程调用 `yd_engine_*`；内部把工作投递到自己的运行时，
//! 编码输出通过 `yd_engine_poll` 拉取，避免跨 FFI 回调（Rust 的 panic/借用检查在
//! 回调里最容易出问题）。
//!
//! 端侧采集/编码、光标、剪贴板、音频、文件及 Windows 隐私窗口由会话统一回收。

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, CStr};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base::message_proto::{message, video_frame, Message};
mod endpoint;
#[cfg(target_os = "linux")]
mod file_journal;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(target_os = "linux")]
mod portal_clipboard;
#[cfg(windows)]
mod privacy;
#[cfg(target_os = "linux")]
mod privileged;
mod video_bridge;
#[cfg(target_os = "linux")]
mod wayland_portal;

/// 引擎句柄对调用方不透明。
pub struct Engine;

/// 一帧编码结果，由调用方提供缓冲区。
#[repr(C)]
pub struct Frame {
    /// `data` 的字节长度。
    pub len: usize,
    /// 是否为关键帧。
    pub key: c_int,
    /// 媒体时间戳（毫秒）。
    pub pts: i64,
}

/// 结果码。
pub const YD_OK: c_int = 0;
/// 入参非法。
pub const YD_ERR_ARG: c_int = -1;
/// 引擎不可用或已关闭。
pub const YD_ERR_STATE: c_int = -2;
/// 没有新数据（非错误）。
pub const YD_ERR_EMPTY: c_int = 1;

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();

/// 查询Wayland Portal能力；DRM角色返回零，不进入Portal授权路径。
#[cfg(target_os = "linux")]
#[no_mangle]
pub extern "C" fn yd_engine_wayland_flags() -> u32 {
    wayland_portal::flags()
}

/// 原生 Wayland 库的隔离自探测子入口，在宿主解析业务参数前分派，不启动桌面或 HTTP。
#[cfg(target_os = "linux")]
#[no_mangle]
pub extern "C" fn yd_engine_linux_wayland_probe() -> ! {
    base::platform::linux::wayland_display_probe_child_main()
}

/// 初始化进程级引擎：注册软件编码器，准备运行时。
///
/// 必须在其它函数之前调用一次；重复调用是幂等的。
#[no_mangle]
pub extern "C" fn yd_engine_init() -> c_int {
    if ENGINE.get().is_some() {
        return YD_OK;
    }
    // 只标记本宿主的编码服务；正常SWITCH不应被上游通用service循环计入故障退避。
    std::env::set_var("YOODESK_EMBEDDED_ENGINE", "1");
    // 依赖树的 log 与宿主 tracing 分离；初始化一次，真实采集失败不能静默消失。
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
    // 端侧配置和 Linux 音频 IPC 使用独立命名空间，不能连到宿主的 RustDesk 进程。
    *hbb_common::config::APP_NAME.write().unwrap() = format!("{}DeskEngine", env!("PRODUCT_ID"));
    #[cfg(target_os = "linux")]
    {
        if privileged::configure_namespace().is_err() {
            return YD_ERR_STATE;
        }
        if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some()
            && unsafe { hbb_common::libc::geteuid() } == 0
        {
            return YD_ERR_STATE;
        }
        if let Err(error) = endpoint::recover_file_jobs() {
            log::error!("file transfer recovery: {error}");
            return YD_ERR_STATE;
        }
        if let Err(error) = privileged::start_user_input() {
            log::error!("user uinput startup: {error}");
            return YD_ERR_STATE;
        }
        std::thread::spawn(librustdesk::ipc::start_pa);
    }
    // 没有 GPU 的机器上上游探测返回空列表，H.264 会静默回落到 VP9；显式注册软编码。
    librustdesk::video_service::register_software_encoders();
    // 本产品只消费 H.264（浏览器 WebCodecs `avc1`），必须显式声明偏好；
    // 引擎的默认协商会选 VP9。
    {
        use base::message_proto::{supported_decoding, SupportedDecoding};
        let mut decoding = SupportedDecoding::new();
        decoding.prefer = supported_decoding::PreferCodec::H264.into();
        decoding.ability_h264 = 1;
        scrap::codec::Encoder::update(scrap::codec::EncodingUpdate::Update(1, decoding));
    }
    let engine = Arc::new(Engine);
    let _ = ENGINE.set(engine);
    #[cfg(target_os = "linux")]
    wayland_portal::start();
    YD_OK
}

fn engine() -> Option<&'static Arc<Engine>> {
    ENGINE.get()
}

/// 日志出口：调用方可用 `yd_engine_set_logger` 接管，默认写 `log` crate。
type Logger = unsafe extern "C" fn(c_int, *const c_char);

static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

/// 安装日志回调（线程安全；传 `None` 恢复默认）。
#[no_mangle]
pub extern "C" fn yd_engine_set_logger(logger: Option<Logger>) {
    *LOGGER.lock().unwrap_or_else(|e| e.into_inner()) = logger;
}

/// 引擎内部日志出口。
pub fn log_out(level: i32, text: &str) {
    let installed = *LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(callback) = installed {
        if let Ok(cstring) = std::ffi::CString::new(text) {
            unsafe { callback(level, cstring.as_ptr()) }
        }
        return;
    }
    match level {
        0 => log::error!("{text}"),
        1 => log::warn!("{text}"),
        _ => log::info!("{text}"),
    }
}

/// 订阅显示器，连接独占预测帧缓存；fps/bitrate 来自产品配置。
#[no_mangle]
pub extern "C" fn yd_engine_subscribe(display: i32, conn: i32, fps: u32, bitrate: u32) -> c_int {
    if engine().is_none() {
        return YD_ERR_STATE;
    }
    video_bridge::subscribe(display, conn, fps, bitrate)
}
#[no_mangle]
pub extern "C" fn yd_engine_unsubscribe() -> c_int {
    video_bridge::close_all()
}
#[no_mangle]
pub extern "C" fn yd_engine_unsubscribe_conn(conn: i32) -> c_int {
    video_bridge::unsubscribe(conn)
}
#[no_mangle]
pub extern "C" fn yd_engine_replay_keyframe(conn: i32) -> c_int {
    video_bridge::replay(conn)
}
#[no_mangle]
pub extern "C" fn yd_engine_replay_cursor(conn: i32) -> c_int {
    video_bridge::replay_cursor(conn)
}
#[no_mangle]
pub extern "C" fn yd_engine_poll_cursor(conn: i32, data: *mut u8, cap: usize) -> c_int {
    video_bridge::poll_cursor(conn, data, cap)
}
#[no_mangle]
pub extern "C" fn yd_engine_poll(
    conn: i32,
    data: *mut u8,
    cap: usize,
    frame: *mut Frame,
    timeout: u64,
) -> c_int {
    #[cfg(target_os = "linux")]
    if !wayland_portal::ready() {
        return YD_ERR_STATE;
    }
    video_bridge::poll(conn, data, cap, frame, timeout)
}
#[no_mangle]
pub extern "C" fn yd_engine_video_ack(conn: i32, pts: i64) -> c_int {
    video_bridge::ack(conn, pts)
}
#[no_mangle]
pub extern "C" fn yd_engine_video_refresh(conn: i32) -> c_int {
    video_bridge::request_refresh(conn)
}

/// 把光标消息原样序列化；非光标消息返回 `None`。
/// 把光标消息序列化；`is_shape` 表示这是完整形状（需要缓存以便重放）。
///
/// RustDesk复用已发送光标时只发CursorId；必须还原完整形状，否则网页不会从拖拽/文本光标切回。
fn cursor_message(message: &Message) -> Option<(Vec<u8>, bool)> {
    match &message.union {
        Some(message::Union::CursorId(id)) => {
            let Some(shape) = librustdesk::input_service::cursor_data_message(*id) else {
                log::warn!("native cursor shape is unavailable for reused id");
                return None;
            };
            cursor_message(&shape)
        }
        Some(message::Union::CursorData(cursor)) => {
            // RustDesk 原生 CursorData.colors 是 zstd；我们的网页 profile 明确是 RGBA8。
            // 必须在 FFI 边界解压并校验，不能把压缩字节交给 ImageData。
            if !(1..=512).contains(&cursor.width) || !(1..=512).contains(&cursor.height) {
                log::error!("invalid native cursor dimensions");
                return None;
            }
            let colors = hbb_common::compress::decompress(&cursor.colors);
            if colors.len() != cursor.width as usize * cursor.height as usize * 4 {
                log::error!("native cursor decompression failed");
                return None;
            }
            let mut cursor = cursor.clone();
            cursor.colors = colors.into();
            let mut output = Message::new();
            output.set_cursor_data(cursor);
            protobuf::Message::write_to_bytes(&output)
                .ok()
                .map(|bytes| (bytes, true))
        }
        Some(message::Union::CursorPosition(_)) => protobuf::Message::write_to_bytes(message)
            .ok()
            .map(|bytes| (bytes, false)),
        _ => None,
    }
}

/// 提取 H.264 码流；非视频消息返回 `None`（调用方继续轮询）。
fn h264_from(message: &Message) -> Option<(Vec<u8>, bool, i64)> {
    let Some(message::Union::VideoFrame(video)) = &message.union else {
        return None;
    };
    let Some(video_frame::Union::H264s(frames)) = &video.union else {
        return None;
    };
    let mut data = Vec::new();
    let mut key = false;
    let mut pts = 0i64;
    for frame in &frames.frames {
        data.extend_from_slice(&frame.data);
        key |= frame.key;
        pts = pts.max(frame.pts);
    }
    Some((data, key, pts))
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    #[test]
    fn http_key_adapter_matches_frozen_native_enum() {
        use protobuf::Enum;
        let frozen = include_str!("../../../protocol/rustdesk/message-v1.proto");
        let control = frozen
            .split("enum ControlKey {")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        for code in 1..=33 {
            let key = http_control_key(code).unwrap();
            let name = format!("{key:?}");
            let prefix = format!("{name} = ");
            let expected: i32 = control
                .lines()
                .find_map(|line| line.trim().strip_prefix(&prefix))
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(key.value(), expected, "HTTP key {code} maps to {name}");
        }
        assert!(http_control_key(0).is_none());
        assert!(http_control_key(34).is_none());
        let mut event = base::message_proto::KeyEvent::new();
        add_modifiers(&mut event, 1 | 2 | 16);
        assert_eq!(event.modifiers.len(), 3);
        assert_eq!(
            event.modifiers[0].enum_value().unwrap(),
            base::message_proto::ControlKey::Control
        );
        assert_eq!(
            event.modifiers[1].enum_value().unwrap(),
            base::message_proto::ControlKey::Shift
        );
        assert_eq!(
            event.modifiers[2].enum_value().unwrap(),
            base::message_proto::ControlKey::RControl
        );
    }
    #[test]
    fn compressed_native_cursor_is_forwarded_as_exact_rgba() {
        use protobuf::Message as _;
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let mut cursor = base::message_proto::CursorData::new();
        cursor.width = 2;
        cursor.height = 1;
        cursor.colors = hbb_common::compress::compress(&rgba).into();
        let mut message = Message::new();
        message.set_cursor_data(cursor.clone());
        let (bytes, shape) = cursor_message(&message).unwrap();
        assert!(shape);
        let forwarded = Message::parse_from_bytes(&bytes).unwrap();
        assert_eq!(forwarded.cursor_data().colors.as_ref(), rgba.as_slice());
        cursor.colors = vec![1, 2, 3].into();
        message.set_cursor_data(cursor);
        assert!(cursor_message(&message).is_none());
    }
}

/// 枚举显示器，写入调用方的缓冲区。
///
/// 每项为 `[index:i32, width:i32, height:i32, x:i32, y:i32]`；返回写入的显示器数量，
/// 出错返回负值。`capacity` 为 `out` 能容纳的**显示器数量**。
#[no_mangle]
pub extern "C" fn yd_engine_list_displays(out: *mut [i32; 5], capacity: usize) -> c_int {
    if out.is_null() {
        return YD_ERR_ARG;
    }
    #[cfg(target_os = "linux")]
    if !wayland_portal::ready() {
        return YD_ERR_STATE;
    }
    #[cfg(target_os = "linux")]
    match privileged::capture_geometry() {
        Ok(Some(displays)) => {
            let count = displays.len().min(capacity);
            for (index, geometry) in displays.iter().take(count).enumerate() {
                unsafe {
                    *out.add(index) = [
                        index as i32,
                        geometry[0],
                        geometry[1],
                        geometry[2],
                        geometry[3],
                    ];
                }
            }
            return count as c_int;
        }
        Ok(None) => {}
        Err(error) => {
            log::error!("capture display enumeration: {error}");
            return YD_ERR_STATE;
        }
    }
    let displays = match scrap::Display::all() {
        Ok(displays) => displays,
        Err(err) => {
            log::error!("list displays failed: {err}");
            return YD_ERR_STATE;
        }
    };
    #[cfg(target_os = "linux")]
    if let Err(error) = privileged::refresh_user_input(&displays) {
        log::error!("uinput display initialization: {error}");
        return YD_ERR_STATE;
    }
    let count = displays.len().min(capacity);
    for (index, display) in displays.iter().take(count).enumerate() {
        let (x, y) = display.origin();
        unsafe {
            *out.add(index) = [
                index as i32,
                display.width() as i32,
                display.height() as i32,
                x,
                y,
            ];
        }
    }
    count as c_int
}

static INJECTED: AtomicI32 = AtomicI32::new(0);

/// 注入鼠标事件。
///
/// `mask` 语义与 `hbb.MouseEvent` 一致；坐标为虚拟桌面物理像素。
#[no_mangle]
pub extern "C" fn yd_engine_mouse(mask: i32, x: i32, y: i32) -> c_int {
    #[cfg(target_os = "linux")]
    if !wayland_portal::ready() && mask & 7 != 2 {
        return YD_ERR_STATE;
    }
    if INJECTED.load(Ordering::SeqCst) == 0 {
        return YD_ERR_STATE;
    }
    let mut event = base::message_proto::MouseEvent::new();
    event.mask = mask;
    event.x = x;
    event.y = y;
    #[cfg(target_os = "linux")]
    let _io = wayland_portal::input_io();
    librustdesk::input_service::handle_mouse(&event, 0, String::new(), 0, true, false);
    YD_OK
}

/// 注入键盘控制键，编号取自冻结快照的 `hbb.ControlKey`。
#[no_mangle]
pub extern "C" fn yd_engine_key(code: i32, down: c_int) -> c_int {
    #[cfg(target_os = "linux")]
    if down != 0 && !wayland_portal::ready() {
        return YD_ERR_STATE;
    }
    if INJECTED.load(Ordering::SeqCst) == 0 {
        return YD_ERR_STATE;
    }
    let Some(key) = http_control_key(code) else {
        return YD_ERR_ARG;
    };
    let mut event = base::message_proto::KeyEvent::new();
    event.union = Some(base::message_proto::key_event::Union::ControlKey(
        key.into(),
    ));
    event.down = down != 0;
    inject_key(&event);
    YD_OK
}

// HTTP 旧编号固定 1..33；原生枚举按名称转换，避免 Home/Shift 落到 F5/PageDown。
fn http_control_key(code: i32) -> Option<base::message_proto::ControlKey> {
    use base::message_proto::ControlKey as K;
    Some(match code {
        1 => K::Alt,
        2 => K::Backspace,
        3 => K::CapsLock,
        4 => K::Control,
        5 => K::Delete,
        6 => K::DownArrow,
        7 => K::End,
        8 => K::Escape,
        9 => K::F1,
        10 => K::F10,
        11 => K::F11,
        12 => K::F12,
        13 => K::F2,
        14 => K::F3,
        15 => K::F4,
        16 => K::Home,
        17 => K::LeftArrow,
        18 => K::Meta,
        19 => K::NumLock,
        20 => K::PageDown,
        21 => K::PageUp,
        22 => K::Return,
        23 => K::RightArrow,
        24 => K::Scroll,
        25 => K::Shift,
        26 => K::Space,
        27 => K::Tab,
        28 => K::UpArrow,
        29 => K::Insert,
        30 => K::RControl,
        31 => K::RShift,
        32 => K::RAlt,
        33 => K::RWin,
        _ => return None,
    })
}

fn add_modifiers(event: &mut base::message_proto::KeyEvent, mask: u32) {
    use base::message_proto::ControlKey;
    for (bit, key) in [
        (1, ControlKey::Control),
        (2, ControlKey::Shift),
        (4, ControlKey::Alt),
        (8, ControlKey::Meta),
        (16, ControlKey::RControl),
        (32, ControlKey::RShift),
        (64, ControlKey::RAlt),
        (128, ControlKey::RWin),
    ] {
        if mask & bit != 0 {
            event.modifiers.push(key.into());
        }
    }
}

/// 保留当前修饰键；当前控制键自身由 down/up 处理，不能临时按下后又自动释放。
#[no_mangle]
pub extern "C" fn yd_engine_key_with_modifiers(code: i32, down: c_int, modifiers: u32) -> c_int {
    #[cfg(target_os = "linux")]
    if down != 0 && !wayland_portal::ready() {
        return YD_ERR_STATE;
    }
    if INJECTED.load(Ordering::SeqCst) == 0 {
        return YD_ERR_STATE;
    }
    if modifiers > 255 {
        return YD_ERR_ARG;
    }
    let Some(key) = http_control_key(code) else {
        return YD_ERR_ARG;
    };
    let mut event = base::message_proto::KeyEvent::new();
    event.union = Some(base::message_proto::key_event::Union::ControlKey(
        key.into(),
    ));
    event.down = down != 0;
    let own = match code {
        4 => 1,
        25 => 2,
        1 => 4,
        18 => 8,
        30 => 16,
        31 => 32,
        32 => 64,
        33 => 128,
        _ => 0,
    };
    add_modifiers(&mut event, modifiers & !own);
    inject_key(&event);
    YD_OK
}

/// 按当前布局发送 ASCII 字符键；Ctrl+C/V 必须走此入口，不能当成文本注入。
#[no_mangle]
pub extern "C" fn yd_engine_layout(code: u32, down: c_int, modifiers: u32) -> c_int {
    #[cfg(target_os = "linux")]
    if down != 0 && !wayland_portal::ready() {
        return YD_ERR_STATE;
    }
    if INJECTED.load(Ordering::SeqCst) == 0 {
        return YD_ERR_STATE;
    }
    if !(32..=126).contains(&code) || modifiers > 255 {
        return YD_ERR_ARG;
    }
    let mut event = base::message_proto::KeyEvent::new();
    event.set_chr(code);
    event.down = down != 0;
    add_modifiers(&mut event, modifiers);
    #[cfg(windows)]
    {
        use winapi::um::winuser::{
            GetForegroundWindow, GetKeyboardLayout, GetWindowThreadProcessId, VkKeyScanExW,
        };
        // Legacy Chr在Windows非组合键分支仍使用Unicode，绕过IME；Translate的VK路径才是真实按键。
        let mapped = unsafe {
            let thread = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
            VkKeyScanExW(code as u16, GetKeyboardLayout(thread))
        };
        if mapped == -1 {
            return YD_ERR_ARG;
        }
        event.mode = base::message_proto::KeyboardMode::Translate.into();
        event.set_chr(((mapped as u32) & 0xff) << 16);
        // 浏览器成字符事件可能没有单独的Shift按下（例如自动化输入大写/符号）。
        let implicit_shift = down != 0 && mapped & 0x100 != 0 && modifiers & (2 | 32) == 0;
        if implicit_shift {
            yd_engine_key_with_modifiers(25, 1, modifiers | 2);
        }
        inject_key(&event);
        if implicit_shift {
            yd_engine_key_with_modifiers(25, 0, modifiers);
        }
        YD_OK
    }
    #[cfg(not(windows))]
    {
        inject_key(&event);
        YD_OK
    }
}

/// 注入文本（UTF-8）。
#[no_mangle]
pub extern "C" fn yd_engine_text(text: *const c_char) -> c_int {
    #[cfg(target_os = "linux")]
    if !wayland_portal::ready() {
        return YD_ERR_STATE;
    }
    if text.is_null() {
        return YD_ERR_ARG;
    }
    if INJECTED.load(Ordering::SeqCst) == 0 {
        return YD_ERR_STATE;
    }
    let Ok(text) = unsafe { CStr::from_ptr(text) }.to_str() else {
        return YD_ERR_ARG;
    };
    let mut event = base::message_proto::KeyEvent::new();
    // Seq 是完整 UTF-8 文本；Unicode 分支不看 down，逐个 down/up 会重复字符。
    event.set_seq(text.to_owned());
    event.down = true;
    inject_key(&event);
    YD_OK
}

/// Portal输入与信号派发共用同一连接，键盘调用必须经过相同IO协调。
fn inject_key(event: &base::message_proto::KeyEvent) {
    #[cfg(target_os = "linux")]
    let _io = wayland_portal::input_io();
    librustdesk::input_service::handle_key(event);
}

/// 标记输入可用（由 WebService 在取得平台授权后调用）。
#[no_mangle]
pub extern "C" fn yd_engine_set_input_enabled(enabled: c_int) {
    INJECTED.store(enabled, Ordering::SeqCst);
}

/// 引擎版本字符串，用于能力响应与日志。
#[no_mangle]
pub extern "C" fn yd_engine_version() -> *const c_char {
    static VERSION: &[u8] = b"rustdesk-engine/1.5.1\0";
    VERSION.as_ptr() as *const c_char
}

/// Rust 包装：供同 workspace 的 example 与测试直接调用。
pub mod rust_api {
    use super::*;

    /// 初始化引擎。
    pub fn init() -> i32 {
        yd_engine_init()
    }

    /// 枚举显示器；返回 (宽, 高, x, y) 列表。
    pub fn displays() -> Vec<(u32, u32, i32, i32)> {
        let mut raw = [[0i32; 5]; 8];
        let count = yd_engine_list_displays(raw.as_mut_ptr(), raw.len());
        if count <= 0 {
            return Vec::new();
        }
        raw[..count as usize]
            .iter()
            .map(|entry| (entry[1] as u32, entry[2] as u32, entry[3], entry[4]))
            .collect()
    }

    /// 订阅显示器。
    pub fn subscribe(display: i32, conn: i32) -> i32 {
        yd_engine_subscribe(display, conn, 20, 4_000_000)
    }

    /// 轮询一帧；成功返回 (数据, 是否关键帧, pts)。
    pub fn poll(timeout_ms: u64) -> Option<(Vec<u8>, bool, i64)> {
        let mut buffer = vec![0u8; 8 << 20];
        let mut frame = Frame {
            len: 0,
            key: 0,
            pts: 0,
        };
        let rc = yd_engine_poll(1, buffer.as_mut_ptr(), buffer.len(), &mut frame, timeout_ms);
        if rc != YD_OK {
            return None;
        }
        yd_engine_video_ack(1, frame.pts);
        buffer.truncate(frame.len);
        Some((buffer, frame.key != 0, frame.pts))
    }

    /// 启用输入注入。
    pub fn set_input_enabled(enabled: bool) {
        yd_engine_set_input_enabled(if enabled { 1 } else { 0 })
    }

    /// 注入鼠标事件。
    pub fn mouse(mask: i32, x: i32, y: i32) -> i32 {
        yd_engine_mouse(mask, x, y)
    }

    /// 取消订阅。
    pub fn unsubscribe() -> i32 {
        yd_engine_unsubscribe()
    }

    /// 拉取一条光标消息。
    pub fn poll_cursor() -> Option<Vec<u8>> {
        let mut buffer = vec![0u8; 64 * 1024];
        let rc = yd_engine_poll_cursor(1, buffer.as_mut_ptr(), buffer.len());
        if rc > 0 {
            buffer.truncate(rc as usize);
            Some(buffer)
        } else {
            None
        }
    }

    /// 引擎版本。
    pub fn version() -> &'static str {
        "rustdesk-engine/1.5.1"
    }
}
