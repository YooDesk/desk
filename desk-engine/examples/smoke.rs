//! 引擎冒烟测试：真实采集 + H.264 编码 + 输入注入。
//!
//! 运行：`cargo run --release --example smoke`

use yoodesk_desk_engine::rust_api;

fn init_logging() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
}

fn main() {
    init_logging();
    println!("version: {}", rust_api::version());
    assert_eq!(rust_api::init(), 0, "engine init failed");

    let displays = rust_api::displays();
    println!("displays: {}", displays.len());
    assert!(!displays.is_empty(), "no display");
    for (index, (width, height, x, y)) in displays.iter().enumerate() {
        println!("  index={index} {width}x{height} at {x},{y}");
    }

    assert_eq!(rust_api::subscribe(0, 1), 0, "subscribe failed");

    let started = std::time::Instant::now();
    let mut frames = 0usize;
    let mut bytes = 0usize;
    let mut keyframes = 0usize;
    while started.elapsed() < std::time::Duration::from_secs(10) && frames < 10 {
        if let Some((data, key, pts)) = rust_api::poll(2000) {
            frames += 1;
            bytes += data.len();
            if key {
                keyframes += 1;
            }
            println!("frame {frames}: {} bytes key={key} pts={pts}", data.len());
        }
    }
    println!("frames={frames} bytes={bytes} keyframes={keyframes}");
    assert!(frames > 0, "engine produced no H.264 frames");
    assert!(keyframes >= 1, "first frame must be a keyframe");

    rust_api::set_input_enabled(true);
    let rc = rust_api::mouse(0, 100, 100);
    println!("mouse inject rc={rc}");
    assert_eq!(rc, 0, "mouse injection failed");

    rust_api::unsubscribe();
    println!("ok");
}
