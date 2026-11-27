"""Install the embedded hardware-first policy in the isolated RustDesk worktree."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def patch(tree: Path) -> None:
    def edit(relative, transform):
        path = tree / relative
        old = path.read_text(encoding="utf-8")
        new = transform(old)
        if new != old:
            path.write_text(new, encoding="utf-8")

    def once(text, old, new):
        if new in text:
            return text
        if text.count(old) != 1:
            raise RuntimeError("RustDesk video pipeline changed: " + old[:90])
        return text.replace(old, new, 1)

    def qos(text):
        text = text.replace("MAX_FPS: u32 = 120", "MAX_FPS: u32 = 144")
        anchor = "    // Calculate seconds per frame based on current FPS\n"
        return once(text, anchor, '''    // Codec timing/level selection uses the configured target, not the temporary
    // low-FPS startup guard. Capture pacing remains controlled by adaptive fps().
    pub fn encoder_fps(&self) -> u32 {
        self.users.values().map(UserData::fps_cap).max().unwrap_or_else(|| self.fps())
    }

''' + anchor)
    edit("src/server/video_qos.rs", qos)

    def codec(text):
        # Keep the H.264 preference after a failed GPU. clear() preserves software.
        old = "                    *ENCODE_CODEC_FORMAT.lock().unwrap() = CodecFormat::VP9;"
        new = """                    if std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() != Ok("1") {
                        *ENCODE_CODEC_FORMAT.lock().unwrap() = CodecFormat::VP9;
                    }"""
        if new not in text:
            if text.count(old) != 2:
                raise RuntimeError("Review upstream hardware creation fallback")
            text = text.replace(old, new)
        return text
    edit("libs/scrap/src/common/codec.rs", codec)

    def hardware(text):
        text = once(text, '    pub keyframe_interval: Option<usize>,\n}\n\npub struct HwRamEncoder', '    pub keyframe_interval: Option<usize>,\n    pub fps: u32,\n}\n\npub struct HwRamEncoder')
        text = once(text, '                    fps: DEFAULT_FPS,', '                    fps: config.fps.clamp(1, 144) as i32,')
        text = once(text, "                        c.ram_encode = vec![];", """                        // Embedded browser sessions must retain a decodable H.264 fallback.
                        if std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1") {
                            c.ram_encode.retain(|codec| matches!(codec.name.as_str(), "libx264" | "libx265"));
                        } else {
                            c.ram_encode = vec![];
                        }""")
        return text
    edit("libs/scrap/src/common/hwcodec.rs", hardware)

    def vram(text):
        text = once(text, '    pub keyframe_interval: Option<usize>,\n}\n\npub struct VRamEncoder', '    pub keyframe_interval: Option<usize>,\n    pub fps: u32,\n}\n\npub struct VRamEncoder')
        return once(text, '                        framerate: 30,', '                        framerate: config.fps.clamp(1, 144) as i32,')
    edit("libs/scrap/src/common/vram.rs", vram)

    def service(text):
        text = once(text, '// replacement DXGI capturer time to produce its first frame. Normal startup is unaffected.', '// replacement DXGI capturer time to produce its first frame. Embedded startup uses the same grace.')
        text = once(text, '''        .take_restart_pending()
        .then(Instant::now);''', '''        .take_restart_pending()
        .then(Instant::now)
        // Four empty polls at high FPS are not evidence of a broken capturer.
        // Give a fresh embedded DXGI capturer the same bounded first-frame
        // grace as an ACCESS_LOST replacement; real errors still fall back.
        .or_else(|| (std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1")).then(Instant::now));''')
        start = text.index("pub fn register_software_encoders() {")
        end = text.index("\n}\n", start) + 3
        replacement = '''pub fn register_software_encoders() {
    // Hardware probing is done by the embedding SDK. Append software rather than
    // replacing its result; lower numeric priorities win in CodecInfo::prioritized.
    #[cfg(feature = "hwcodec")]
    {
        use scrap::raw_hwcodec::ffmpeg_ram::{CodecInfo, Priority};
        let mut config = scrap::hwcodec::HwCodecConfig::get();
        let soft = CodecInfo::soft();
        for (codec, name) in [(soft.h264, "libx264"), (soft.h265, "libx265")] {
            if !config.ram_encode.iter().any(|c| c.name == name) {
                if let Some(mut codec) = codec {
                    codec.name = name.to_owned();
                    codec.priority = Priority::Bad as i32 + 1;
                    config.ram_encode.push(codec);
                }
            }
        }
        scrap::hwcodec::HwCodecConfig::set(serde_json::to_string(&config).unwrap_or_default());
        scrap::codec::Encoder::update(scrap::codec::EncodingUpdate::Check);
    }
}
'''
        text = text[:start] + replacement + text[end:]
        text = once(text, '                    quality,\n                    feature,', '                    quality,\n                    fps: VIDEO_QOS.lock().unwrap().encoder_fps(),\n                    feature,')
        text = once(text, '                    quality,\n                    keyframe_interval,\n                });', '                    quality,\n                    fps: VIDEO_QOS.lock().unwrap().encoder_fps(),\n                    keyframe_interval,\n                });')
        text = once(text, '    let (mut second_instant, mut send_counter) = (Instant::now(), 0);', '    let (mut second_instant, mut send_counter) = (Instant::now(), 0);\n    let mut pipeline_metrics = yoodesk_video_metrics::Metrics::new();')
        text = once(text, '    let mut pipeline_metrics = yoodesk_video_metrics::Metrics::new();', '    let mut pipeline_metrics = yoodesk_video_metrics::Metrics::new();\n    let mut frame_pacer = yoodesk_video_pacing::Pacer::new();')
        text = once(text, '        let res = match c.frame(spf) {\n            Ok(frame) => {', '        pipeline_metrics.report(display_idx, encoder.is_hardware());\n        let capture_started = Instant::now();\n        let res = match c.frame(spf) {\n            Ok(frame) => {\n                pipeline_metrics.capture(capture_started.elapsed());')
        text = once(text, '                    let result = handle_one_frame(', '                    let encode_started = Instant::now();\n                    let result = handle_one_frame(')
        text = once(text, '                    static_refresh.on_frame_encoded(&result);', '                    pipeline_metrics.encode(encode_started.elapsed(), !result.send_conn_ids.is_empty());\n                    static_refresh.on_frame_encoded(&result);')
        module = '\nmod yoodesk_video_metrics { include!("../../yoodesk_video_metrics.rs"); }\n'
        if module not in text:
            text += module
        pacing = '\nmod yoodesk_video_pacing { include!("../../yoodesk_video_pacing.rs"); }\n'
        if pacing not in text:
            text += pacing
        text = once(text, '        if elapsed < spf {\n            std::thread::sleep(spf - elapsed);\n        }', '        frame_pacer.wait(now, spf, std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1"));')
        return text
    edit("src/server/video_service.rs", service)
    def test_configs(text):
        old = '                quality: BR_BALANCED,\n                keyframe_interval: None,'
        new = '                quality: BR_BALANCED,\n                keyframe_interval: None,\n                fps: 30,'
        if new not in text:
            if text.count(old) != 2:
                raise RuntimeError("Review static-refresh encoder test configuration")
            text = text.replace(old, new)
        return text
    edit("src/server/video_service/static_refresh.rs", test_configs)
    edit("libs/scrap/examples/benchmark.rs", lambda text: once(text, 'EncoderCfg::HWRAM(HwRamEncoderConfig {', 'EncoderCfg::HWRAM(HwRamEncoderConfig {\n                fps: 30,'))
    metrics = tree / "yoodesk_video_metrics.rs"
    content = (ROOT / "scripts/video_pipeline_metrics.rs").read_bytes()
    if not metrics.exists() or metrics.read_bytes() != content:
        metrics.write_bytes(content)
    pacing = tree / "yoodesk_video_pacing.rs"
    content = (ROOT / "scripts/video_pipeline_pacing.rs").read_bytes()
    if not pacing.exists() or pacing.read_bytes() != content:
        pacing.write_bytes(content)


if __name__ == "__main__":
    import sys
    patch(Path(sys.argv[1]).resolve())
