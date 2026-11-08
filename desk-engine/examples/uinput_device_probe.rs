//! Linux VM 专项：用户态成熟 uinput IPC/客户端真实移动系统指针，不伪造显示器或输入回包。
#[cfg(target_os = "linux")]
fn main() {
    librustdesk::platform::linux::dispatch_wayland_display_probe();
    if std::env::args().nth(1).as_deref() == Some("--drm-gate-only") {
        // 独立短 root 探测进程仅做真实帧支持判断；即使支持也不得开启 producer。
        assert_eq!(unsafe { hbb_common::libc::geteuid() }, 0);
        let namespace = format!(
            "{}DeskEngine-gate-{}",
            env!("PRODUCT_ID"),
            std::process::id()
        );
        std::env::set_var("YOODESK_ENGINE_NAMESPACE", &namespace);
        std::env::set_var("YOODESK_ENGINE_HELPER_UID", "1000");
        std::env::set_var("YOODESK_ENGINE_HELPER_DRM_PROBE_ONLY", "1");
        extern "C" {
            fn yd_engine_linux_privileged_init(uid: u32, gid: u32) -> i32;
        }
        let result = unsafe { yd_engine_linux_privileged_init(1000, 1000) };
        let socket = std::path::PathBuf::from(format!("/tmp/{namespace}-service/ipc_drm"));
        assert!(
            !socket.exists(),
            "probe-only must not create a persistent DRM producer"
        );
        println!(
            "{}",
            serde_json::json!({"actualDrmSupported":result==0,"producerStarted":false,"result":result})
        );
        return;
    }
    use yoodesk_desk_engine::{
        rust_api, yd_engine_key, yd_engine_mouse, yd_engine_set_input_enabled,
    };
    assert_ne!(
        unsafe { hbb_common::libc::geteuid() },
        0,
        "run as graphical user"
    );
    let namespace = format!(
        "{}DeskEngine-uinput-{}",
        env!("PRODUCT_ID"),
        std::process::id()
    );
    std::env::set_var("YOODESK_ENGINE_NAMESPACE", &namespace);
    std::env::set_var("YOODESK_ENGINE_USER_UINPUT", "1");
    std::env::set_var("YOODESK_ENGINE_DRM_ENABLED", "0");
    std::env::set_var("RUSTDESK_FORCED_DISPLAY_SERVER", "x11");
    assert_eq!(rust_api::init(), 0);
    let displays = rust_api::displays();
    let &(width, height, x, y) = displays.first().expect("real X11 display");
    assert!(width > 0 && height > 0);
    // X11 仅作为当前 VM 的真实位置观测器；此处主动安装并使用的底层是成熟 uinput。
    let target = (
        std::env::var("YOODESK_UINPUT_PROBE_X")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(x + width as i32 / 3),
        std::env::var("YOODESK_UINPUT_PROBE_Y")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(y + height as i32 / 3),
    );
    yd_engine_set_input_enabled(1);
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(yd_engine_mouse(0, target.0, target.1), 0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let output = std::process::Command::new("/usr/bin/xdotool")
        .args(["getmouselocation", "--shell"])
        .output()
        .expect("real cursor observation");
    assert!(output.status.success());
    let coordinates = String::from_utf8(output.stdout).unwrap();
    let coordinate = |key: &str| -> i32 {
        coordinates
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap()
            .parse()
            .unwrap()
    };
    let actual = (coordinate("X="), coordinate("Y="));
    assert!(
        (actual.0 - target.0).abs() <= 2 && (actual.1 - target.1).abs() <= 2,
        "uinput expected {target:?}, observed {actual:?}"
    );
    assert_eq!(yd_engine_mouse(5, 12, 10), 0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let moved = std::process::Command::new("/usr/bin/xdotool")
        .args(["getmouselocation", "--shell"])
        .output()
        .unwrap();
    let moved = String::from_utf8(moved.stdout).unwrap();
    assert!(
        moved
            .lines()
            .find_map(|line| line.strip_prefix("X="))
            .unwrap()
            .parse::<i32>()
            .unwrap()
            > actual.0,
        "relative device must move real pointer"
    );
    if std::env::var_os("YOODESK_UINPUT_PROBE_X").is_some() {
        // 仅测试夹具明确放置观察窗口时才发送按钮/轮和回车，避免点到用户窗口。
        assert_eq!(yd_engine_mouse(9, 0, 0), 0);
        assert_eq!(yd_engine_mouse(10, 0, 0), 0);
        assert_eq!(yd_engine_mouse(3, 0, 1), 0);
        assert_eq!(yd_engine_key(22, 1), 0);
        assert_eq!(yd_engine_key(22, 0), 0);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    println!(
        "{}",
        serde_json::json!({"uid":unsafe{hbb_common::libc::geteuid()},"uinputDevice":true,"expected":target,"observed":actual,"namespace":namespace})
    );
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This real device probe requires the Linux graphical VM.");
}
