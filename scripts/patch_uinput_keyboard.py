"""Give this product's virtual keyboard a name that its seat-access rule matches."""
from pathlib import Path


def patch_keyboard(tree: Path) -> None:
    target = tree / "src/server/uinput.rs"
    source = target.read_text(encoding="utf-8")
    old = '.name("RustDesk UInput Keyboard")'
    new = '.name(&std::env::var("YOODESK_ENGINE_KEYBOARD_NAME").unwrap_or_else(|_| "RustDesk UInput Keyboard".into()))'
    if old not in source and new not in source:
        raise RuntimeError("unsupported RustDesk keyboard device name")
    updated = source.replace(old, new, 1)
    if updated != source:
        target.write_text(updated, encoding="utf-8", newline="\n")
