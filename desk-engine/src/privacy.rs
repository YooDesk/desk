//! 普通 Windows 桌面隐私屏：本进程窗口，排除采集，不注入其它进程。
//! 所有 HWND 与低级输入钩子属于同一专用线程；退出必定销毁并解除钩子。
use std::{
    ptr,
    sync::{
        atomic::{AtomicI32, Ordering},
        mpsc, Mutex,
    },
    thread,
};
use winapi::{
    shared::{
        minwindef::{LPARAM, LRESULT, WPARAM},
        windef::{HHOOK, HWND},
    },
    um::{
        errhandlingapi::GetLastError, libloaderapi::GetModuleHandleW,
        processthreadsapi::GetCurrentThreadId, wingdi::*, winuser::*,
    },
};

static OWNER: AtomicI32 = AtomicI32::new(0);
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
#[repr(C)]
struct Version {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}
#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(version: *mut Version) -> i32;
}
pub fn supported() -> bool {
    // VerifyVersionInfo 依赖 exe 的兼容性 manifest；RtlGetVersion 返回真实系统版本。
    let mut version = Version {
        size: std::mem::size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    unsafe { RtlGetVersion(&mut version) == 0 && version.major >= 10 && version.build >= 19041 }
}
struct Active {
    conn: i32,
    thread_id: u32,
    worker: thread::JoinHandle<()>,
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
pub fn enabled_for(conn: i32) -> bool {
    OWNER.load(Ordering::SeqCst) == conn
}

pub fn set(conn: i32, enabled: bool) -> Result<(), String> {
    let mut active = ACTIVE.lock().unwrap();
    if let Some(current) = active.as_ref() {
        if current.worker.is_finished() {
            active.take();
        }
    }
    if let Some(current) = active.as_ref() {
        if current.conn != conn {
            return Err("privacy screen occupied by another session".to_owned());
        }
        if enabled {
            return Ok(());
        }
    }
    if !enabled {
        if let Some(current) = active.take() {
            if unsafe { PostThreadMessageW(current.thread_id, WM_QUIT, 0, 0) } == 0
                && !current.worker.is_finished()
            {
                *active = Some(current);
                return Err(format!("privacy shutdown failed: {}", unsafe {
                    GetLastError()
                }));
            }
            current
                .worker
                .join()
                .map_err(|_| "privacy thread failed".to_owned())?;
        }
        return Ok(());
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || unsafe {
        match Screen::new() {
            Ok(screen) => {
                OWNER.store(conn, Ordering::SeqCst);
                let _ = tx.send(Ok(GetCurrentThreadId()));
                let mut msg = std::mem::zeroed();
                while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                drop(screen);
                OWNER.store(0, Ordering::SeqCst);
            }
            Err(error) => {
                let _ = tx.send(Err(error));
            }
        }
    });
    let thread_id = rx
        .recv()
        .map_err(|_| "privacy thread unavailable".to_owned())??;
    *active = Some(Active {
        conn,
        thread_id,
        worker,
    });
    Ok(())
}

pub fn close_for(conn: i32) -> Result<(), String> {
    if enabled_for(conn) {
        set(conn, false)
    } else {
        Ok(())
    }
}

struct Screen {
    window: HWND,
    keyboard: HHOOK,
    mouse: HHOOK,
}
impl Screen {
    unsafe fn new() -> Result<Self, String> {
        let class = wide("DeskPrivacyWindow");
        let module = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: GetStockObject(BLACK_BRUSH as i32) as _,
            lpszMenuName: ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 && GetLastError() != 1410 {
            return Err(format!("privacy register window: {}", GetLastError()));
        }
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            class.as_ptr(),
            wide(&format!("{} 隐私屏", env!("PRODUCT_DISPLAY_NAME"))).as_ptr(),
            WS_POPUP,
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null_mut(),
        );
        if window.is_null() {
            return Err(format!("privacy create window: {}", GetLastError()));
        }
        let mut screen = Screen {
            window,
            keyboard: ptr::null_mut(),
            mouse: ptr::null_mut(),
        };
        // 不透明的分层遮罩在本地仍可见，但命中测试穿透到其它进程；远端注入继续由底层窗口接收。
        if SetLayeredWindowAttributes(window, 0, 255, LWA_ALPHA) == 0 {
            return Err(format!("privacy layered window: {}", GetLastError()));
        }
        if SetWindowDisplayAffinity(window, 0x11) == 0 {
            return Err(format!("privacy exclude capture: {}", GetLastError()));
        }
        screen.keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
        if screen.keyboard.is_null() {
            return Err(format!("privacy keyboard hook: {}", GetLastError()));
        }
        screen.mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        if screen.mouse.is_null() {
            return Err(format!("privacy mouse hook: {}", GetLastError()));
        }
        ShowWindow(window, SW_SHOWNOACTIVATE);
        if SetTimer(window, 1, 500, None) == 0 {
            return Err(format!("privacy refresh timer: {}", GetLastError()));
        }
        if IsWindowVisible(window) == 0 {
            return Err("privacy window not visible".to_owned());
        }
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        OWNER.store(0, Ordering::SeqCst);
        unsafe {
            if !self.keyboard.is_null() {
                UnhookWindowsHookEx(self.keyboard);
            }
            if !self.mouse.is_null() {
                UnhookWindowsHookEx(self.mouse);
            }
            if !self.window.is_null() {
                DestroyWindow(self.window);
            }
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let event = &*(lparam as *const KBDLLHOOKSTRUCT);
        if event.flags & LLKHF_INJECTED == 0 {
            // 本地 Escape 是恢复入口；远端注入的 Escape 不关闭遮罩。
            if event.vkCode == VK_ESCAPE as u32
                && (wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize)
            {
                PostThreadMessageW(GetCurrentThreadId(), WM_QUIT, 0, 0);
            }
            return 1;
        }
    }
    CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
}
unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (*(lparam as *const MSLLHOOKSTRUCT)).flags & LLMHF_INJECTED == 0 {
        return 1;
    }
    CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
}
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_TIMER | WM_DISPLAYCHANGE => {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
                SWP_NOACTIVATE,
            );
            0
        }
        WM_PAINT => {
            let mut paint = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = std::mem::zeroed();
            GetClientRect(hwnd, &mut rect);
            SetTextColor(dc, 0x00ffffff);
            SetBkMode(dc, TRANSPARENT as i32);
            let text = wide(&format!(
                "{} 远程会话中 · 本地按 Esc 恢复",
                env!("PRODUCT_DISPLAY_NAME")
            ));
            DrawTextW(
                dc,
                text.as_ptr(),
                -1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            EndPaint(hwnd, &paint);
            0
        }
        WM_CLOSE => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
