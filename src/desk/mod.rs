//! desk 被控端引擎：采集、编码、输入注入、光标、剪贴板与显示器枚举。
//!
//! 只集成 RustDesk 端侧的基础能力，协议、鉴权与 UI 全部由本产品自己实现
//! （见 `docs/design/04-webservice.md` §8 与 `AGENTS.md`）。
//!
//! ## 平台边界
//!
//! 所有平台差异都收敛到 [`Backend`] 及其可选能力 trait：
//!
//! | 能力 | 必需 / 可选 | 跨平台要求 |
//! | --- | --- | --- |
//! | 显示器枚举、采集 | [`Backend`] 必需 | 新平台必须实现 |
//! | 输入注入 | [`Backend::create_input`] + [`InputSink`] | 只读平台可返回 `None` |
//! | 光标形状/位置 | [`Backend::cursor_source`] → [`CursorSource`] | 可选；缺失则能力响应不声明 |
//! | 剪贴板 | [`Backend::clipboard`] → [`Clipboard`] | 可选；按方向分别声明 |
//!
//! 上层（会话、HTTP、前端）只依赖这些 trait 与 [`DeskCapabilities`]，不出现任何平台类型。
//! 新增 macOS/Linux/移动端时只需实现本模块的 trait，不改协议、不改会话逻辑。
//!
//! 媒体编码使用独立的 `openh264`（BSD），与 RustDesk 的 AGPL 代码树解耦；本模块不复制其源码。

use std::sync::Arc;
use std::time::SystemTime;

use crate::json::{codes, ApiError};

#[cfg(any(target_os = "android", test))]
mod android;
pub mod encode;
#[cfg(feature = "desk-engine")]
pub mod engine;
pub mod input;
#[cfg(windows)]
pub mod windows;

/// 显示器几何信息，对应 `DisplayInfo` 的端侧部分与前端画布尺寸。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DisplayGeometry {
    /// 显示器索引（与 `VideoFrame.display` 一致）。
    pub index: u32,
    /// 虚拟桌面内的左上角 X。
    pub x: i32,
    /// 虚拟桌面内的左上角 Y。
    pub y: i32,
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// 系统显示名称。
    pub name: String,
    /// 是否主显示器。
    pub primary: bool,
}

impl DisplayGeometry {
    /// 该显示器在虚拟桌面中的右边界。
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    /// 该显示器在虚拟桌面中的下边界。
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }
}

/// 一帧 BGRA 像素（各平台采集统一输出该格式，转换由编码器负责）。
#[derive(Debug, Clone)]
pub struct RgbaFrame {
    /// 显示器索引。
    pub display: u32,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// BGRA8 紧凑像素，长度为 `width * height * 4`。
    pub data: Vec<u8>,
    /// 采集时刻。
    pub captured_at: SystemTime,
}

/// 采集方式；用于能力响应与诊断，不参与协议语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// GPU 桌面复制（Windows DXGI）。
    DesktopDuplication,
    /// GDI 屏幕位图（Windows 回退）。
    Gdi,
    /// 平台原生其它方式；新平台实现时自行命名到此处以外的枚举值。
    Native,
}

/// 光标形状与位置。
///
/// 形状用 `id` 去重：同一光标形状只传一次，浏览器缓存后按 id 引用，
/// 后续只发生标移动。这避免每帧内嵌光标像素。
#[derive(Debug, Clone)]
pub struct CursorFrame {
    /// 光标形状 id（同一形状在同一会话内稳定）。
    pub id: u64,
    /// 热点 X。
    pub hotx: i32,
    /// 热点 Y。
    pub hoty: i32,
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// RGBA8 紧凑像素；与服务端当前形状相同则为 `None`（只需位置）。
    pub rgba: Option<Vec<u8>>,
    /// 光标在目标显示器坐标系内的位置；平台无法读取时为 `None`。
    pub position: Option<(i32, i32)>,
}

/// 剪贴板内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    /// UTF-8 文本。
    Text(String),
    /// 未压缩 RGBA8 图像。
    ImageRgba {
        /// 宽。
        width: u32,
        /// 高。
        height: u32,
        /// RGBA8 紧凑像素。
        data: Vec<u8>,
    },
}

/// 采集器接口：一个会话一个实例，独占对应显示器。
pub trait Capturer: Send {
    /// Separate startup budget for platforms that require local consent before capture.
    /// When set, the stream attachment deadline starts after the first frame is ready.
    fn first_frame_timeout(&self) -> Option<std::time::Duration> {
        None
    }

    /// 显示器几何。
    fn geometry(&self) -> &DisplayGeometry;

    /// 阻塞直到有画面变化或超时；`WouldBlock` 表示超时无新帧。
    fn next_frame(&mut self, timeout: std::time::Duration) -> std::io::Result<RgbaFrame>;

    /// 主动读取一次当前画面（不等待变化）。
    ///
    /// 静态桌面没有增量更新，而新观众必须有可解码的起始帧；增量采集器需要覆盖此方法
    /// 提供一次完整快照。默认实现直接复用增量路径。
    fn snapshot(&mut self) -> std::io::Result<RgbaFrame> {
        self.next_frame(std::time::Duration::from_millis(0))
    }

    /// 当前实际采集方式。
    fn mode(&self) -> CaptureMode;

    /// 是否已回退到平台回退路径（Windows 为 GDI）。
    ///
    /// 保留该方法而不只暴露 [`CaptureMode`]，因为会话日志需要稳定的布尔语义；
    /// 非 Windows 平台默认返回 `false`。
    fn is_gdi(&self) -> bool {
        false
    }
}

/// 输入注入接口。
pub trait InputSink: Send {
    /// 鼠标事件，坐标为物理像素（虚拟桌面绝对坐标）。
    fn mouse(&mut self, event: input::MouseInput) -> Result<(), ApiError>;

    /// 键盘事件。
    fn key(&mut self, event: input::KeyInput) -> Result<(), ApiError>;

    /// 释放所有按下的键与按钮，用于会话关闭。
    fn release_all(&mut self);
}

/// 光标来源：实现该 trait 的平台才会声明光标能力。
pub trait CursorSource: Send {
    /// 读取当前光标；形状未变化时 `rgba` 为 `None`。
    fn poll(&mut self) -> Result<Option<CursorFrame>, ApiError>;

    /// 让下一次 `poll` 重新发送形状。
    ///
    /// 新观众加入时调用：光标形状只在变化时产生，而广播不保留历史，
    /// 订阅者可能错过唯一一次。默认实现不做任何事（平台实现应按需覆盖）。
    fn resend_shape(&mut self) {}
}

/// 剪贴板访问：读写分离声明，平台只支持单向时另一方向返回 `unsupported_feature`。
pub trait Clipboard: Send {
    /// 读取当前剪贴板；内容为空返回 `Ok(None)`。
    fn read(&mut self) -> Result<Option<ClipboardContent>, ApiError>;

    /// 写入剪贴板。
    fn write(&mut self, content: &ClipboardContent) -> Result<(), ApiError>;
}

/// 平台能力描述。
///
/// 这是端点 `capabilities/` 与 `supported_features` 的唯一来源：新增平台只实现 trait，
/// 声明会自动出现在响应里，不需要改 HTTP 层。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeskCapabilities {
    /// 是否支持桌面采集。
    pub capture: bool,
    /// 是否支持显示器枚举。
    pub displays: bool,
    /// 是否支持 H.264 编码。
    pub h264: bool,
    /// 是否支持指针注入。
    pub pointer: bool,
    /// 是否支持键盘注入。
    pub keyboard: bool,
    /// 是否支持光标形状/位置上报。
    pub cursor: bool,
    /// 是否支持读取剪贴板。
    pub clipboard_read: bool,
    /// 是否支持写入剪贴板。
    pub clipboard_write: bool,
    /// 是否支持系统音频采集。
    pub audio: bool,
    /// 本产品双向 HTTP 聊天。
    pub chat: bool,
    /// 支持会话内文件传输。
    pub file_transfer: bool,
    /// HDR 桌面经色调映射输出 SDR 视频。
    pub hdr: bool,
    /// 支持本地隐私屏。
    pub privacy_screen: bool,
    /// 是否支持多显示器（可枚举并分别采集）。
    pub multi_display: bool,
}

impl DeskCapabilities {
    /// 全无能力：用于明确不支持的平台。
    pub fn none() -> Self {
        Self {
            capture: false,
            displays: false,
            h264: false,
            pointer: false,
            keyboard: false,
            cursor: false,
            clipboard_read: false,
            clipboard_write: false,
            audio: false,
            chat: false,
            file_transfer: false,
            hdr: false,
            privacy_screen: false,
            multi_display: false,
        }
    }

    /// 转成 `capabilities/` 的 feature 标识列表。
    ///
    /// 标识名是前端与测试共同依赖的稳定契约，新增能力只能追加。
    pub fn features(&self) -> Vec<String> {
        let mut list = Vec::new();
        if self.chat {
            list.push("desk.chat".into());
        }
        if self.displays {
            list.push("desk.display.list".to_string());
        }
        if self.capture {
            list.push("desk.capture".to_string());
            list.push("desk.video.h264s".to_string());
        }
        if self.h264 {
            list.push("desk.video.encode.h264".to_string());
        }
        if self.pointer {
            list.push("desk.input.pointer".to_string());
        }
        if self.keyboard {
            list.push("desk.input.keyboard".to_string());
        }
        if self.cursor {
            list.push("desk.cursor.shape".to_string());
            list.push("desk.cursor.position".to_string());
        }
        if self.clipboard_read {
            list.push("desk.clipboard.read".to_string());
        }
        if self.clipboard_write {
            list.push("desk.clipboard.write".to_string());
        }
        if self.audio {
            list.push("desk.audio".to_string());
        }
        if self.file_transfer {
            list.push("desk.file.transfer".to_string());
        }
        if self.hdr {
            list.push("desk.video.hdr_to_sdr".to_string());
        }
        if self.privacy_screen {
            list.push("desk.privacy.screen".to_string());
        }
        if self.multi_display {
            list.push("desk.display.multi".to_string());
        }
        list.sort();
        list
    }
}

/// 后端名称与运行环境，用于能力响应、诊断与跨平台排查。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlatformInfo {
    /// 操作系统标识（`std::env::consts::OS`）。
    pub os: &'static str,
    /// CPU 架构（`std::env::consts::ARCH`）。
    pub arch: &'static str,
    /// 后端实现名。
    pub backend: &'static str,
    /// 采集方式（创建采集器前为后端首选方式）。
    pub capture_mode: CaptureMode,
    /// 当前桌面会话是否可注入输入。
    pub input_available: bool,
}

/// 被控端后端：平台能力集合。
pub trait Backend: Send + Sync + 'static {
    /// 设备能接收新的采集请求；需系统授权的平台可先接受请求，再等待首帧。
    fn accepts_capture_request(&self) -> bool {
        self.capabilities().capture
    }

    /// 检查会话目标；系统尚未提供几何时，平台可以接受自己的默认显示器。
    fn accepts_display(&self, display: u32) -> Result<bool, ApiError> {
        Ok(self.displays()?.iter().any(|item| item.index == display))
    }

    /// 后端名称，用于能力响应和日志。
    fn name(&self) -> &'static str;

    /// 能力声明；默认由必需方法与可选工厂推导，平台可覆盖。
    fn capabilities(&self) -> DeskCapabilities {
        let displays = self.displays().is_ok();
        let pointer = self.supports_input();
        DeskCapabilities {
            capture: true,
            displays,
            h264: self.supports_h264(),
            pointer,
            keyboard: pointer,
            cursor: self.cursor_source().is_some(),
            clipboard_read: self.clipboard().is_some(),
            clipboard_write: self.clipboard().is_some(),
            audio: false,
            chat: false,
            file_transfer: false,
            hdr: false,
            privacy_screen: false,
            multi_display: displays && self.displays().map(|d| d.len() > 1).unwrap_or(false),
        }
    }

    /// 枚举当前在线显示器；顺序稳定，主显示器优先。
    fn displays(&self) -> Result<Vec<DisplayGeometry>, ApiError>;

    /// 为指定显示器创建采集器。
    fn create_capturer(&self, display: u32) -> Result<Box<dyn Capturer>, ApiError>;

    /// 创建输入注入器。
    fn create_input(&self) -> Result<Box<dyn InputSink>, ApiError>;

    /// 本平台是否支持指针/键盘注入（只读能力必须如实反映）。
    fn supports_input(&self) -> bool;

    /// 是否支持 H.264 编码（依赖 `openh264` 编译期可用性）。
    fn supports_h264(&self) -> bool;

    /// 光标来源；不支持时返回 `None`。
    fn cursor_source(&self) -> Option<Box<dyn CursorSource>> {
        None
    }

    /// 剪贴板；不支持时返回 `None`。
    fn clipboard(&self) -> Option<Box<dyn Clipboard>> {
        None
    }

    /// 首选采集方式（创建采集器前用于能力响应）。
    fn capture_mode(&self) -> CaptureMode {
        CaptureMode::Native
    }

    /// 若后端由 RustDesk 引擎提供，返回它的强类型句柄。
    ///
    /// 引擎完成采集与编码，与会话层的“采集器 + 编码器”模型不同，因此需要一个
    /// 类型化的入口；其余后端返回 `None`，走通用路径。
    #[cfg(feature = "desk-engine")]
    fn as_any_engine(&self) -> Option<&engine::EngineBackend> {
        None
    }

    /// 平台信息。
    fn platform_info(&self) -> PlatformInfo {
        PlatformInfo {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            backend: self.name(),
            capture_mode: self.capture_mode(),
            input_available: self.supports_input(),
        }
    }
}

/// 明确不支持的平台后端。
///
/// 不回落到模拟画面，也不声称已接入 RustDesk；调用方据此返回 `unsupported_feature`。
pub struct UnsupportedBackend {
    /// 平台说明，进日志与能力响应。
    pub platform: &'static str,
}

impl Backend for UnsupportedBackend {
    fn name(&self) -> &'static str {
        self.platform
    }

    fn capabilities(&self) -> DeskCapabilities {
        DeskCapabilities::none()
    }

    fn displays(&self) -> Result<Vec<DisplayGeometry>, ApiError> {
        Err(unsupported())
    }

    fn create_capturer(&self, _display: u32) -> Result<Box<dyn Capturer>, ApiError> {
        Err(unsupported())
    }

    fn create_input(&self) -> Result<Box<dyn InputSink>, ApiError> {
        Err(unsupported())
    }

    fn supports_input(&self) -> bool {
        false
    }

    fn supports_h264(&self) -> bool {
        encode::h264_available()
    }
}

/// 平台未实现该能力时的统一错误。
pub fn unsupported() -> ApiError {
    ApiError::new(
        501,
        codes::UNSUPPORTED_FEATURE,
        "desk feature is not implemented for this platform",
    )
}

/// 平台默认后端。
///
/// Windows 返回真实 DXGI/GDI 采集、`SendInput` 注入、光标与剪贴板；
/// 其他平台返回明确不支持（后续实现各平台 trait 后替换此处即可）。
pub fn default_backend() -> Arc<dyn Backend> {
    // desk-engine 优先：它直接复用 RustDesk 的采集/编码/输入，包含多显示器、
    // 音频与隐私屏等自研后端尚未实现的能力。
    #[cfg(feature = "desk-engine")]
    {
        Arc::new(engine::EngineBackend::new())
    }
    #[cfg(all(windows, not(feature = "desk-engine")))]
    {
        Arc::new(windows::WindowsBackend::new())
    }
    #[cfg(all(target_os = "android", not(feature = "desk-engine")))]
    {
        Arc::new(android::AndroidBackend)
    }
    #[cfg(all(not(any(windows, target_os = "android")), not(feature = "desk-engine")))]
    {
        Arc::new(UnsupportedBackend {
            platform: std::env::consts::OS,
        })
    }
}

/// 端侧实现的功能清单，写入能力响应的 `supported_features`。
pub fn features(backend: &dyn Backend) -> Vec<String> {
    backend.capabilities().features()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_geometry_bounds_are_saturating() {
        let geometry = DisplayGeometry {
            index: 0,
            x: -100,
            y: 0,
            width: 1920,
            height: 1080,
            name: "test".into(),
            primary: false,
        };
        assert_eq!(geometry.right(), 1820);
        assert_eq!(geometry.bottom(), 1080);
    }

    #[test]
    fn unsupported_backend_reports_no_capabilities() {
        let backend = UnsupportedBackend { platform: "test" };
        let capabilities = backend.capabilities();
        assert!(!capabilities.capture);
        assert!(!capabilities.pointer);
        assert!(capabilities.features().is_empty());
        assert!(backend.displays().is_err());
        assert!(backend.create_capturer(0).is_err());
        assert!(backend.create_input().is_err());
        assert!(backend.cursor_source().is_none());
        assert!(backend.clipboard().is_none());
    }

    #[test]
    fn features_only_advertise_declared_capabilities() {
        let capabilities = DeskCapabilities {
            capture: true,
            displays: true,
            h264: true,
            pointer: false,
            keyboard: false,
            cursor: false,
            clipboard_read: false,
            clipboard_write: false,
            audio: false,
            chat: false,
            file_transfer: false,
            hdr: false,
            privacy_screen: false,
            multi_display: false,
        };
        let features = capabilities.features();
        assert!(features.contains(&"desk.capture".to_string()));
        assert!(!features.contains(&"desk.input.pointer".to_string()));
        assert!(!features.contains(&"desk.cursor.shape".to_string()));
    }

    #[test]
    fn capability_features_are_sorted_and_unique() {
        let capabilities = DeskCapabilities {
            capture: true,
            displays: true,
            h264: true,
            pointer: true,
            keyboard: true,
            cursor: true,
            clipboard_read: true,
            clipboard_write: true,
            audio: false,
            chat: false,
            file_transfer: false,
            hdr: false,
            privacy_screen: false,
            multi_display: true,
        };
        let features = capabilities.features();
        let mut sorted = features.clone();
        sorted.sort();
        assert_eq!(features, sorted);
        let unique: std::collections::BTreeSet<_> = features.iter().collect();
        assert_eq!(unique.len(), features.len());
    }
}
