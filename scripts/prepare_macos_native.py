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
        if entry == "x264":
            break  # An unconditional dependency already includes macOS.
        if isinstance(entry, dict) and entry["name"] == "x264":
            platform = entry.get("platform")
            if platform and platform != "osx" and not platform.startswith("osx | ("):
                entry["platform"] = f"osx | ({platform})"
            break
    else:
        # The pinned upstream port has no x264 dependency at all on a fresh
        # macOS checkout; changing only existing Windows entries misses it.
        data["dependencies"].append({"name": "x264", "platform": "osx"})
    content = json.dumps(data, indent=2) + '\n'
    if manifest.read_text(encoding="utf-8") != content:
        manifest.write_text(content, encoding="utf-8")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]))
