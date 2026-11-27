"""Reject cached FFmpeg archives missing the encoders required by a native release."""
import argparse
from pathlib import Path


def check(library: Path, platform: str):
    data = library.read_bytes()
    required = ["ff_libx264_encoder"]
    if platform in ("windows-x86", "windows-x64"):
        required += ["ff_h264_nvenc_encoder", "ff_h264_amf_encoder", "ff_h264_qsv_encoder"]
    elif platform.startswith("macos-"):
        required += ["ff_h264_videotoolbox_encoder"]
    elif platform == "linux-x64":
        required += ["ff_h264_nvenc_encoder", "ff_h264_vaapi_encoder"]
    missing = [name for name in required if name.encode("ascii") not in data]
    if missing:
        raise ValueError(f"{platform}: rebuild FFmpeg/native cache; missing: {', '.join(missing)}")
    print(f"FFmpeg encoder archive verified ({platform}): {', '.join(required)}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    parser.add_argument("platform")
    args = parser.parse_args()
    check(args.library, args.platform)
