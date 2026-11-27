"""Enable H.264 software fallback beside VideoToolbox in an isolated macOS build."""
import json
from pathlib import Path
import sys


def prepare(tree: Path):
    root = Path(__file__).resolve().parents[1]
    if tree.resolve() == (root / "deps/rustdesk").resolve():
        raise ValueError("Use an isolated RustDesk worktree")
    port = tree / "res/vcpkg/ffmpeg/portfile.cmake"
    text = port.read_text(encoding="utf-8")
    anchor = 'elseif(VCPKG_TARGET_IS_OSX)'
    options = '    string(APPEND OPTIONS " --enable-gpl --enable-libx264 --enable-encoder=libx264 ")'
    if options not in text:
        if text.count(anchor) != 1:
            raise ValueError("Review FFmpeg macOS platform configuration")
        port.write_text(text.replace(anchor, anchor + '\n' + options), encoding="utf-8")
    manifest = tree / "res/vcpkg/ffmpeg/vcpkg.json"
    data = json.loads(manifest.read_text(encoding="utf-8"))
    for entry in data["dependencies"]:
        if isinstance(entry, dict) and entry["name"] == "x264":
            entry["platform"] = "osx | linux | (windows & !uwp)"
    manifest.write_text(json.dumps(data, indent=2) + '\n', encoding="utf-8")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]))
