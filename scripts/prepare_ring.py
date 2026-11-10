"""Give the engine's pinned ring copy a private native symbol namespace."""
from pathlib import Path
import hashlib
import os
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.17.14"
SHA256 = "a4689e6c2294d81e88dc6261c768b63bc4fcdb852be6d1352498b114f61383b7"


def prepare():
    source = None
    for base in (Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src",
                 ROOT / ".ci-cache/cargo/registry/src"):
        source = next((item for item in base.glob(f"*/ring-{VERSION}") if item.is_dir()), None)
        if source:
            break
    destination = ROOT / "artifacts/ring-engine-patched"
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        tree = Path(temporary) / "crate"
        if source:
            shutil.copytree(source, tree)
        else:
            archive = Path(temporary) / "ring.crate"
            subprocess.run(["curl", "-fL", "--retry", "3",
                            f"https://static.crates.io/crates/ring/ring-{VERSION}.crate",
                            "-o", str(archive)], check=True)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
                raise RuntimeError("ring archive checksum mismatch")
            with tarfile.open(archive) as bundle:
                bundle.extractall(Path(temporary), filter="data")
            tree = Path(temporary) / f"ring-{VERSION}"
        for filename in ("Cargo.toml", "build.rs", "src/prefixed.rs"):
            target = tree / filename
            target.write_bytes(target.read_text(encoding="utf-8").encode("utf-8"))
        subprocess.run(["git", "-C", str(tree), "init", "-q"], check=True)
        portable = os.environ.get("CARGO_BUILD_TARGET") in (
            "loongarch64-unknown-linux-gnu", "mips64el-unknown-linux-gnuabi64")
        if portable:
            payload = (ROOT / "engine-patches/ring-engine-symbol-namespace.patch").read_text(encoding="utf-8").encode()
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
    print(f"ring {VERSION} {'private portable' if portable else 'upstream assembly'} namespace prepared at {destination}")


if __name__ == "__main__":
    prepare()
