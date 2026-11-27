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

    edit("src/server/video_qos.rs", lambda s: s.replace("MAX_FPS: u32 = 120", "MAX_FPS: u32 = 144"))

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
        text = once(text, "                        c.ram_encode = vec![];", """                        // Embedded browser sessions must retain a decodable H.264 fallback.
                        if std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1") {
                            c.ram_encode.retain(|codec| matches!(codec.name.as_str(), "libx264" | "libx265"));
                        } else {
                            c.ram_encode = vec![];
                        }""")
        return text
    edit("libs/scrap/src/common/hwcodec.rs", hardware)

    def service(text):
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
        text = once(text, '    let (mut second_instant, mut send_counter) = (Instant::now(), 0);', '    let (mut second_instant, mut send_counter) = (Instant::now(), 0);\n    let mut pipeline_metrics = yoodesk_video_metrics::Metrics::new();')
        text = once(text, '        let res = match c.frame(spf) {\n            Ok(frame) => {', '        pipeline_metrics.report(display_idx, encoder.is_hardware());\n        let capture_started = Instant::now();\n        let res = match c.frame(spf) {\n            Ok(frame) => {\n                pipeline_metrics.capture(capture_started.elapsed());')
        text = once(text, '                    let result = handle_one_frame(', '                    let encode_started = Instant::now();\n                    let result = handle_one_frame(')
        text = once(text, '                    static_refresh.on_frame_encoded(&result);', '                    pipeline_metrics.encode(encode_started.elapsed(), !result.send_conn_ids.is_empty());\n                    static_refresh.on_frame_encoded(&result);')
        module = '\nmod yoodesk_video_metrics { include!("../../yoodesk_video_metrics.rs"); }\n'
        if module not in text:
            text += module
        return text
    edit("src/server/video_service.rs", service)
    metrics = tree / "yoodesk_video_metrics.rs"
    content = (ROOT / "scripts/video_pipeline_metrics.rs").read_bytes()
    if not metrics.exists() or metrics.read_bytes() != content:
        metrics.write_bytes(content)


if __name__ == "__main__":
    import sys
    patch(Path(sys.argv[1]).resolve())
