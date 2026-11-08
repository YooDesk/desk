//! 在隔离的目标用户短进程内实抓X11根窗口，排除Xwayland只返回全黑的情况。
#![allow(unsafe_code)]
use std::ffi::{c_int, c_uint, c_ulong, c_void};

#[link(name = "X11")]
extern "C" {
    fn XOpenDisplay(name: *const u8) -> *mut c_void;
    fn XDefaultRootWindow(display: *mut c_void) -> c_ulong;
    fn XGetGeometry(
        display: *mut c_void,
        drawable: c_ulong,
        root: *mut c_ulong,
        x: *mut c_int,
        y: *mut c_int,
        width: *mut c_uint,
        height: *mut c_uint,
        border: *mut c_uint,
        depth: *mut c_uint,
    ) -> c_int;
    fn XGetImage(
        display: *mut c_void,
        drawable: c_ulong,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        planes: c_ulong,
        format: c_int,
    ) -> *mut c_void;
    fn XGetPixel(image: *mut c_void, x: c_int, y: c_int) -> c_ulong;
    fn XDestroyImage(image: *mut c_void) -> c_int;
    fn XCloseDisplay(display: *mut c_void) -> c_int;
}

/// Xlib连接/协议错误只会结束本探针，宿主另设总deadline；不写图片或认证信息。
pub fn run() -> i32 {
    unsafe {
        let display = XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return 2;
        }
        let window = XDefaultRootWindow(display);
        let (mut root, mut x, mut y, mut width, mut height, mut border, mut depth) =
            (0, 0, 0, 0, 0, 0, 0);
        let geometry = XGetGeometry(
            display,
            window,
            &mut root,
            &mut x,
            &mut y,
            &mut width,
            &mut height,
            &mut border,
            &mut depth,
        );
        let mut valid = false;
        if geometry != 0
            && width > 0
            && height > 0
            && u64::from(width) * u64::from(height) <= 16_777_216
        {
            let image = XGetImage(display, window, 0, 0, width, height, !0, 2);
            if !image.is_null() {
                'rows: for row in 0..height {
                    for column in 0..width {
                        if XGetPixel(image, column as c_int, row as c_int) & 0x00ff_ffff != 0 {
                            valid = true;
                            break 'rows;
                        }
                    }
                }
                XDestroyImage(image);
            }
        }
        XCloseDisplay(display);
        if valid {
            0
        } else {
            2
        }
    }
}
