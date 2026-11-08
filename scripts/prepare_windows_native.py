"""Enable the Windows software encoders in a pinned RustDesk build worktree."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PATCH = ROOT / "engine-patches/rustdesk-windows-ffmpeg-software-encoders.patch"
ARCH_PATCH = PATCH.with_name("rustdesk-windows-target-architectures.patch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    tree = parser.parse_args().source.resolve(strict=True)
    entries = json.loads((ROOT / "dependencies.lock.json").read_text(encoding="utf-8"))["repositories"]
    locked = next(entry["commit"] for entry in entries if entry["name"] == "rustdesk")
    git = ["git", "-C", str(tree)]
    if subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip() != locked:
        raise SystemExit(f"Expected RustDesk commit {locked}")
    if tree == (ROOT / "deps/rustdesk").resolve():
        raise SystemExit("Use a separate RustDesk build worktree")
    port=tree/'res/vcpkg/ffmpeg/portfile.cmake'
    options='string(APPEND OPTIONS " --enable-libx264 --enable-libx265 --enable-encoder=libx264 --enable-encoder=libx265'
    # Normalize our whitespace fix while checking the already applied versioned patch.
    port.write_text(port.read_text(encoding='utf-8').replace(options+' ")',options+'")'),encoding='utf-8')
    if subprocess.run(git + ["apply", "--reverse", "--check", str(ARCH_PATCH)], capture_output=True).returncode:
        if subprocess.run(git + ["apply", "--reverse", "--check", str(PATCH)], capture_output=True).returncode:
            subprocess.run(git + ["apply", str(PATCH)], check=True)
        subprocess.run(git + ["apply", str(ARCH_PATCH)], check=True)
    opus_patch = PATCH.with_name("rustdesk-windows-opus-static-crt.patch")
    if subprocess.run(git + ["apply", "--reverse", "--check", str(opus_patch)], capture_output=True).returncode:
        subprocess.run(git + ["apply", str(opus_patch)], check=True)
    port.write_text(port.read_text(encoding='utf-8').replace(options+'")',options+' ")'),encoding='utf-8')
    print(f"Windows FFmpeg software encoders enabled at {tree}")


if __name__ == "__main__":
    main()
