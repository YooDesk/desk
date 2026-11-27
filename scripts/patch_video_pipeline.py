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
        text = once(text, '        if repeated && frames.is_empty() {', '        if frames.is_empty() {')
        text = once(text, '    fn latency_free(&self) -> bool {\n        true\n    }', '''    fn latency_free(&self) -> bool {
        !(self.ctx.f.driver == Driver::FFMPEG && self.ctx.f.vendor == Driver::NV
            && self.ctx.f.data_format == DataFormat::H264 && self.ctx.d.framerate > 60)
    }''')
        return once(text, '                        framerate: 30,', '                        framerate: config.fps.clamp(1, 144) as i32,')
    edit("libs/scrap/src/common/vram.rs", vram)

    def service(text):
        text = once(text, '    send_conn_ids: HashSet<i32>,\n}', '    send_conn_ids: HashSet<i32>,\n    wait_runtime: tokio::runtime::Runtime,\n}')
        text = once(text, '            send_conn_ids: HashSet::new(),\n        }', '''            send_conn_ids: HashSet::new(),
            wait_runtime: tokio::runtime::Builder::new_current_thread().enable_all()
                .build().expect("capture frame-wait runtime"),
        }''')
        if '    async fn wait_next(display_idx: usize,' not in text:
            begin = text.index('    #[tokio::main(flavor = "current_thread")]\n    async fn try_wait_next')
            end = text.index('\n}\n\n#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]', begin)
            wait = text[begin:end]
            wait = once(wait, '''    #[tokio::main(flavor = "current_thread")]
    async fn try_wait_next(&mut self, fetched_conn_ids: &mut HashSet<i32>, timeout_millis: u64) {
        if self.send_conn_ids.is_empty() {
            return;
        }
''', '''    fn try_wait_next(&mut self, fetched_conn_ids: &mut HashSet<i32>, timeout_millis: u64) {
        if self.send_conn_ids.is_empty() { return; }
        self.wait_runtime.block_on(Self::wait_next(self.display_idx, fetched_conn_ids, timeout_millis));
    }

    async fn wait_next(display_idx: usize, fetched_conn_ids: &mut HashSet<i32>, timeout_millis: u64) {
''')
            wait = once(wait, '.get(&self.display_idx)', '.get(&display_idx)')
            text = text[:begin] + wait + text[end:]
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
        if '    let mut frame_pacer = ' not in text:
            text = once(text, '    let mut pipeline_metrics = yoodesk_video_metrics::Metrics::new();', '    let mut pipeline_metrics = yoodesk_video_metrics::Metrics::new();\n    let mut frame_pacer = yoodesk_video_pacing::Pacer::new();')
        text = once(text, '    let mut frame_pacer = yoodesk_video_pacing::Pacer::new();', '    let mut frame_pacer = yoodesk_video_pacing::Pacer::new(VIDEO_QOS.lock().unwrap().encoder_fps() > 60);')
        text = once(text, '    while sp.ok() {\n        #[cfg(windows)]', '    while sp.ok() {\n        let control_started = Instant::now();\n        #[cfg(windows)]')
        text = once(text, '        let res = match c.frame(spf) {\n            Ok(frame) => {', '        pipeline_metrics.report(display_idx, encoder.is_hardware());\n        let capture_started = Instant::now();\n        let res = match c.frame(spf) {\n            Ok(frame) => {\n                pipeline_metrics.capture(capture_started.elapsed());')
        text = once(text, '        pipeline_metrics.report(display_idx, encoder.is_hardware());', '        pipeline_metrics.control(control_started.elapsed());\n        pipeline_metrics.report(display_idx, encoder.is_hardware());')
        text = once(text, '            Err(ref e) if e.kind() == WouldBlock => {', '            Err(ref e) if e.kind() == WouldBlock => {\n                pipeline_metrics.empty();')
        text = once(text, '        yoodesk_frame_wait::wait_for_frames(', '        let frame_wait_started = Instant::now();\n        yoodesk_frame_wait::wait_for_frames(')
        text = once(text, '        DISPLAY_CONN_IDS.lock().unwrap().remove(&display_idx);', '        pipeline_metrics.wait(frame_wait_started.elapsed());\n        DISPLAY_CONN_IDS.lock().unwrap().remove(&display_idx);')
        text = once(text, '                    let result = handle_one_frame(', '                    let encode_started = Instant::now();\n                    let result = handle_one_frame(')
        text = once(text, '                    static_refresh.on_frame_encoded(&result);', '                    pipeline_metrics.encode(encode_started.elapsed(), !result.send_conn_ids.is_empty());\n                    static_refresh.on_frame_encoded(&result);')
        if 'send_conn_ids: HashSet::new(), vpx_no_output: false,\n' in text:
            text = text.replace('send_conn_ids: HashSet::new(), vpx_no_output: false,\n', 'send_conn_ids: HashSet::new(), vpx_no_output: false, buffered_input: true,\n', 1)
        text = once(text, '        Ok(mut vf) => {\n            *encode_fail_counter = 0;', '''        Ok(mut vf) => {
            match yoodesk_video_output::observe(vf.union.is_some(), encode_fail_counter) {
                yoodesk_video_output::Output::Buffered => return Ok(static_refresh::FrameEncodeResult {
                    send_conn_ids: HashSet::new(), vpx_no_output: false, buffered_input: true,
                }),
                yoodesk_video_output::Output::Stalled => {
                    if encoder.is_hardware() { encoder.disable(); }
                    bail!("SWITCH");
                }
                yoodesk_video_output::Output::Ready => {}
            }
            *encode_fail_counter = 0;''')
        text = once(text, '        send_conn_ids,\n        vpx_no_output,\n    })', '        send_conn_ids,\n        vpx_no_output,\n        buffered_input: false,\n    })')
        module = '\nmod yoodesk_video_metrics { include!("../../yoodesk_video_metrics.rs"); }\n'
        if module not in text:
            text += module
        pacing = '\nmod yoodesk_video_pacing { include!("../../yoodesk_video_pacing.rs"); }\n'
        if pacing not in text:
            text += pacing
        output = '\nmod yoodesk_video_output { include!("../../yoodesk_video_output.rs"); }\n'
        if output not in text:
            text += output
        if 'Ok("1"), source_rate);' not in text:
            text = once(text, '        if elapsed < spf {\n            std::thread::sleep(spf - elapsed);\n        }', '        frame_pacer.wait(now, spf, std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1"));')
        text = once(text, '        frame_pacer.wait(now, spf, std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1"));', '''        #[cfg(all(windows, feature = "vram"))]
        let source_rate = if encoder.input_texture() { c.refresh_rate() } else { None };
        #[cfg(not(all(windows, feature = "vram")))]
        let source_rate = None;
        frame_pacer.wait(now, spf, std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1"), source_rate);''')
        return text
    edit("src/server/video_service.rs", service)
    def dxgi(text):
        old = 'wrap_hresult((*self.duplication.0).AcquireNextFrame(timeout, &mut info, &mut frame))?;'
        new = '''let capture_result = (*self.duplication.0).AcquireNextFrame(timeout, &mut info, &mut frame);
            if std::env::var_os("YOODESK_DXGI_DIAGNOSTICS").is_some() {
                static SAMPLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                if SAMPLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 16 {
                    if capture_result >= 0 {
                        hbb_common::log::info!("DXGI capture: hr={:#x} timeout_ms={} accumulated={} present={} mouse={}",
                            capture_result, timeout, info.AccumulatedFrames,
                            *info.LastPresentTime.QuadPart(), *info.LastMouseUpdateTime.QuadPart());
                    } else {
                        hbb_common::log::info!("DXGI capture: hr={:#x} timeout_ms={}", capture_result, timeout);
                    }
                }
            }
            wrap_hresult(capture_result)?;'''
        if new not in text:
            if text.count(old) != 2:
                raise RuntimeError("Review DXGI frame acquisition diagnostics")
            text = text.replace(old, new)
        return text
    edit("libs/scrap/src/dxgi/mod.rs", dxgi)
    def initial_snapshot(text):
        begin = text.index('    fn get_texture(&mut self, timeout: UINT)')
        end = text.index('    fn release_frame(&mut self)', begin)
        texture = text[begin:end]
        texture = once(texture, '            if info.AccumulatedFrames == 0 || *info.LastPresentTime.QuadPart() == 0 {', '''            // The first successful acquisition includes a desktop snapshot,
            // even if no presentation occurred after duplication began.
            let initial_snapshot = self.texture.is_null() && !frame.is_null()
                && std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1");
            if (info.AccumulatedFrames == 0 || *info.LastPresentTime.QuadPart() == 0) && !initial_snapshot {''')
        return text[:begin] + texture + text[end:]
    edit("libs/scrap/src/dxgi/mod.rs", initial_snapshot)
    def refresh_clock(text):
        text = once(text, '    fastlane: bool,', '    fastlane: bool,\n    source_refresh_rate: Option<f64>,')
        text = once(text, '            surface: ComPtr(ptr::null_mut()),', '''            source_refresh_rate: if desc.ModeDesc.RefreshRate.Denominator > 0 && desc.ModeDesc.RefreshRate.Numerator > 0 {
                Some(desc.ModeDesc.RefreshRate.Numerator as f64 / desc.ModeDesc.RefreshRate.Denominator as f64)
            } else { None },
            surface: ComPtr(ptr::null_mut()),''')
        return once(text, '    pub fn is_gdi(&self) -> bool {', '''    pub fn source_refresh_rate(&self) -> Option<f64> {
        if self.is_gdi() { None } else { self.source_refresh_rate }
    }

    pub fn is_gdi(&self) -> bool {''')
    edit("libs/scrap/src/dxgi/mod.rs", refresh_clock)
    edit("libs/scrap/src/common/mod.rs", lambda text: once(text, 'pub trait TraitCapturer {', '''pub trait TraitCapturer {
    #[cfg(all(windows, feature = "vram"))]
    fn refresh_rate(&self) -> Option<f64> { None }'''))
    def capture_timeout(text):
        text = once(text, 'match self.inner.frame(timeout.as_millis() as _) {', 'match self.inner.frame(yoodesk_capture_timeout::millis(timeout)) {')
        module = '\nmod yoodesk_capture_timeout { include!("../../../../yoodesk_capture_timeout.rs"); }\n'
        text = once(text, '    fn is_gdi(&self) -> bool {\n        self.inner.is_gdi()\n    }', '''    #[cfg(feature = "vram")]
    fn refresh_rate(&self) -> Option<f64> { self.inner.source_refresh_rate() }

    fn is_gdi(&self) -> bool {
        self.inner.is_gdi()
    }''')
        return text if module in text else text + module
    edit("libs/scrap/src/common/dxgi.rs", capture_timeout)
    def test_configs(text):
        text = once(text, '    pub(super) vpx_no_output: bool,', '    pub(super) vpx_no_output: bool,\n    pub(super) buffered_input: bool,')
        text = once(text, '    source_ready: bool,', '    source_ready: bool,\n    warming_up: bool,')
        text = once(text, '            source_ready: false,', '            source_ready: false,\n            warming_up: false,')
        text = once(text, '    pub(super) fn on_frame_encoded(&mut self, result: &FrameEncodeResult) {', '''    pub(super) fn on_frame_encoded(&mut self, result: &FrameEncodeResult) {
        if result.buffered_input {
            // The owned input is valid for a repeat, but no video was sent.
            self.on_encoded(false);
            self.source_ready = true;
            self.warming_up = true;
            return;
        }''')
        text = once(text, '        if success {\n            self.source_ready = true;', '        if success {\n            self.warming_up = false;\n            self.source_ready = true;')
        text = once(text, '        let spf = spf.max(Duration::from_secs(1) / MAX_REPEAT_FPS);', '        let requested_spf = spf;\n        let spf = spf.max(Duration::from_secs(1) / MAX_REPEAT_FPS);')
        text = once(text, '                let interval = if matches!(self.codec_format, CodecFormat::VP8 | CodecFormat::VP9)', '''                #[cfg(all(windows, feature = "vram"))]
                let drain_texture = self.has_texture && !encoder.latency_free() && self.repeat_output_counter == 0;
                #[cfg(not(all(windows, feature = "vram")))]
                let drain_texture = false;
                let interval = if drain_texture { requested_spf }
                else if matches!(self.codec_format, CodecFormat::VP8 | CodecFormat::VP9)''')
        text = once(text, '                if sps.has_subscribes() {', '                if sps.has_subscribes() && !self.warming_up {')
        text = once(text, '                Ok(vf) if vf.union.is_none() => {\n                    self.repeat_no_outputs += 1;', '''                Ok(vf) if vf.union.is_none() => {
                    self.repeat_no_outputs += 1;
                    if self.warming_up && self.repeat_no_outputs >= 3 {
                        if encoder.is_hardware() { encoder.disable(); }
                        bail!("SWITCH");
                    }''')
        text = once(text, '            self.repeat_output_counter += 1;\n            self.repeat_failures = 0;', '            self.repeat_output_counter += 1;\n            self.warming_up = false;\n            self.repeat_failures = 0;')
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
    output = tree / "yoodesk_video_output.rs"
    content = (ROOT / "scripts/video_pipeline_output.rs").read_bytes()
    if not output.exists() or output.read_bytes() != content:
        output.write_bytes(content)
    timeout = tree / "yoodesk_capture_timeout.rs"
    content = (ROOT / "scripts/video_capture_timeout.rs").read_bytes()
    if not timeout.exists() or timeout.read_bytes() != content:
        timeout.write_bytes(content)
    priority = tree / "video_capture_priority.rs"
    content = (ROOT / "scripts/video_capture_priority.rs").read_bytes()
    if not priority.exists() or priority.read_bytes() != content:
        priority.write_bytes(content)


if __name__ == "__main__":
    import sys
    patch(Path(sys.argv[1]).resolve())
