//! Windows 输入注入：`SendInput` 实现指针与键盘事件。
//!
//! 只做端侧需要的注入，不包含 RustDesk 的 UAC 提权、隐私屏等扩展逻辑。
//! 坐标语义与端侧一致：`MouseEvent.x/y` 为虚拟桌面物理像素；滚轮量在注入前按
//! `WHEEL_DELTA` 换算。未知控制键码明确拒绝，不猜测。
//!
//! 控制键编号来自 `crate::desk::input::control_key`（冻结快照），本文件只负责
//! **编号 → Windows 虚拟键码** 的映射。上游 main 的编号与快照不同，不能混用。

#![allow(unsafe_code)]

use std::collections::HashSet;
use std::mem::size_of;

use winapi::shared::windef::{HCURSOR, POINT};
use winapi::um::wingdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use winapi::um::winuser::*;

use crate::desk::input::{control_key, KeyInput, MouseButton, MouseInput};
use crate::desk::{CursorFrame, CursorSource, InputSink};
use crate::json::{codes, ApiError};

/// 注入器实例；每个会话一个，记录按下的键与按钮用于退出时释放。
pub struct WinInput {
    pressed_keys: HashSet<u16>,
    pressed_buttons: HashSet<MouseButton>,
}

impl WinInput {
    /// 创建注入器。
    pub fn new() -> Self {
        Self {
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
        }
    }

    fn send(&self, input: INPUT) -> Result<(), ApiError> {
        let mut input = input;
        let sent = unsafe { SendInput(1, &mut input as *mut _, size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(ApiError::new(503, codes::DESK_ENGINE, "SendInput rejected"))
        }
    }

    fn mouse_event(&self, flags: u32, dx: i32, dy: i32, data: u32) -> Result<(), ApiError> {
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.type_ = INPUT_MOUSE;
        unsafe {
            *input.u.mi_mut() = MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            };
        }
        self.send(input)
    }

    fn key_event(&self, flags: u32, vk: u16, scan: u16) -> Result<(), ApiError> {
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.type_ = INPUT_KEYBOARD;
        unsafe {
            *input.u.ki_mut() = KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            };
        }
        self.send(input)
    }

    /// 把虚拟桌面像素坐标转换为 `MOUSEEVENTF_ABSOLUTE` 需要的 0..=65535 归一化值。
    ///
    /// `SendInput` 的绝对坐标按虚拟桌面范围映射，因此必须使用 `SM_*VIRTUALSCREEN` 指标。
    fn to_absolute(value: i32, origin: i32, extent: i32) -> Option<i32> {
        if extent <= 0 {
            return None;
        }
        let absolute = (value as i64 - origin as i64) * 65535 / extent as i64;
        Some(absolute.clamp(0, 65535) as i32)
    }

    /// 冻结快照编号 → Windows 虚拟键码。
    ///
    /// 只映射键盘输入必需的控制键；未列出的码返回 `None`，由调用方明确拒绝。
    fn control_key_to_vk(code: i32) -> Option<u16> {
        let vk = match code {
            control_key::BACKSPACE => VK_BACK,
            control_key::TAB => VK_TAB,
            control_key::RETURN => VK_RETURN,
            control_key::SHIFT => VK_LSHIFT,
            control_key::R_SHIFT => VK_RSHIFT,
            control_key::CONTROL => VK_LCONTROL,
            control_key::R_CONTROL => VK_RCONTROL,
            control_key::ALT => VK_LMENU,
            control_key::R_ALT => VK_RMENU,
            control_key::META => VK_LWIN,
            control_key::R_WIN => VK_RWIN,
            control_key::ESCAPE => VK_ESCAPE,
            control_key::SPACE => VK_SPACE,
            control_key::CAPS_LOCK => VK_CAPITAL,
            control_key::NUM_LOCK => VK_NUMLOCK,
            control_key::SCROLL_LOCK => VK_SCROLL,
            control_key::PAGE_UP => VK_PRIOR,
            control_key::PAGE_DOWN => VK_NEXT,
            control_key::END => VK_END,
            control_key::HOME => VK_HOME,
            control_key::LEFT_ARROW => VK_LEFT,
            control_key::UP_ARROW => VK_UP,
            control_key::RIGHT_ARROW => VK_RIGHT,
            control_key::DOWN_ARROW => VK_DOWN,
            control_key::INSERT => VK_INSERT,
            control_key::DELETE => VK_DELETE,
            control_key::F1 => VK_F1,
            control_key::F2 => VK_F2,
            control_key::F3 => VK_F3,
            control_key::F4 => VK_F4,
            control_key::F10 => VK_F10,
            control_key::F11 => VK_F11,
            control_key::F12 => VK_F12,
            _ => return None,
        };
        Some(vk as u16)
    }

    /// 按 Unicode 码元注入一个字符（不依赖目标机键盘布局）。
    fn unicode_input(&self, unit: u16, down: bool) -> Result<(), ApiError> {
        let flags = if down { 0 } else { KEYEVENTF_KEYUP } | KEYEVENTF_UNICODE;
        self.key_event(flags, 0, unit)
    }

    /// 注入文本：逐 UTF-16 码元按下再抬起，代理对由系统合成。
    fn send_text(&mut self, text: &str) -> Result<(), ApiError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "text payload too large",
            ));
        }
        for unit in text.encode_utf16() {
            self.unicode_input(unit, true)?;
            self.unicode_input(unit, false)?;
        }
        Ok(())
    }

    /// 注入布局字符键，用于组合键（Ctrl+C 等）。
    ///
    /// `VkKeyScanW` 给出该字符在当前活动布局下的虚拟键与 Shift 状态；
    /// 需要 Ctrl/Alt 才能打出的字符不在组合键场景内，明确拒绝。
    fn send_layout(&mut self, ch: char, down: bool) -> Result<(), ApiError> {
        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        if units.len() != 1 {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "layout key must be a BMP character",
            ));
        }
        let value = units[0];
        if value > 0x7f {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "layout key must be ASCII",
            ));
        }
        let scan = unsafe { VkKeyScanW(value) };
        if scan == -1 {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "character is not reachable in the current layout",
            ));
        }
        let vk = (scan as u16) & 0xff;
        let modifiers = ((scan as u16) >> 8) & 0xff;
        // 只接受无修饰或仅 Shift 的字符；Ctrl/Alt 组合需要用户显式发送控制键。
        if modifiers & 0x6 != 0 {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "character requires ctrl/alt in the current layout",
            ));
        }
        let shift = modifiers & 0x1 != 0;
        let vk_scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        if down && shift {
            self.key_event(0, VK_SHIFT as u16, unsafe {
                MapVirtualKeyW(VK_SHIFT as u32, MAPVK_VK_TO_VSC) as u16
            })?;
        }
        self.press_check_vk(vk, down)?;
        let flags = if down { 0 } else { KEYEVENTF_KEYUP };
        let result = self.key_event(flags, vk, vk_scan);
        if !down && shift {
            let _ = self.key_event(KEYEVENTF_KEYUP, VK_SHIFT as u16, unsafe {
                MapVirtualKeyW(VK_SHIFT as u32, MAPVK_VK_TO_VSC) as u16
            });
        }
        result
    }
}

/// 单次文本注入上限（UTF-8 字节）。过大的粘贴应由剪贴板通道承担。
const MAX_TEXT_BYTES: usize = 4096;

impl Default for WinInput {
    fn default() -> Self {
        Self::new()
    }
}

impl InputSink for WinInput {
    fn mouse(&mut self, event: MouseInput) -> Result<(), ApiError> {
        match event {
            MouseInput::Move { x, y } => {
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    )
                };
                let dx = Self::to_absolute(x, vx, vw).ok_or_else(|| {
                    ApiError::new(503, codes::DESK_ENGINE, "virtual screen unavailable")
                })?;
                let dy = Self::to_absolute(y, vy, vh).ok_or_else(|| {
                    ApiError::new(503, codes::DESK_ENGINE, "virtual screen unavailable")
                })?;
                self.mouse_event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, dx, dy, 0)
            }
            MouseInput::MoveRelative { dx, dy } => self.mouse_event(MOUSEEVENTF_MOVE, dx, dy, 0),
            MouseInput::Down { button } => {
                let flag = match button {
                    MouseButton::Left => MOUSEEVENTF_LEFTDOWN,
                    MouseButton::Right => MOUSEEVENTF_RIGHTDOWN,
                    MouseButton::Middle => MOUSEEVENTF_MIDDLEDOWN,
                    MouseButton::Back | MouseButton::Forward => MOUSEEVENTF_XDOWN,
                };
                let data = match button {
                    MouseButton::Back => XBUTTON1 as u32,
                    MouseButton::Forward => XBUTTON2 as u32,
                    _ => 0,
                };
                if self.pressed_buttons.contains(&button) {
                    return Err(ApiError::new(
                        409,
                        codes::STATE_CONFLICT,
                        "mouse button already pressed",
                    ));
                }
                self.mouse_event(flag, 0, 0, data)?;
                self.pressed_buttons.insert(button);
                Ok(())
            }
            MouseInput::Up { button } => {
                let flag = match button {
                    MouseButton::Left => MOUSEEVENTF_LEFTUP,
                    MouseButton::Right => MOUSEEVENTF_RIGHTUP,
                    MouseButton::Middle => MOUSEEVENTF_MIDDLEUP,
                    MouseButton::Back | MouseButton::Forward => MOUSEEVENTF_XUP,
                };
                let data = match button {
                    MouseButton::Back => XBUTTON1 as u32,
                    MouseButton::Forward => XBUTTON2 as u32,
                    _ => 0,
                };
                self.mouse_event(flag, 0, 0, data)?;
                // 未按下也允许抬起：浏览器可能在会话建立前就按下了键。
                self.pressed_buttons.remove(&button);
                Ok(())
            }
            MouseInput::Wheel {
                horizontal,
                vertical,
            } => {
                if vertical != 0 {
                    self.mouse_event(
                        MOUSEEVENTF_WHEEL,
                        0,
                        0,
                        (vertical * WHEEL_DELTA as i32) as u32,
                    )?;
                }
                if horizontal != 0 {
                    self.mouse_event(
                        MOUSEEVENTF_HWHEEL,
                        0,
                        0,
                        (horizontal * WHEEL_DELTA as i32) as u32,
                    )?;
                }
                Ok(())
            }
        }
    }

    fn key(&mut self, event: KeyInput) -> Result<(), ApiError> {
        match event {
            KeyInput::Text(text) => self.send_text(&text),
            KeyInput::Layout { ch, down } => self.send_layout(ch, down),
            KeyInput::Control { code, down } => {
                if code == control_key::CAPS_LOCK || code == control_key::NUM_LOCK {
                    // 锁键只响应按下，抬起没有意义；保持与 Windows 行为一致。
                    if !down {
                        return Ok(());
                    }
                }
                let vk = Self::control_key_to_vk(code).ok_or_else(|| {
                    ApiError::new(400, codes::INVALID_REQUEST, "unsupported control key")
                })?;
                let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
                self.press_check_vk(vk, down)?;
                let flags = if down { 0 } else { KEYEVENTF_KEYUP };
                self.key_event(flags, vk, scan)
            }
        }
    }

    fn release_all(&mut self) {
        let releases: Vec<_> = self.pressed_buttons.iter().copied().collect();
        for button in releases {
            let (flag, data) = match button {
                MouseButton::Left => (MOUSEEVENTF_LEFTUP, 0),
                MouseButton::Right => (MOUSEEVENTF_RIGHTUP, 0),
                MouseButton::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
                MouseButton::Back => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
                MouseButton::Forward => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
            };
            let _ = self.mouse_event(flag, 0, 0, data);
        }
        self.pressed_buttons.clear();
        let keys: Vec<_> = self.pressed_keys.iter().copied().collect();
        for vk in keys {
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
            let _ = self.key_event(KEYEVENTF_KEYUP, vk, scan);
        }
        self.pressed_keys.clear();
    }
}

impl WinInput {
    fn press_check_vk(&mut self, vk: u16, down: bool) -> Result<(), ApiError> {
        if down {
            if self.pressed_keys.contains(&vk) {
                return Err(ApiError::new(
                    409,
                    codes::STATE_CONFLICT,
                    "key already pressed",
                ));
            }
            self.pressed_keys.insert(vk);
        } else {
            self.pressed_keys.remove(&vk);
        }
        Ok(())
    }
}

/// 注入是否可用：安全桌面/服务会话下 `SendInput` 会失败。
pub fn injection_available() -> bool {
    let (width, height) = unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    width > 0 && height > 0
}

/// 丢弃“已发送”记忆，使下一次 `poll` 重新发送形状。
///
/// 新观众加入时使用：广播不保留历史，形状可能已在订阅前发出。
fn reset_cursor_memory(cursor: &mut WinCursor) {
    cursor.delivered_shape = false;
    cursor.last_handle = 0;
}

/// 当前光标在虚拟桌面内的位置。
pub fn cursor_position() -> Option<(i32, i32)> {
    let mut point = POINT { x: 0, y: 0 };
    let ok = unsafe { GetCursorPos(&mut point) };
    if ok == 0 {
        None
    } else {
        Some((point.x, point.y))
    }
}

/// Windows 光标读取器：形状按 `hCursor` 去重，只在新形状出现时回传 RGBA。
///
/// 使用 `GetCursorInfo` 取当前句柄；`cursor_embedded` 的采集（光标画进帧里）不在本实现范围，
/// 因此形状总是单独发送。
pub struct WinCursor {
    last_handle: u64,
    last_position: Option<(i32, i32)>,
    /// 是否已经交付过形状。
    ///
    /// 形状只在句柄变化时发送；如果第一次采样没有形状（例如光标在会话建立前就已显示，
    /// 或首帧采样时桌面短暂无光标），之后不会再有“变化”，新观众将永远看不到光标。
    /// 因此第一次成功读到可见形状时必须发送。
    delivered_shape: bool,
}

impl WinCursor {
    /// 创建光标读取器。
    pub fn new() -> Self {
        Self {
            last_handle: 0,
            last_position: None,
            delivered_shape: false,
        }
    }
}

impl Default for WinCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorSource for WinCursor {
    /// 让下一次 `poll` 重新发送形状（新观众需要可绘制内容）。
    ///
    /// 必须作为 trait 方法实现：会话层持有 `&mut dyn CursorSource`，
    /// 只实现固有方法会调用到 trait 的默认空实现，形状永远不会重发。
    fn resend_shape(&mut self) {
        reset_cursor_memory(self);
    }

    fn poll(&mut self) -> Result<Option<CursorFrame>, ApiError> {
        // 形状：只在句柄变化时读取位图，避免每帧拷贝。
        let (handle, position, shape) = read_cursor_shape()?;
        let position_changed = position != self.last_position;
        // 形状需要重发时（新观众加入）也算变化，否则下面的早退会吞掉这次请求。
        let shape_changed = handle != self.last_handle || !self.delivered_shape;
        if !position_changed && !shape_changed {
            return Ok(None);
        }
        self.last_position = position;
        let Some((hotx, hoty, width, height, rgba)) = shape else {
            // 光标被隐藏：只更新位置，不发送形状。
            return Ok(Some(CursorFrame {
                id: 0,
                hotx: 0,
                hoty: 0,
                width: 0,
                height: 0,
                rgba: None,
                position,
            }));
        };
        // 首个可见形状必须带像素；否则浏览器只有位置、没有可绘制内容。
        // 注意 `last_handle` 只在这里更新：提前赋值会让 `shape_changed` 永远为假，
        // 新观众就永远收不到形状。
        let send_pixels = shape_changed || !self.delivered_shape;
        if send_pixels {
            self.delivered_shape = true;
        }
        self.last_handle = handle;
        Ok(Some(CursorFrame {
            id: handle,
            hotx,
            hoty,
            width,
            height,
            rgba: send_pixels.then_some(rgba),
            position,
        }))
    }
}

/// 光标位图：热点 + 尺寸 + 紧凑 RGBA。
type CursorBitmap = (i32, i32, u32, u32, Vec<u8>);

/// 一次光标采样：句柄、位置与（形状变化时）位图。
type CursorSample = (u64, Option<(i32, i32)>, Option<CursorBitmap>);

/// 读取当前光标句柄、位置与（可选的）RGBA 位图。
///
/// 位图通过 `GetIconInfo` + `GetDIBits` 取得；无法读取时返回 `None` 形状而不是猜测。
fn read_cursor_shape() -> Result<CursorSample, ApiError> {
    let position = cursor_position();
    let mut info: CURSORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<CURSORINFO>() as u32;
    let ok = unsafe { GetCursorInfo(&mut info) };
    if ok == 0 {
        return Ok((0, position, None));
    }
    let handle = info.hCursor as usize as u64;
    if info.flags & CURSOR_SHOWING == 0 || info.hCursor.is_null() {
        return Ok((0, position, None));
    }
    let shape = unsafe { bitmap_to_rgba(info.hCursor) };
    Ok((handle, position, shape))
}

/// 把光标位图转换为紧凑 RGBA。
///
/// SAFETY: 所有句柄都由本函数取得并在结束时删除；DIB 缓冲区按 `GetDIBits` 返回的行数读取。
unsafe fn bitmap_to_rgba(cursor: HCURSOR) -> Option<CursorBitmap> {
    if cursor.is_null() {
        return None;
    }
    let mut icon: ICONINFO = std::mem::zeroed();
    if GetIconInfo(cursor, &mut icon) == 0 {
        return None;
    }
    let _guard = IconGuard(&icon);
    let mut mask: BITMAPINFO = std::mem::zeroed();
    mask.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    // 先取尺寸：`GetDIBits` 要求位图非 NULL 才能拿到真实尺寸，因此分两步。
    let width = if !icon.hbmMask.is_null() {
        let mut bm: BITMAP = std::mem::zeroed();
        if GetObjectW(
            icon.hbmMask as *mut _,
            size_of::<BITMAP>() as i32,
            &mut bm as *mut _ as *mut _,
        ) == 0
        {
            return None;
        }
        bm.bmWidth
    } else {
        0
    };
    if width <= 0 {
        return None;
    }
    // 单色掩码高度是彩色位图两倍（XOR + AND），彩色位图高度取一半。
    let mut bm: BITMAP = std::mem::zeroed();
    if GetObjectW(
        icon.hbmMask as *mut _,
        size_of::<BITMAP>() as i32,
        &mut bm as *mut _ as *mut _,
    ) == 0
    {
        return None;
    }
    let height = if icon.hbmColor.is_null() {
        bm.bmHeight / 2
    } else {
        bm.bmHeight
    };
    if height <= 0 || width > 512 || height > 512 {
        return None;
    }
    let (w, h) = (width as u32, height as u32);
    mask.bmiHeader.biWidth = width;
    mask.bmiHeader.biHeight = -height;
    mask.bmiHeader.biPlanes = 1;
    mask.bmiHeader.biBitCount = 32;
    mask.bmiHeader.biCompression = BI_RGB;
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let dc = CreateCompatibleDC(std::ptr::null_mut());
    if dc.is_null() {
        return None;
    }
    let lines = GetDIBits(
        dc,
        icon.hbmColor,
        0,
        height as u32,
        bgra.as_mut_ptr() as *mut _,
        &mut mask,
        DIB_RGB_COLORS,
    );
    let _ = DeleteDC(dc);
    if lines == 0 {
        return None;
    }
    // 光标热点：`xHotspot/yHotspot` 相对位图左上角。
    let hotx = icon.xHotspot as i32;
    let hoty = icon.yHotspot as i32;
    // BGRA → RGBA。
    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    Some((hotx, hoty, w, h, rgba))
}

/// 保证 `ICONINFO` 里的位图句柄被释放。
struct IconGuard<'a>(&'a ICONINFO);

impl Drop for IconGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hbmMask.is_null() {
                DeleteObject(self.0.hbmMask as *mut _);
            }
            if !self.0.hbmColor.is_null() {
                DeleteObject(self.0.hbmColor as *mut _);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_mapping_is_clamped() {
        assert_eq!(WinInput::to_absolute(0, 0, 1920), Some(0));
        assert_eq!(WinInput::to_absolute(1920, 0, 1920), Some(65535));
        assert_eq!(WinInput::to_absolute(-50, 0, 1920), Some(0));
        assert_eq!(WinInput::to_absolute(5000, 0, 1920), Some(65535));
        assert_eq!(WinInput::to_absolute(5000, 0, 0), None);
    }

    #[test]
    fn control_key_mapping_follows_frozen_snapshot_numbers() {
        // 快照编号：Home=16, Insert=29, RWin=33。
        assert_eq!(
            WinInput::control_key_to_vk(control_key::HOME),
            Some(VK_HOME as u16)
        );
        assert_eq!(
            WinInput::control_key_to_vk(control_key::INSERT),
            Some(VK_INSERT as u16)
        );
        assert_eq!(
            WinInput::control_key_to_vk(control_key::R_WIN),
            Some(VK_RWIN as u16)
        );
        assert_eq!(WinInput::control_key_to_vk(control_key::UNKNOWN), None);
        assert_eq!(WinInput::control_key_to_vk(9999), None);
    }
}
