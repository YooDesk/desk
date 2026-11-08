//! Windows 被控端后端装配。
//!
//! 把桌面采集（DXGI/GDI）、输入注入（`SendInput`）、光标与剪贴板组合成 [`Backend`]。
//! 只暴露本产品自己的抽象，不向上层泄露 Win32 类型，便于其它平台按同一组 trait 接入。

#![allow(unsafe_code)]

mod capture;
mod clipboard;
mod input;

pub use capture::DxgiCapturer;
pub use clipboard::WinClipboard;
pub use input::{WinCursor, WinInput};

use std::sync::Arc;

use crate::desk::{
    Backend, CaptureMode, Capturer, Clipboard, CursorSource, DeskCapabilities, DisplayGeometry,
    InputSink,
};
use crate::json::ApiError;

/// Windows 后端。
pub struct WindowsBackend;

impl WindowsBackend {
    /// 创建后端；无状态，可重复创建。
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for WindowsBackend {
    fn name(&self) -> &'static str {
        "windows-dxgi"
    }

    fn capabilities(&self) -> DeskCapabilities {
        let displays = self.displays();
        let display_count = displays.as_ref().map(|list| list.len()).unwrap_or(0);
        DeskCapabilities {
            capture: true,
            displays: displays.is_ok(),
            h264: crate::desk::encode::h264_available(),
            pointer: self.supports_input(),
            keyboard: self.supports_input(),
            // 光标与剪贴板读取都不依赖桌面会话是否可注入，只要进程能访问用户桌面即可。
            cursor: true,
            clipboard_read: true,
            clipboard_write: true,
            // 音频尚未接入：不声明，避免前端显示不可用功能。
            audio: false,
            chat: true,
            file_transfer: false,
            hdr: false,
            privacy_screen: false,
            multi_display: display_count > 1,
        }
    }

    fn displays(&self) -> Result<Vec<DisplayGeometry>, ApiError> {
        capture::enumerate_displays()
    }

    fn create_capturer(&self, display_id: u32) -> Result<Box<dyn Capturer>, ApiError> {
        let displays = capture::enumerate_displays()?;
        let geometry = displays
            .into_iter()
            .find(|d| d.index == display_id)
            .ok_or_else(|| {
                ApiError::new(404, crate::json::codes::NOT_FOUND, "display not found")
            })?;
        // DXGI 初始化失败（老显卡、远程会话等）时使用 GDI；这是设计内的平台回退，
        // 不是掩盖错误的兜底：`mode()` 会如实上报，UI 据此提示画质/帧率限制。
        match DxgiCapturer::new(geometry.clone()) {
            Ok(capturer) => Ok(Box::new(capturer)),
            Err(err) => {
                tracing::warn!(error = %err, display_index = display_id, "DXGI 初始化失败，使用 GDI 采集");
                let gdi = capture::GdiCapturer::new(&geometry).map_err(|gdi_err| {
                    tracing::error!(error = %gdi_err, display_index = display_id, "GDI 采集器创建失败");
                    ApiError::new(
                        503,
                        crate::json::codes::DESK_ENGINE,
                        "no display capturer available",
                    )
                })?;
                Ok(Box::new(gdi))
            }
        }
    }

    fn create_input(&self) -> Result<Box<dyn InputSink>, ApiError> {
        Ok(Box::new(WinInput::new()))
    }

    fn supports_input(&self) -> bool {
        // 会话可能运行在服务/安全桌面，那里 SendInput 会被系统拒绝；
        // 只做“桌面可枚举”的粗判，真正失败在注入时以 desk_engine 报告。
        input::injection_available()
    }

    fn supports_h264(&self) -> bool {
        crate::desk::encode::h264_available()
    }

    fn cursor_source(&self) -> Option<Box<dyn CursorSource>> {
        Some(Box::new(WinCursor::new()))
    }

    fn clipboard(&self) -> Option<Box<dyn Clipboard>> {
        Some(Box::new(WinClipboard::new()))
    }

    fn capture_mode(&self) -> CaptureMode {
        CaptureMode::DesktopDuplication
    }
}

/// 平台后端构造器。
pub fn backend() -> Arc<dyn Backend> {
    Arc::new(WindowsBackend::new())
}
