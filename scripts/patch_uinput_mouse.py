"""Apply the isolated RustDesk absolute/relative mouse device adapter."""
from __future__ import annotations

from pathlib import Path

RESOURCE = Path(__file__).resolve().parents[1] / "engine-patches" / "uinput-mouse-split.rs"
MARKER = "    // YooDesk split absolute and relative uinput devices."


def patch_mouse(tree: Path) -> None:
    """仅替换已知mouce适配尾部；重复调用更新为同一资源，未知来源明确失败。"""
    target = tree / "src" / "server" / "uinput.rs"
    source = target.read_text(encoding="utf-8")
    anchor = MARKER if MARKER in source else "    pub struct UInputMouseManager {"
    if source.count(anchor) != 1 or "// https://github.com/emrebicer/mouce\nmod mouce {" not in source:
        raise RuntimeError(f"unsupported RustDesk uinput source: {target}")
    start = source.index(anchor)
    tail = source[start:]
    for method in ("pub fn new(", "pub fn move_to(", "pub fn move_relative(", "pub fn press_button(",
                   "pub fn release_button(", "pub fn click_button(", "pub fn scroll_wheel("):
        if method not in tail:
            raise RuntimeError(f"RustDesk uinput adapter signature changed: {method}")
    patched = source[:start] + RESOURCE.read_text(encoding="utf-8")
    if source != patched:
        # 该目录是本产品隔离构建副本；不接触deps中的参考子模块。
        target.write_text(patched, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    import sys
    patch_mouse(Path(sys.argv[1]).resolve())
