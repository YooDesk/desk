"""Install the shared bounded frame-wait policy in an isolated engine build."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def patch(tree: Path) -> None:
    shared = (ROOT / 'scripts/frame_wait.rs').read_bytes()
    installed = tree / 'yoodesk_frame_wait.rs'
    if not installed.exists() or installed.read_bytes() != shared:
        installed.write_bytes(shared)
    source = tree / 'src/server/video_service.rs'
    original = source.read_text(encoding='utf-8')
    text = original
    if '\nmod yoodesk_frame_wait {' in text:
        text = text[:text.index('\nmod yoodesk_frame_wait {')]
    old = '\n// A slow viewer must not stop the shared embedded display'
    if old in text:
        text = text[:text.index(old)]
    start = '        let mut fetched_conn_ids = HashSet::new();\n'
    end = '        DISPLAY_CONN_IDS.lock().unwrap().remove(&display_idx);'
    if text.count(start) != 1 or text.count(end) != 1:
        raise RuntimeError('RustDesk frame wait changed; review embedded ACK isolation')
    begin = text.index(start)
    finish = text.index(end, begin)
    text = text[:begin] + '''        let mut fetched_conn_ids = HashSet::new();
        let expected = frame_controller.send_conn_ids.clone();
        yoodesk_frame_wait::wait_for_frames(
            &expected,
            &mut fetched_conn_ids,
            yoodesk_frame_wait::budget(std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1")),
            || -> ResultType<()> {
                if vs.source.is_monitor() {
                    check_privacy_mode_changed(&sp, display_idx, &c)?;
                }
                Ok(())
            },
            |fetched, timeout| frame_controller.try_wait_next(fetched, timeout),
        )?;
''' + text[finish:]
    text += '\nmod yoodesk_frame_wait {\n    include!("../../yoodesk_frame_wait.rs");\n}\n'
    if text != original:
        source.write_text(text, encoding='utf-8')
