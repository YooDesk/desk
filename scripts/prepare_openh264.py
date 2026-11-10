"""Apply portable cross architecture support to an isolated, pinned OpenH264 crate."""
from pathlib import Path
import hashlib
import os
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.9.9"
SHA256 = "414f5bd53da84e126dc9449eb6f0003bb6994b2de63c48119b0c9b7a8b28086a"


def source():
    bases = [Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src",
             ROOT / ".ci-cache/cargo/registry/src"]
    for base in bases:
        for candidate in base.glob(f"*/openh264-sys2-{VERSION}"):
            if (candidate / "Cargo.toml").is_file():
                return candidate
    destination = ROOT / "artifacts" / f"openh264-sys2-{VERSION}"
    if not (destination / "Cargo.toml").is_file():
        with tempfile.TemporaryDirectory(prefix="openh264-crate-") as temporary:
            archive = Path(temporary) / "openh264.crate"
            subprocess.run(["curl", "-fL", "--retry", "3",
                            f"https://static.crates.io/crates/openh264-sys2/openh264-sys2-{VERSION}.crate",
                            "-o", str(archive)], check=True)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
                raise RuntimeError("OpenH264 crate checksum mismatch")
            with tarfile.open(archive) as bundle:
                bundle.extractall(destination.parent, filter="data")
    return destination


def prepare():
    destination = ROOT / "artifacts/openh264-sys2-patched"
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        tree = Path(temporary) / "crate"
        shutil.copytree(source(), tree)
        # The crates.io archive retains CRLF in build.rs. Normalize only the
        # isolated patch target so Linux applies the same strict diff as Windows.
        build_script = tree / "build.rs"
        build_script.write_bytes(build_script.read_text(encoding="utf-8").encode("utf-8"))
        subprocess.run(["git", "-C", str(tree), "init", "-q"], check=True)
        payload = (ROOT / "engine-patches/openh264-portable-cross-arches.patch").read_text(encoding="utf-8").encode()
        for options in (["--check"], []):
            subprocess.run(["git", "-C", str(tree), "apply", *options, "-"], input=payload, check=True)
        for original in tree.rglob("*"):
            relative = original.relative_to(tree)
            if relative.parts[0] == ".git" or not original.is_file():
                continue
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            if not target.is_file() or target.read_bytes() != original.read_bytes():
                shutil.copy2(original, target)
    print(f"OpenH264 {VERSION} prepared for LoongArch and MIPS64 at {destination}")


if __name__ == "__main__":
    prepare()
