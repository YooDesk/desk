//! 输入语义：与 `hbb.MouseEvent` / `hbb.KeyEvent` 对齐的平台无关数据结构。
//!
//! 掩码常量与 RustDesk 端侧协议一致（`mask & 0x7` 为事件类型，`mask >> 3` 为按钮），
//! 这样浏览器发来的 `MouseEvent` 可以原样映射，不需要自创第二套语义。
//!
//! ## 控制键编号以冻结快照为准
//!
//! [`control_key`] 的数值来自 `protocol/rustdesk/message-v1.proto`（由 `protocol/sources.json`
//! 固定上游提交）。注意该快照的编号与 RustDesk 当前 main **不同**（例如 `Home` 在快照中是 16，
//! 在 main 中是 21）。本产品与浏览器都按快照编号通信，平台适配层负责映射到本地虚拟键码；
//! 不得用上游最新分支的编号覆盖，否则浏览器会打出错误的键。

use crate::json::{codes, ApiError};

/// 事件类型掩码。
pub const MOUSE_TYPE_MASK: i32 = 0x7;
/// 移动。
pub const MOUSE_TYPE_MOVE: i32 = 0;
/// 按下。
pub const MOUSE_TYPE_DOWN: i32 = 1;
/// 抬起。
pub const MOUSE_TYPE_UP: i32 = 2;
/// 滚轮。
pub const MOUSE_TYPE_WHEEL: i32 = 3;
/// 触控板模式（本实现不支持）。
pub const MOUSE_TYPE_TRACKPAD: i32 = 4;
/// 相对移动。
pub const MOUSE_TYPE_MOVE_RELATIVE: i32 = 5;

/// 左键。
pub const MOUSE_BUTTON_LEFT: i32 = 0x01;
/// 右键。
pub const MOUSE_BUTTON_RIGHT: i32 = 0x02;
/// 中键。
pub const MOUSE_BUTTON_WHEEL: i32 = 0x04;
/// 后退键。
pub const MOUSE_BUTTON_BACK: i32 = 0x08;
/// 前进键。
pub const MOUSE_BUTTON_FORWARD: i32 = 0x10;

/// 相对位移上限，与端侧一致，防止极端值直接打到 OS API。
pub const MAX_RELATIVE_DELTA: i32 = 10_000;

/// 单个鼠标按钮的物理状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MouseButton {
    /// 左键。
    Left,
    /// 右键。
    Right,
    /// 中键。
    Middle,
    /// 后退。
    Back,
    /// 前进。
    Forward,
}

impl MouseButton {
    /// 由 `mask >> 3` 得到按钮；未知位返回 `None`。
    pub fn from_mask_bits(bits: i32) -> Option<Self> {
        match bits {
            MOUSE_BUTTON_LEFT => Some(Self::Left),
            MOUSE_BUTTON_RIGHT => Some(Self::Right),
            MOUSE_BUTTON_WHEEL => Some(Self::Middle),
            MOUSE_BUTTON_BACK => Some(Self::Back),
            MOUSE_BUTTON_FORWARD => Some(Self::Forward),
            _ => None,
        }
    }
}

/// 解析后的鼠标输入。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MouseInput {
    /// 绝对移动（虚拟桌面物理像素）。
    Move {
        /// X。
        x: i32,
        /// Y。
        y: i32,
    },
    /// 相对移动（像素增量，已限幅）。
    MoveRelative {
        /// ΔX。
        dx: i32,
        /// ΔY。
        dy: i32,
    },
    /// 按钮按下。
    Down {
        /// 按钮。
        button: MouseButton,
    },
    /// 按钮抬起。
    Up {
        /// 按钮。
        button: MouseButton,
    },
    /// 滚轮；`vertical` 为正表示向上滚。
    Wheel {
        /// 水平滚动量（正为向右）。
        horizontal: i32,
        /// 垂直滚动量（正为向上）。
        vertical: i32,
    },
}

/// 输入语义错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputError {
    /// 稳定的错误说明。
    pub reason: &'static str,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason)
    }
}

impl InputError {
    /// 转成 API 错误。
    pub fn into_api(self) -> ApiError {
        ApiError::new(400, codes::INVALID_REQUEST, self.reason)
    }
}

/// 把 `hbb.MouseEvent` 的 `mask/x/y` 解析为 [`MouseInput`]。
///
/// 规则与端侧一致：
/// - `MOUSE_TYPE_MOVE`：绝对坐标；
/// - `MOUSE_TYPE_DOWN/UP`：`mask >> 3` 必须是已知按钮；
/// - `MOUSE_TYPE_WHEEL`：`x/y` 为滚动量；平台适配层再换算成系统刻度；
/// - `MOUSE_TYPE_MOVE_RELATIVE`：`x/y` 为增量，限幅到 ±10000。
pub fn parse_mouse(mask: i32, x: i32, y: i32) -> Result<MouseInput, InputError> {
    let event_type = mask & MOUSE_TYPE_MASK;
    let buttons = mask >> 3;
    match event_type {
        MOUSE_TYPE_MOVE => Ok(MouseInput::Move { x, y }),
        MOUSE_TYPE_MOVE_RELATIVE => Ok(MouseInput::MoveRelative {
            dx: x.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA),
            dy: y.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA),
        }),
        MOUSE_TYPE_DOWN | MOUSE_TYPE_UP => {
            let button = MouseButton::from_mask_bits(buttons).ok_or(InputError {
                reason: "unknown mouse button",
            })?;
            Ok(if event_type == MOUSE_TYPE_DOWN {
                MouseInput::Down { button }
            } else {
                MouseInput::Up { button }
            })
        }
        MOUSE_TYPE_WHEEL => Ok(MouseInput::Wheel {
            horizontal: x,
            vertical: y,
        }),
        MOUSE_TYPE_TRACKPAD => Err(InputError {
            reason: "trackpad events are not supported",
        }),
        _ => Err(InputError {
            reason: "unknown mouse event type",
        }),
    }
}

/// 键盘输入。
///
/// 三种方式并存：
/// - [`KeyInput::Text`]：普通文本（中文、emoji 等），不依赖目标机键盘布局；
/// - [`KeyInput::Control`]：物理控制键，按 [`control_key`] 编号；
/// - [`KeyInput::Layout`]：已知布局下的字符键位，供组合键（Ctrl+C 等）使用。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeyInput {
    /// 输入一段文本。
    Text(String),
    /// 按下/抬起一个物理控制键（编号与 `hbb.ControlKey` 一致）。
    Control {
        /// `ControlKey` 枚举值。
        code: i32,
        /// 是否按下。
        down: bool,
    },
    /// 按下/抬起一个布局字符键（用于组合键）。
    Layout {
        /// 字符。
        ch: char,
        /// 是否按下。
        down: bool,
    },
}

/// HTTP 的旧控制键兼容编号。本产品页面和平台适配层使用此表；
/// RustDesk 原生消息的枚举按冻结快照定义，由引擎桥按名称转换，不能直接强制转换整数。
pub mod control_key {
    /// 未知。
    pub const UNKNOWN: i32 = 0;
    /// Alt。
    pub const ALT: i32 = 1;
    /// Backspace。
    pub const BACKSPACE: i32 = 2;
    /// CapsLock。
    pub const CAPS_LOCK: i32 = 3;
    /// Control。
    pub const CONTROL: i32 = 4;
    /// Delete。
    pub const DELETE: i32 = 5;
    /// 下方向键。
    pub const DOWN_ARROW: i32 = 6;
    /// End。
    pub const END: i32 = 7;
    /// Escape。
    pub const ESCAPE: i32 = 8;
    /// F1。
    pub const F1: i32 = 9;
    /// F10。
    pub const F10: i32 = 10;
    /// F11。
    pub const F11: i32 = 11;
    /// F12。
    pub const F12: i32 = 12;
    /// F2。
    pub const F2: i32 = 13;
    /// F3。
    pub const F3: i32 = 14;
    /// F4。
    pub const F4: i32 = 15;
    /// Home。
    pub const HOME: i32 = 16;
    /// 左方向键。
    pub const LEFT_ARROW: i32 = 17;
    /// Meta / Win / Command。
    pub const META: i32 = 18;
    /// NumLock。
    pub const NUM_LOCK: i32 = 19;
    /// PageDown。
    pub const PAGE_DOWN: i32 = 20;
    /// PageUp。
    pub const PAGE_UP: i32 = 21;
    /// Return。
    pub const RETURN: i32 = 22;
    /// 右方向键。
    pub const RIGHT_ARROW: i32 = 23;
    /// ScrollLock。
    pub const SCROLL_LOCK: i32 = 24;
    /// Shift。
    pub const SHIFT: i32 = 25;
    /// Space。
    pub const SPACE: i32 = 26;
    /// Tab。
    pub const TAB: i32 = 27;
    /// 上方向键。
    pub const UP_ARROW: i32 = 28;
    /// Insert。
    pub const INSERT: i32 = 29;
    /// 右 Control。
    pub const R_CONTROL: i32 = 30;
    /// 右 Shift。
    pub const R_SHIFT: i32 = 31;
    /// 右 Alt。
    pub const R_ALT: i32 = 32;
    /// 右 Win。
    pub const R_WIN: i32 = 33;
    /// 小键盘数字 0–9 对应连续编号 34–43。
    pub const NUMPAD_0: i32 = 34;
    /// 小键盘数字 9。
    pub const NUMPAD_9: i32 = 43;
    /// 小键盘小数点。
    pub const NUMPAD_DECIMAL: i32 = 44;
    /// 小键盘除号。
    pub const NUMPAD_DIVIDE: i32 = 45;
    /// 小键盘乘号。
    pub const NUMPAD_MULTIPLY: i32 = 46;
    /// 小键盘减号。
    pub const NUMPAD_SUBTRACT: i32 = 47;
    /// 小键盘加号。
    pub const NUMPAD_ADD: i32 = 48;
    /// 小键盘回车。
    pub const NUMPAD_ENTER: i32 = 49;
    /// 小键盘等号。
    pub const NUMPAD_EQUAL: i32 = 50;
}

/// 控制键的稳定名称，用于错误信息与日志（不含平台虚拟键码）。
///
/// 返回 `None` 表示该编号不在当前支持集合内；调用方必须明确拒绝而不是猜测。
pub fn control_key_name(code: i32) -> Option<&'static str> {
    let name = match code {
        control_key::UNKNOWN => return None,
        control_key::ALT => "Alt",
        control_key::BACKSPACE => "Backspace",
        control_key::CAPS_LOCK => "CapsLock",
        control_key::CONTROL => "Control",
        control_key::DELETE => "Delete",
        control_key::DOWN_ARROW => "DownArrow",
        control_key::END => "End",
        control_key::ESCAPE => "Escape",
        control_key::F1 => "F1",
        control_key::F2 => "F2",
        control_key::F3 => "F3",
        control_key::F4 => "F4",
        control_key::F10 => "F10",
        control_key::F11 => "F11",
        control_key::F12 => "F12",
        control_key::HOME => "Home",
        control_key::LEFT_ARROW => "LeftArrow",
        control_key::META => "Meta",
        control_key::NUM_LOCK => "NumLock",
        control_key::PAGE_DOWN => "PageDown",
        control_key::PAGE_UP => "PageUp",
        control_key::RETURN => "Return",
        control_key::RIGHT_ARROW => "RightArrow",
        control_key::SCROLL_LOCK => "ScrollLock",
        control_key::SHIFT => "Shift",
        control_key::SPACE => "Space",
        control_key::TAB => "Tab",
        control_key::UP_ARROW => "UpArrow",
        control_key::INSERT => "Insert",
        control_key::R_CONTROL => "RControl",
        control_key::R_SHIFT => "RShift",
        control_key::R_ALT => "RAlt",
        control_key::R_WIN => "RWin",
        control_key::NUMPAD_0..=control_key::NUMPAD_9 => [
            "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4", "Numpad5", "Numpad6", "Numpad7",
            "Numpad8", "Numpad9",
        ][(code - control_key::NUMPAD_0) as usize],
        control_key::NUMPAD_DECIMAL => "Decimal",
        control_key::NUMPAD_DIVIDE => "Divide",
        control_key::NUMPAD_MULTIPLY => "Multiply",
        control_key::NUMPAD_SUBTRACT => "Subtract",
        control_key::NUMPAD_ADD => "Add",
        control_key::NUMPAD_ENTER => "NumpadEnter",
        control_key::NUMPAD_EQUAL => "Equals",
        _ => return None,
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_move() {
        assert_eq!(
            parse_mouse(MOUSE_TYPE_MOVE, 10, 20),
            Ok(MouseInput::Move { x: 10, y: 20 })
        );
    }

    #[test]
    fn parses_button_with_button_bits() {
        let mask = (MOUSE_BUTTON_LEFT << 3) | MOUSE_TYPE_DOWN;
        assert_eq!(
            parse_mouse(mask, 0, 0),
            Ok(MouseInput::Down {
                button: MouseButton::Left
            })
        );
        let mask = (MOUSE_BUTTON_RIGHT << 3) | MOUSE_TYPE_UP;
        assert_eq!(
            parse_mouse(mask, 0, 0),
            Ok(MouseInput::Up {
                button: MouseButton::Right
            })
        );
    }

    #[test]
    fn rejects_unknown_button_and_trackpad() {
        let mask = (0x40 << 3) | MOUSE_TYPE_DOWN;
        assert!(parse_mouse(mask, 0, 0).is_err());
        assert!(parse_mouse(MOUSE_TYPE_TRACKPAD, 0, 0).is_err());
    }

    #[test]
    fn clamps_relative_move() {
        assert_eq!(
            parse_mouse(MOUSE_TYPE_MOVE_RELATIVE, 1_000_000, -1_000_000),
            Ok(MouseInput::MoveRelative {
                dx: MAX_RELATIVE_DELTA,
                dy: -MAX_RELATIVE_DELTA
            })
        );
    }

    #[test]
    fn parses_wheel() {
        assert_eq!(
            parse_mouse(MOUSE_TYPE_WHEEL, 1, -2),
            Ok(MouseInput::Wheel {
                horizontal: 1,
                vertical: -2
            })
        );
    }

    /// HTTP 兼容编号保持稳定；原生 RustDesk 桥按键名映射到冻结消息的枚举。
    #[test]
    fn legacy_http_control_key_numbers_are_stable() {
        let expected: &[(i32, &str)] = &[
            (1, "Alt"),
            (2, "Backspace"),
            (3, "CapsLock"),
            (4, "Control"),
            (5, "Delete"),
            (6, "DownArrow"),
            (7, "End"),
            (8, "Escape"),
            (9, "F1"),
            (10, "F10"),
            (11, "F11"),
            (12, "F12"),
            (13, "F2"),
            (14, "F3"),
            (15, "F4"),
            (16, "Home"),
            (17, "LeftArrow"),
            (18, "Meta"),
            (19, "NumLock"),
            (20, "PageDown"),
            (21, "PageUp"),
            (22, "Return"),
            (23, "RightArrow"),
            (24, "ScrollLock"),
            (25, "Shift"),
            (26, "Space"),
            (27, "Tab"),
            (28, "UpArrow"),
            (29, "Insert"),
            (30, "RControl"),
            (31, "RShift"),
            (32, "RAlt"),
            (33, "RWin"),
        ];
        for (code, name) in expected {
            assert_eq!(control_key_name(*code), Some(*name), "code {code}");
        }
        assert_eq!(control_key_name(0), None);
        assert_eq!(control_key_name(34), Some("Numpad0"));
        assert_eq!(control_key_name(43), Some("Numpad9"));
        assert_eq!(control_key_name(49), Some("NumpadEnter"));
        assert_eq!(control_key_name(51), None);
        assert_eq!(control_key_name(9999), None);
    }

    #[test]
    fn all_supported_control_codes_are_named() {
        for code in [
            control_key::ALT,
            control_key::BACKSPACE,
            control_key::CAPS_LOCK,
            control_key::CONTROL,
            control_key::DELETE,
            control_key::DOWN_ARROW,
            control_key::END,
            control_key::ESCAPE,
            control_key::F1,
            control_key::F2,
            control_key::F3,
            control_key::F4,
            control_key::F10,
            control_key::F11,
            control_key::F12,
            control_key::HOME,
            control_key::LEFT_ARROW,
            control_key::META,
            control_key::NUM_LOCK,
            control_key::PAGE_DOWN,
            control_key::PAGE_UP,
            control_key::RETURN,
            control_key::RIGHT_ARROW,
            control_key::SCROLL_LOCK,
            control_key::SHIFT,
            control_key::SPACE,
            control_key::TAB,
            control_key::UP_ARROW,
            control_key::INSERT,
            control_key::R_CONTROL,
            control_key::R_SHIFT,
            control_key::R_ALT,
            control_key::R_WIN,
        ] {
            assert!(control_key_name(code).is_some(), "code {code}");
        }
    }
}
