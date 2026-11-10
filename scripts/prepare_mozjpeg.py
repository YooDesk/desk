"""Prepare the pinned mozjpeg codec with portable implementations on cross targets."""
from pathlib import Path
import hashlib
import os
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = "2.2.2"
SHA256 = "ad3626d7942d5b56cc6d47b1c59724c0a976b786fca059c5aaa904aef6324d55"


def prepare():
    source = None
    for base in (Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src",
                 ROOT / ".ci-cache/cargo/registry/src"):
        source = next((item for item in base.glob(f"*/mozjpeg-sys-{VERSION}") if item.is_dir()), None)
        if source:
            break
    destination = ROOT / "artifacts/mozjpeg-engine-patched"
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        tree = Path(temporary) / "crate"
        if source:
            shutil.copytree(source, tree)
        else:
            archive = Path(temporary) / "mozjpeg.crate"
            subprocess.run(["curl", "-fL", "--retry", "3",
                            f"https://static.crates.io/crates/mozjpeg-sys/mozjpeg-sys-{VERSION}.crate",
                            "-o", str(archive)], check=True)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
                raise RuntimeError("mozjpeg archive checksum mismatch")
            with tarfile.open(archive) as bundle:
                bundle.extractall(Path(temporary), filter="data")
            tree = Path(temporary) / f"mozjpeg-sys-{VERSION}"
        for filename in ("src/build.rs",):
            target = tree / filename
            target.write_bytes(target.read_text(encoding="utf-8").encode("utf-8"))
        subprocess.run(["git", "-C", str(tree), "init", "-q"], check=True)
        payload = (ROOT / "engine-patches/mozjpeg-portable-cross-simd.patch").read_text(encoding="utf-8").encode()
        for options in (["--check"], []):
            subprocess.run(["git", "-C", str(tree), "apply", *options, "-"], input=payload, check=True)
        for original in tree.rglob("*"):
            relative = original.relative_to(tree)
            if relative.parts[0] == ".git" or not original.is_file():
                continue
            output = destination / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            if not output.is_file() or output.read_bytes() != original.read_bytes():
                shutil.copy2(original, output)
    print(f"mozjpeg-sys {VERSION} portable codec prepared at {destination}")


if __name__ == "__main__":
    prepare()
