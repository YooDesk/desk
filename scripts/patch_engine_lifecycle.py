"""区分嵌入引擎主动切换与真正故障；只在隔离构建副本应用。"""

from pathlib import Path


def patch_display_session(tree: Path) -> None:
    """嵌入worker使用宿主给定的会话类型；X11/Wayland/DRM仍沿原采集分派。"""
    path = tree / "libs/base/src/platform/linux.rs"
    text = path.read_text(encoding="utf-8")
    if "fn yoodesk_session_display_server(" in text:
        return
    anchor = "    // Check if `loginctl` can be called successfully\n"
    if text.count(anchor) != 1:
        raise RuntimeError("RustDesk display discovery changed; review embedded session selection")
    selected = """    // 嵌入worker绑定宿主确认的会话，避免每次显示/光标查询都fork loginctl。
    // 显式override在上方优先处理；未知会话继续原自动探测，DRM选择保持独立。
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();
    if let Some(kind) = yoodesk_session_display_server(
        std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1"),
        session_type.as_deref(),
    ) {
        return kind.to_owned();
    }

"""
    text = text.replace(anchor, selected + anchor, 1)
    text += """

fn yoodesk_session_display_server(embedded: bool, session_type: Option<&str>) -> Option<&str> {
    if !embedded { return None; }
    session_type.filter(|kind| matches!(*kind, "x11" | "wayland"))
}

#[cfg(test)]
mod yoodesk_display_session_tests {
    use super::yoodesk_session_display_server;
    #[test]
    fn declared_graphical_session_preserves_both_display_backends() {
        for kind in ["x11", "wayland"] {
            assert_eq!(yoodesk_session_display_server(true, Some(kind)), Some(kind));
        }
    }
    #[test]
    fn standalone_and_unknown_sessions_keep_original_discovery() {
        assert_eq!(yoodesk_session_display_server(false, Some("x11")), None);
        for kind in [None, Some(""), Some("tty"), Some("drm")] {
            assert_eq!(yoodesk_session_display_server(true, kind), None);
        }
    }
}
"""
    path.write_text(text, encoding="utf-8")


def patch(tree: Path) -> None:
    """SWITCH是上游正常控制信号，不能累计错误退避；真实错误仍走原逻辑。"""
    patch_display_session(tree)
    path = tree / "src/server/service.rs"
    text = path.read_text(encoding="utf-8")
    anchor = "if let Err(err) = callback(sp.clone()) {"
    addition = anchor + """
                        // 中文注释：嵌入宿主的新观众/IDR切换不是采集故障，保留30ms调度节奏。
                        if std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1")
                            && err.to_string() == "SWITCH" {
                            error_timeout = HIBERNATE_TIMEOUT;
                            thread::sleep(time::Duration::from_millis(HIBERNATE_TIMEOUT));
                            #[cfg(windows)]
                            crate::platform::windows::try_change_desktop();
                            continue;
                        }
"""
    if addition not in text:
        legacy = addition.replace(
            "                            #[cfg(windows)]\n                            crate::platform::windows::try_change_desktop();\n",
            "",
        )
        if legacy in text:
            text = text.replace(legacy, addition, 1)
            path.write_text(text, encoding="utf-8")
            return
        if text.count(anchor) != 1:
            raise RuntimeError(
                "RustDesk service lifecycle API changed; review the SWITCH adapter"
            )
        path.write_text(text.replace(anchor, addition, 1), encoding="utf-8")
