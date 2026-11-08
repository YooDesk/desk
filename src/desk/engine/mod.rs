//! RustDesk 引擎后端（feature `desk-engine`）。
//!
//! 通过 `desk-engine` 子 crate 的 C ABI 复用 RustDesk 端侧能力；该子 crate 有自己的
//! workspace 与锁文件，因此 RustDesk 的依赖图不会污染本 crate。
//!
//! 运行时要求：`yoodesk_desk_engine` 静态库必须已经链接进二进制（见
//! `build.rs` 的 `desk-engine` 分支与 `scripts/build_engine.py`）。

#![allow(unsafe_code)]

use std::collections::HashSet;
use std::time::Duration;

use crate::desk::input::{KeyInput, MouseButton, MouseInput};
use crate::desk::{
    Backend, CaptureMode, Capturer, DeskCapabilities, DisplayGeometry, InputSink, RgbaFrame,
};
use crate::json::{codes, ApiError};
use serde_json::{json, Value};

pub mod rpc;
pub(crate) mod worker;

extern "C" {
    #[cfg(target_os = "linux")]
    fn yd_engine_wayland_flags() -> u32;
    fn yd_engine_init() -> i32;
    fn yd_engine_list_displays(out: *mut [i32; 5], capacity: usize) -> i32;
    fn yd_engine_subscribe(display: i32, conn: i32, fps: u32, bitrate: u32) -> i32;
    fn yd_engine_unsubscribe() -> i32;
    fn yd_engine_replay_keyframe(conn: i32) -> i32;
    fn yd_engine_unsubscribe_conn(conn: i32) -> i32;
    fn yd_engine_poll_cursor(conn: i32, data: *mut u8, capacity: usize) -> i32;
    fn yd_engine_replay_cursor(conn: i32) -> i32;
    fn yd_engine_video_ack(conn: i32, pts: i64) -> i32;
    fn yd_engine_video_refresh(conn: i32) -> i32;
    fn yd_engine_poll(
        conn: i32,
        data: *mut u8,
        capacity: usize,
        frame: *mut RawFrame,
        timeout_ms: u64,
    ) -> i32;
    fn yd_engine_set_input_enabled(enabled: i32);
    fn yd_engine_mouse(mask: i32, x: i32, y: i32) -> i32;
    fn yd_engine_key_with_modifiers(code: i32, down: i32, modifiers: u32) -> i32;
    fn yd_engine_layout(code: u32, down: i32, modifiers: u32) -> i32;
    fn yd_engine_text(text: *const std::ffi::c_char) -> i32;
    fn yd_engine_version() -> *const std::ffi::c_char;
    fn yd_engine_endpoint(conn: i32, input: *const u8, len: usize, out: *mut u8, cap: usize)
        -> i32;
    fn yd_engine_poll_audio(conn: i32, replay: i32, out: *mut u8, cap: usize) -> i32;
}

/// 与引擎 `Frame` 的 C 布局一致。
#[repr(C)]
struct RawFrame {
    len: usize,
    key: i32,
    pts: i64,
}

/// 引擎成功码。
const YD_OK: i32 = 0;
/// 引擎“暂无数据”。
const YD_ERR_EMPTY: i32 = 1;

/// 独立Linux root helper初始化；普通桌面worker不调用此入口。
#[cfg(target_os = "linux")]
pub fn initialize_privileged(uid: u32, gid: u32) -> Result<(), ApiError> {
    extern "C" {
        fn yd_engine_linux_privileged_init(uid: u32, gid: u32) -> i32;
    }
    check(unsafe { yd_engine_linux_privileged_init(uid, gid) })
}

/// Linux 内部 Wayland 自探测短子进程；由 main 在配置解析前按唯一参数分派。
#[cfg(target_os = "linux")]
pub fn run_wayland_display_probe() -> i32 {
    extern "C" {
        fn yd_engine_linux_wayland_probe() -> !;
    }
    unsafe { yd_engine_linux_wayland_probe() }
}

/// Linux用户态uinput专用子进程，不初始化桌面Portal或HTTP。
#[cfg(target_os = "linux")]
pub fn run_user_input_service() -> i32 {
    extern "C" {
        fn yd_engine_linux_user_input_service() -> i32;
    }
    unsafe { yd_engine_linux_user_input_service() }
}

/// 引擎后端。
pub struct EngineBackend;

impl EngineBackend {
    /// 初始化引擎；幂等。
    pub fn new() -> Self {
        if rpc::enabled() {
            // 服务父进程仅装配代理，不能为探测能力初始化桌面/音频服务。
            return Self;
        }
        let rc = unsafe { yd_engine_init() };
        if rc != YD_OK {
            tracing::error!(rc, "RustDesk 引擎初始化失败");
        } else {
            tracing::info!(version = %Self::version(), "RustDesk 引擎已初始化");
        }
        Self
    }

    /// 引擎版本字符串。
    pub fn version() -> String {
        unsafe {
            let pointer = yd_engine_version();
            if pointer.is_null() {
                return "unknown".to_owned();
            }
            std::ffi::CStr::from_ptr(pointer)
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Default for EngineBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// 引擎采集器。
///
/// 引擎内部完成采集与编码，`next_frame` 因此不返回像素；实际码流由
/// [`EngineBackend::poll`] 拉取。保留 `Capturer` 是为了让会话层统一处理几何与模式。
pub struct EngineCapturer {
    geometry: DisplayGeometry,
}

impl Capturer for EngineCapturer {
    fn geometry(&self) -> &DisplayGeometry {
        &self.geometry
    }

    fn next_frame(&mut self, _timeout: Duration) -> std::io::Result<RgbaFrame> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "engine encodes internally; use EngineBackend::poll",
        ))
    }

    fn mode(&self) -> CaptureMode {
        CaptureMode::Native
    }
}

// 用户 Wayland 缺设备授权时仍保留画面，不能宣称已经具备输入或静默丢弃事件。
fn local_input_available() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some()
        && std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland")
    {
        let flags = unsafe { yd_engine_wayland_flags() };
        if flags & 1 != 0 && flags & 2 == 0 {
            return false;
        }
        return std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() == Ok("1")
            || flags & 12 == 12;
    }
    true
}
impl Backend for EngineBackend {
    fn name(&self) -> &'static str {
        "rustdesk-engine"
    }

    fn as_any_engine(&self) -> Option<&EngineBackend> {
        Some(self)
    }

    fn capabilities(&self) -> DeskCapabilities {
        if let Some(result) = rpc::call("capabilities", json!({})) {
            return result
                .ok()
                .and_then(|r| serde_json::from_value(r.value).ok())
                .unwrap_or_else(DeskCapabilities::none);
        }
        let displays = self.displays();
        let count = displays.as_ref().map(|list| list.len()).unwrap_or(0);
        #[cfg(target_os = "linux")]
        let clipboard_available = {
            let flags = unsafe { yd_engine_wayland_flags() };
            flags & 1 == 0 || flags & 18 == 18
        };
        #[cfg(not(target_os = "linux"))]
        let clipboard_available = true;
        DeskCapabilities {
            capture: true,
            displays: displays.is_ok(),
            h264: true,
            pointer: local_input_available(),
            keyboard: local_input_available(),
            cursor: true,
            clipboard_read: clipboard_available,
            clipboard_write: clipboard_available,
            audio: true,
            chat: true,
            file_transfer: true,
            hdr: self
                .endpoint(0, &json!({"action":"capabilities"}))
                .map(|v| v["hdr"] == true)
                .unwrap_or(false),
            privacy_screen: self
                .endpoint(0, &json!({"action":"capabilities"}))
                .map(|v| v["privacy_screen"] == true)
                .unwrap_or(false),
            multi_display: count > 1,
        }
    }

    fn displays(&self) -> Result<Vec<DisplayGeometry>, ApiError> {
        if let Some(result) = rpc::call("displays", json!({})) {
            return result.and_then(|r| {
                serde_json::from_value(r.value)
                    .map_err(|_| ApiError::new(503, codes::DESK_ENGINE, "invalid worker displays"))
            });
        }
        #[cfg(target_os = "linux")]
        {
            let flags = unsafe { yd_engine_wayland_flags() };
            if flags & 1 != 0 && flags & 2 == 0 {
                return Err(ApiError::new(
                    if flags & 64 != 0 { 403 } else { 409 },
                    codes::DESK_ENGINE,
                    if flags & 64 != 0 {
                        "Wayland screen sharing authorization failed"
                    } else {
                        "Waiting for Wayland screen sharing authorization"
                    },
                ));
            }
        }
        let mut raw = [[0i32; 5]; 8];
        let count = unsafe { yd_engine_list_displays(raw.as_mut_ptr(), raw.len()) };
        if count < 0 {
            return Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "display enumeration failed",
            ));
        }
        let mut result = Vec::with_capacity(count as usize);
        for entry in raw.iter().take(count as usize) {
            result.push(DisplayGeometry {
                index: entry[0] as u32,
                width: entry[1].max(0) as u32,
                height: entry[2].max(0) as u32,
                x: entry[3],
                y: entry[4],
                name: format!("display-{}", entry[0]),
                primary: entry[0] == 0,
            });
        }
        if result.is_empty() {
            return Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "no attached display",
            ));
        }
        Ok(result)
    }

    fn create_capturer(&self, display: u32) -> Result<Box<dyn Capturer>, ApiError> {
        let geometry = self
            .displays()?
            .into_iter()
            .find(|candidate| candidate.index == display)
            .ok_or_else(|| ApiError::new(404, codes::NOT_FOUND, "display not found"))?;
        Ok(Box::new(EngineCapturer { geometry }))
    }

    fn create_input(&self) -> Result<Box<dyn InputSink>, ApiError> {
        if rpc::enabled() {
            return rpc::input();
        }
        if !local_input_available() {
            return Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "Wayland input unavailable: uinput access was not granted",
            ));
        }
        Ok(Box::new(EngineInput::new()))
    }

    fn supports_input(&self) -> bool {
        #[cfg(target_os = "linux")]
        if rpc::enabled() {
            let capabilities = self.capabilities();
            return capabilities.pointer || capabilities.keyboard;
        }
        local_input_available()
    }

    fn supports_h264(&self) -> bool {
        true
    }

    fn capture_mode(&self) -> CaptureMode {
        CaptureMode::Native
    }

    fn clipboard(&self) -> Option<Box<dyn crate::desk::Clipboard>> {
        if rpc::enabled() {
            return Some(rpc::clipboard());
        }
        Some(Box::new(EngineClipboard))
    }
}

struct EngineClipboard;
impl crate::desk::Clipboard for EngineClipboard {
    fn read(&mut self) -> Result<Option<crate::desk::ClipboardContent>, ApiError> {
        let value = EngineBackend.endpoint(0, &json!({"action":"clipboard.read"}))?;
        if value.is_null() {
            return Ok(None);
        }
        crate::http::clipboard_from_json(&value).map(Some)
    }
    fn write(&mut self, content: &crate::desk::ClipboardContent) -> Result<(), ApiError> {
        EngineBackend.endpoint(
            0,
            &json!({"action":"clipboard.write", "content":crate::http::clipboard_to_json(content)}),
        )?;
        Ok(())
    }
}

impl EngineBackend {
    /// C ABI 仅传字节，不跨运行时传递 Rust 对象。输出大小受剪贴板图像上限约束。
    pub fn endpoint(&self, conn: i32, value: &Value) -> Result<Value, ApiError> {
        if rpc::enabled()
            && value["action"]
                .as_str()
                .is_some_and(|a| a.starts_with("files."))
        {
            return rpc::files(conn, value);
        }
        if let Some(result) = rpc::call("endpoint", json!({"conn":conn,"value":value})) {
            return result.map(|r| r.value);
        }
        let input = serde_json::to_vec(value)
            .map_err(|e| ApiError::new(400, codes::INVALID_REQUEST, e.to_string()))?;
        let mut output = vec![0u8; 24 << 20];
        let rc = unsafe {
            yd_engine_endpoint(
                conn,
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        if rc <= 0 || rc as usize > output.len() {
            return Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "engine endpoint failed",
            ));
        }
        let envelope: Value = serde_json::from_slice(&output[..rc as usize])
            .map_err(|e| ApiError::new(503, codes::DESK_ENGINE, e.to_string()))?;
        decode_endpoint(envelope)
    }

    /// 拉取会话的 Opus 包或重放格式，空队列返回 None。
    pub fn poll_audio(&self, conn: i32, replay: bool) -> Result<Option<Vec<u8>>, ApiError> {
        if let Some(result) = rpc::call("poll_audio", json!({"conn":conn,"replay":replay})) {
            return result.map(|r| {
                if r.value["present"] == true {
                    Some(r.bytes)
                } else {
                    None
                }
            });
        }
        let mut output = vec![0u8; 65536];
        let rc = unsafe {
            yd_engine_poll_audio(conn, i32::from(replay), output.as_mut_ptr(), output.len())
        };
        if rc == 0 {
            return Ok(None);
        }
        if rc < 0 || rc as usize > output.len() {
            return Err(ApiError::new(503, codes::DESK_ENGINE, "audio poll failed"));
        }
        output.truncate(rc as usize);
        Ok(Some(output))
    }
}

fn decode_endpoint(envelope: Value) -> Result<Value, ApiError> {
    if let Some(error) = envelope["error"].as_str() {
        let status = envelope["status"].as_u64().unwrap_or(503) as u16;
        let code = match status {
            400 => codes::INVALID_REQUEST,
            404 => codes::NOT_FOUND,
            409 => codes::STATE_CONFLICT,
            429 => codes::CAPACITY_EXCEEDED,
            413 => codes::INVALID_REQUEST,
            501 => codes::UNSUPPORTED_FEATURE,
            _ => codes::DESK_ENGINE,
        };
        return Err(ApiError::new(status, code, error));
    }
    Ok(envelope["data"].clone())
}

impl EngineBackend {
    /// 订阅显示器并开始编码；`conn` 必须唯一。
    pub fn subscribe(
        &self,
        display_index: u32,
        conn: i32,
        settings: crate::desk::encode::EncoderSettings,
    ) -> Result<(), ApiError> {
        if let Some(result) = rpc::call(
            "subscribe",
            json!({"display":display_index,"conn":conn,"fps":settings.fps,"bitrate":settings.bitrate_bps}),
        ) {
            return result.map(|_| ());
        }
        let rc = unsafe {
            yd_engine_subscribe(
                display_index as i32,
                conn,
                settings.fps,
                settings.bitrate_bps,
            )
        };
        tracing::info!(
            "引擎订阅结果 display={} conn={} rc={}",
            display_index,
            conn,
            rc
        );
        if rc == YD_OK {
            unsafe { yd_engine_set_input_enabled(1) };
            Ok(())
        } else {
            Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "engine subscribe failed",
            ))
        }
    }

    /// 取消指定 conn 的订阅；不是当前订阅者时无操作。
    pub fn unsubscribe_conn(&self, conn: i32) -> Result<(), ApiError> {
        if rpc::enabled() {
            rpc::close_files(conn);
        }
        if let Some(result) = rpc::call("unsubscribe_conn", json!({"conn":conn})) {
            return result.map(|_| ());
        }
        let rc = unsafe { yd_engine_unsubscribe_conn(conn) };
        if rc == YD_OK {
            Ok(())
        } else {
            Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "engine unsubscribe failed",
            ))
        }
    }

    /// 取消订阅。
    pub fn unsubscribe(&self) -> Result<(), ApiError> {
        if let Some(result) = rpc::call("unsubscribe", json!({})) {
            return result.map(|_| ());
        }
        let rc = unsafe { yd_engine_unsubscribe() };
        if rc == YD_OK {
            Ok(())
        } else {
            Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "engine unsubscribe failed",
            ))
        }
    }

    /// 请求引擎重放最近的关键帧（新观众需要 SPS/PPS + IDR 才能解码）。
    pub fn replay_keyframe(&self, conn: i32) -> Result<(), ApiError> {
        if let Some(result) = rpc::call("replay_keyframe", json!({"conn":conn})) {
            return result.map(|_| ());
        }
        let rc = unsafe { yd_engine_replay_keyframe(conn) };
        if rc == YD_OK {
            Ok(())
        } else {
            Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "engine keyframe replay failed",
            ))
        }
    }

    /// 请求引擎重放最近的光标形状（新观众需要它才能绘制）。
    pub fn replay_cursor(&self, conn: i32) -> Result<(), ApiError> {
        if let Some(result) = rpc::call("replay_cursor", json!({"conn":conn})) {
            return result.map(|_| ());
        }
        let rc = unsafe { yd_engine_replay_cursor(conn) };
        if rc == YD_OK {
            Ok(())
        } else {
            Err(ApiError::new(
                503,
                codes::DESK_ENGINE,
                "engine cursor replay failed",
            ))
        }
    }

    /// 拉取下一条光标消息；没有则返回 `Ok(None)`。
    ///
    /// 返回 `(bytes, is_shape)`：形状与位置在会话层分别计数，
    /// 形状只在变化时产生，位置则是每次移动一条。
    pub fn poll_cursor(&self, conn: i32) -> Result<Option<(Vec<u8>, bool)>, ApiError> {
        if let Some(result) = rpc::call("poll_cursor", json!({"conn":conn})) {
            return result.map(|r| {
                if r.value["present"] == true {
                    Some((r.bytes, r.value["shape"] == true))
                } else {
                    None
                }
            });
        }
        let mut buffer = vec![0u8; 64 * 1024];
        let rc = unsafe { yd_engine_poll_cursor(conn, buffer.as_mut_ptr(), buffer.len()) };
        if rc == YD_ERR_EMPTY {
            return Ok(None);
        }
        if rc > 0 {
            // 首字节是 protobuf tag：字段 12 为 CursorData，13 为 CursorPosition。
            let is_shape = buffer.first().map(|tag| *tag == 0x62).unwrap_or(false);
            buffer.truncate(rc as usize);
            return Ok(Some((buffer, is_shape)));
        }
        Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "engine cursor poll failed",
        ))
    }

    /// 拉取一帧 H.264；超时或暂无数据返回 `Ok(None)`。
    pub fn poll(
        &self,
        conn: i32,
        timeout: Duration,
    ) -> Result<Option<(Vec<u8>, bool, i64)>, ApiError> {
        if let Some(result) = rpc::call("poll", json!({"conn":conn})) {
            return result.map(|r| {
                if r.value["present"] == true {
                    Some((
                        r.bytes,
                        r.value["key"] == true,
                        r.value["pts"].as_i64().unwrap_or(0),
                    ))
                } else {
                    None
                }
            });
        }
        let mut buffer = vec![0u8; 8 << 20];
        let mut frame = RawFrame {
            len: 0,
            key: 0,
            pts: 0,
        };
        let rc = unsafe {
            yd_engine_poll(
                conn,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut frame,
                timeout.as_millis() as u64,
            )
        };
        match rc {
            YD_OK => {
                buffer.truncate(frame.len);
                Ok(Some((buffer, frame.key != 0, frame.pts)))
            }
            YD_ERR_EMPTY => Ok(None),
            other => {
                tracing::warn!(rc = other, "引擎轮询失败");
                Err(ApiError::new(503, codes::DESK_ENGINE, "engine poll failed"))
            }
        }
    }

    /// 网页真实解码完成后释放对应帧，不能用发送成功代替。
    pub fn ack_video(&self, conn: i32, pts: i64) -> Result<(), ApiError> {
        if let Some(result) = rpc::call("ack_video", json!({"conn":conn,"pts":pts})) {
            return result.map(|_| ());
        }
        check(unsafe { yd_engine_video_ack(conn, pts) })
    }

    /// 丢失预测帧后重建编码器，所有观众从新 IDR 开始。
    pub fn refresh_video(&self, conn: i32) -> Result<(), ApiError> {
        if let Some(result) = rpc::call("refresh_video", json!({"conn":conn})) {
            return result.map(|_| ());
        }
        check(unsafe { yd_engine_video_refresh(conn) })
    }
}

/// 引擎输入注入。
pub struct EngineInput {
    pressed: HashSet<MouseButton>,
    keys: HashSet<char>,
    controls: HashSet<i32>,
    modifiers: u32,
}

impl EngineInput {
    /// 创建注入器。
    pub fn new() -> Self {
        Self {
            pressed: HashSet::new(),
            keys: HashSet::new(),
            controls: HashSet::new(),
            modifiers: 0,
        }
    }
}

impl Default for EngineInput {
    fn default() -> Self {
        Self::new()
    }
}

impl InputSink for EngineInput {
    fn mouse(&mut self, event: MouseInput) -> Result<(), ApiError> {
        let mask = match event {
            MouseInput::Move { x, y } => {
                return check(unsafe { yd_engine_mouse(crate::desk::input::MOUSE_TYPE_MOVE, x, y) })
            }
            MouseInput::MoveRelative { dx, dy } => {
                return check(unsafe {
                    yd_engine_mouse(crate::desk::input::MOUSE_TYPE_MOVE_RELATIVE, dx, dy)
                })
            }
            MouseInput::Down { button } => {
                if self.pressed.contains(&button) {
                    return Err(ApiError::new(
                        409,
                        codes::STATE_CONFLICT,
                        "mouse button already pressed",
                    ));
                }
                self.pressed.insert(button);
                (button_bit(button) << 3) | crate::desk::input::MOUSE_TYPE_DOWN
            }
            MouseInput::Up { button } => {
                self.pressed.remove(&button);
                (button_bit(button) << 3) | crate::desk::input::MOUSE_TYPE_UP
            }
            MouseInput::Wheel {
                horizontal,
                vertical,
            } => {
                return check(unsafe {
                    yd_engine_mouse(crate::desk::input::MOUSE_TYPE_WHEEL, horizontal, vertical)
                })
            }
        };
        check(unsafe { yd_engine_mouse(mask, 0, 0) })
    }

    fn key(&mut self, event: KeyInput) -> Result<(), ApiError> {
        match event {
            KeyInput::Text(text) => {
                if text.len() > 4096 {
                    return Err(ApiError::new(400, codes::INVALID_REQUEST, "text too long"));
                }
                let Ok(cstring) = std::ffi::CString::new(text) else {
                    return Err(ApiError::new(
                        400,
                        codes::INVALID_REQUEST,
                        "text contains NUL",
                    ));
                };
                check(unsafe { yd_engine_text(cstring.as_ptr()) })
            }
            KeyInput::Control { code, down } => {
                let bit = match code {
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
                let modifiers = if down {
                    self.modifiers | bit
                } else {
                    self.modifiers & !bit
                };
                check(unsafe { yd_engine_key_with_modifiers(code, i32::from(down), modifiers) })?;
                self.modifiers = modifiers;
                if down {
                    self.controls.insert(code);
                } else {
                    self.controls.remove(&code);
                }
                Ok(())
            }
            KeyInput::Layout { ch, down } => {
                check(unsafe { yd_engine_layout(ch as u32, i32::from(down), self.modifiers) })?;
                if down {
                    self.keys.insert(ch);
                } else {
                    self.keys.remove(&ch);
                }
                Ok(())
            }
        }
    }

    fn release_all(&mut self) {
        for key in self.keys.drain() {
            let _ = unsafe { yd_engine_layout(key as u32, 0, self.modifiers) };
        }
        for code in self.controls.drain() {
            let _ = unsafe { yd_engine_key_with_modifiers(code, 0, 0) };
        }
        self.modifiers = 0;
        let buttons: Vec<_> = self.pressed.drain().collect();
        for button in buttons {
            let mask = (button_bit(button) << 3) | crate::desk::input::MOUSE_TYPE_UP;
            let _ = unsafe { yd_engine_mouse(mask, 0, 0) };
        }
    }
}

fn button_bit(button: MouseButton) -> i32 {
    match button {
        MouseButton::Left => crate::desk::input::MOUSE_BUTTON_LEFT,
        MouseButton::Right => crate::desk::input::MOUSE_BUTTON_RIGHT,
        MouseButton::Middle => crate::desk::input::MOUSE_BUTTON_WHEEL,
        MouseButton::Back => crate::desk::input::MOUSE_BUTTON_BACK,
        MouseButton::Forward => crate::desk::input::MOUSE_BUTTON_FORWARD,
    }
}

fn check(rc: i32) -> Result<(), ApiError> {
    if rc == YD_OK {
        Ok(())
    } else {
        Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "engine rejected input",
        ))
    }
}
