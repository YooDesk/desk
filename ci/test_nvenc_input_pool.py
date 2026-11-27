"""Exercise actual FFmpeg frame-reference ownership without requiring a GPU."""
import os
from pathlib import Path
import re
import subprocess
import sys

def main():
    root = Path(__file__).resolve().parents[1]
    installed = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ["VCPKG_INSTALLED_ROOT"]) / os.environ["VCPKG_DEFAULT_TRIPLET"]
    output = root / "artifacts/nvenc-input-pool-test"
    output.mkdir(parents=True, exist_ok=True)
    pkg = (installed / "lib/pkgconfig/libavutil.pc").read_text(encoding="utf-8")
    libs = list(dict.fromkeys(re.findall(r"(?<!\S)-l([A-Za-z0-9_-]+)(?=\s|$)", pkg)))
    assert "avutil" in libs
    exe = output / "test.exe"
    subprocess.run(["cl.exe", "/nologo", "/EHsc", "/O2", "/MT", str(root / "ci/nvenc_input_pool_test.cpp"),
                    "/I" + str(installed / "include"), "/Fo" + str(output / "test.obj"), "/Fe" + str(exe),
                    "/link", "/LIBPATH:" + str(installed / "lib"), *[name + ".lib" for name in libs]], check=True)
    subprocess.run([str(exe)], check=True)
    print("NVENC input pool: ownership, bounded exhaustion, release and partial cleanup passed")


if __name__ == "__main__":
    main()
