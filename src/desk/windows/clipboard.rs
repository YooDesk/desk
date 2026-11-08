//! Windows 剪贴板：文本（CF_UNICODETEXT）与图像（CF_DIB）读写。
//!
//! 只实现端侧需要的两个方向，且每次操作都显式 `OpenClipboard`/`CloseClipboard`，
//! 失败时返回明确错误而不是静默丢弃。图像以未压缩 RGBA8 交给上层，与
//! `hbb.Clipboard.Format.ImageRgba` 对齐。
//!
//! 未实现：RTF/HTML/SVG、文件列表（CF_HDROP）与特殊格式；这些需要单独的能力声明，
//! 不能因为 `CF_DIB` 可用就声称支持。

#![allow(unsafe_code)]

use std::mem::size_of;

use winapi::shared::minwindef::HGLOBAL;
use winapi::shared::windef::HWND;
use winapi::um::winbase::{
    GlobalAlloc, GlobalFree, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use winapi::um::winuser::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData, CF_DIB,
    CF_UNICODETEXT,
};

use crate::desk::{Clipboard, ClipboardContent};
use crate::json::{codes, ApiError};

/// Windows 剪贴板访问器。
///
/// 不持有任何全局锁或句柄；每次 `read`/`write` 独立打开剪贴板，避免长时间占用
/// 影响用户其它程序。
pub struct WinClipboard;

impl WinClipboard {
    /// 创建访问器。
    pub fn new() -> Self {
        Self
    }

    fn open(&self) -> Result<ClipboardGuard, ApiError> {
        let ok = unsafe { OpenClipboard(std::ptr::null_mut() as HWND) };
        if ok == 0 {
            return Err(ApiError::new(503, codes::DESK_ENGINE, "clipboard is busy").retryable(true));
        }
        Ok(ClipboardGuard)
    }
}

impl Default for WinClipboard {
    fn default() -> Self {
        Self::new()
    }
}

/// 打开期间保证关闭。
struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

impl Clipboard for WinClipboard {
    fn read(&mut self) -> Result<Option<ClipboardContent>, ApiError> {
        let _guard = self.open()?;
        unsafe {
            // 优先文本：与浏览器 `navigator.clipboard.readText()` 的常见用法一致；
            // 图像仅在文本不可用时读取。
            if let Some(text) = read_text()? {
                return Ok(Some(ClipboardContent::Text(text)));
            }
            if let Some(image) = read_dib()? {
                return Ok(Some(image));
            }
        }
        Ok(None)
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<(), ApiError> {
        let _guard = self.open()?;
        unsafe {
            // 返回值 0 表示失败；有些环境（如会话未连接）会拒绝清空剪贴板。
            if EmptyClipboard() == 0 {
                return Err(ApiError::new(
                    503,
                    codes::DESK_ENGINE,
                    "clipboard is not writable",
                ));
            }
            match content {
                ClipboardContent::Text(text) => write_text(text),
                ClipboardContent::ImageRgba {
                    width,
                    height,
                    data,
                } => write_dib(*width, *height, data),
            }
        }
    }
}

/// 读取 `CF_UNICODETEXT`。
unsafe fn read_text() -> Result<Option<String>, ApiError> {
    let handle = GetClipboardData(CF_UNICODETEXT);
    if handle.is_null() {
        return Ok(None);
    }
    let handle = handle as HGLOBAL;
    let size = GlobalSize(handle);
    if size == 0 {
        return Ok(None);
    }
    let ptr = GlobalLock(handle) as *const u16;
    if ptr.is_null() {
        return Ok(None);
    }
    let units = size / size_of::<u16>();
    let slice = std::slice::from_raw_parts(ptr, units);
    let end = slice.iter().position(|unit| *unit == 0).unwrap_or(units);
    let text = String::from_utf16_lossy(&slice[..end]);
    GlobalUnlock(handle);
    if text.len() > MAX_TEXT_BYTES {
        return Ok(None);
    }
    Ok(Some(text))
}

/// 读取 `CF_DIB` 并转换为 RGBA。
unsafe fn read_dib() -> Result<Option<ClipboardContent>, ApiError> {
    let handle = GetClipboardData(CF_DIB);
    if handle.is_null() {
        return Ok(None);
    }
    let handle = handle as HGLOBAL;
    let size = GlobalSize(handle);
    // 头部至少要容纳 BITMAPINFOHEADER。
    if size < size_of::<winapi::um::wingdi::BITMAPINFOHEADER>() {
        return Ok(None);
    }
    let ptr = GlobalLock(handle) as *const u8;
    if ptr.is_null() {
        return Ok(None);
    }
    let bytes = std::slice::from_raw_parts(ptr, size);
    let result = decode_dib(bytes);
    GlobalUnlock(handle);
    Ok(result)
}

/// 解析 DIB：支持 24/32 位未压缩、自下而上或自上而下的行序。
fn decode_dib(bytes: &[u8]) -> Option<ClipboardContent> {
    use winapi::um::wingdi::{BITMAPINFOHEADER, BI_RGB};
    let header_size = size_of::<BITMAPINFOHEADER>();
    if bytes.len() < header_size {
        return None;
    }
    let header = unsafe { &*(bytes.as_ptr() as *const BITMAPINFOHEADER) };
    if header.biCompression != BI_RGB || header.biPlanes != 1 {
        return None;
    }
    let bpp = header.biBitCount;
    if bpp != 24 && bpp != 32 {
        return None;
    }
    let width = header.biWidth;
    let height = header.biHeight;
    let top_down = height < 0;
    let height = height.unsigned_abs();
    if width <= 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    let (w, h) = (width as u32, height);
    let stride = (width as usize * bpp as usize).div_ceil(32) * 4;
    let pixel_offset = header.biSize as usize + header.biClrUsed as usize * 4;
    let needed = pixel_offset + stride * height as usize;
    if bytes.len() < needed {
        return None;
    }
    let pixels = &bytes[pixel_offset..];
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..height as usize {
        let source_row = if top_down {
            row
        } else {
            height as usize - 1 - row
        };
        let line =
            &pixels[source_row * stride..source_row * stride + width as usize * (bpp as usize / 8)];
        for pixel in line.chunks_exact(bpp as usize / 8) {
            let (b, g, r) = (pixel[0], pixel[1], pixel[2]);
            let a = if bpp == 32 && pixel[3] != 0 {
                pixel[3]
            } else {
                0xff
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    Some(ClipboardContent::ImageRgba {
        width: w,
        height: h,
        data: rgba,
    })
}

/// 写入 `CF_UNICODETEXT`。
unsafe fn write_text(text: &str) -> Result<(), ApiError> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * size_of::<u16>();
    let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
    if memory.is_null() {
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard allocation failed",
        ));
    }
    let ptr = GlobalLock(memory) as *mut u16;
    if ptr.is_null() {
        GlobalFree(memory);
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard lock failed",
        ));
    }
    std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
    GlobalUnlock(memory);
    // 成功后所有权交给系统，不能再释放。
    if SetClipboardData(CF_UNICODETEXT, memory as *mut _).is_null() {
        GlobalFree(memory);
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard write failed",
        ));
    }
    Ok(())
}

/// 写入 `CF_DIB`（32 位自下而上）。
unsafe fn write_dib(width: u32, height: u32, rgba: &[u8]) -> Result<(), ApiError> {
    use winapi::um::wingdi::{BITMAPINFOHEADER, BI_RGB};
    if width == 0 || height == 0 || rgba.len() != (width * height * 4) as usize {
        return Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "image size mismatch",
        ));
    }
    if width > 8192 || height > 8192 {
        return Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "image too large",
        ));
    }
    let stride = (width as usize * 4 + 3) & !3;
    let pixel_bytes = stride * height as usize;
    let header_bytes = size_of::<BITMAPINFOHEADER>();
    let total = header_bytes + pixel_bytes;
    let memory = GlobalAlloc(GMEM_MOVEABLE, total);
    if memory.is_null() {
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard allocation failed",
        ));
    }
    let base = GlobalLock(memory) as *mut u8;
    if base.is_null() {
        GlobalFree(memory);
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard lock failed",
        ));
    }
    let header = BITMAPINFOHEADER {
        biSize: header_bytes as u32,
        biWidth: width as i32,
        biHeight: height as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: pixel_bytes as u32,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    std::ptr::copy_nonoverlapping(&header as *const _ as *const u8, base, header_bytes);
    let pixels = base.add(header_bytes);
    // RGBA → BGRA，并翻转行序（DIB 默认自下而上）。
    for row in 0..height as usize {
        let source_row = row;
        let target_row = height as usize - 1 - row;
        let source = &rgba[source_row * width as usize * 4..(source_row + 1) * width as usize * 4];
        let target =
            std::slice::from_raw_parts_mut(pixels.add(target_row * stride), width as usize * 4);
        for (index, pixel) in source.as_chunks::<4>().0.iter().enumerate() {
            target[index * 4] = pixel[2];
            target[index * 4 + 1] = pixel[1];
            target[index * 4 + 2] = pixel[0];
            target[index * 4 + 3] = pixel[3];
        }
    }
    GlobalUnlock(memory);
    if SetClipboardData(CF_DIB, memory as *mut _).is_null() {
        GlobalFree(memory);
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard write failed",
        ));
    }
    Ok(())
}

/// 单次文本上限（UTF-8 字节），与输入通道保持一致。
const MAX_TEXT_BYTES: usize = 1 << 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bottom_up_24_bit_dib() {
        use winapi::um::wingdi::{BITMAPINFOHEADER, BI_RGB};
        // 2x2：底行红绿，顶行蓝白。
        let mut bytes = vec![0u8; size_of::<BITMAPINFOHEADER>()];
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 2,
            biHeight: 2,
            biPlanes: 1,
            biBitCount: 24,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &header as *const _ as *const u8,
                bytes.as_mut_ptr(),
                size_of::<BITMAPINFOHEADER>(),
            );
        }
        // 每行 2 像素 × 3 字节 = 6 字节，4 字节对齐后 8 字节。
        bytes.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // 底行：红、绿
        bytes.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // 顶行：蓝、白
        let decoded = decode_dib(&bytes).expect("decode");
        let ClipboardContent::ImageRgba {
            width,
            height,
            data,
        } = decoded
        else {
            panic!("expected image");
        };
        assert_eq!((width, height), (2, 2));
        // 第一行应是顶行：蓝、白。
        assert_eq!(&data[0..4], &[0, 0, 255, 255]);
        assert_eq!(&data[4..8], &[255, 255, 255, 255]);
        // 第二行应是底行：红、绿。
        assert_eq!(&data[8..12], &[255, 0, 0, 255]);
        assert_eq!(&data[12..16], &[0, 255, 0, 255]);
    }

    #[test]
    fn rejects_unsupported_dib_formats() {
        use winapi::um::wingdi::{BITMAPINFOHEADER, BI_BITFIELDS};
        let mut bytes = vec![0u8; size_of::<BITMAPINFOHEADER>()];
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 2,
            biHeight: 2,
            biPlanes: 1,
            biBitCount: 16,
            biCompression: BI_BITFIELDS,
            ..unsafe { std::mem::zeroed() }
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &header as *const _ as *const u8,
                bytes.as_mut_ptr(),
                size_of::<BITMAPINFOHEADER>(),
            );
        }
        assert!(decode_dib(&bytes).is_none());
    }
}
