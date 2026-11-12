"""Materialize the patched nix crate used by webrtc-util and zbus.

nix 0.23/0.25/0.26 hard-code the ioctl constant layout per architecture and omit
loongarch64, so the crate fails to compile for that target (unresolved
import `self::consts`). The patch adds loongarch64 to the "generic"
ioctl group, whose layout it shares.

The crate is vendored into artifacts/ (git-ignored) the same way the
patched hwcodec tree is, and [patch.crates-io] points at it.
"""
from pathlib import Path
import hashlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VERSIONS = {
    "0.23.2": "8f3790c00a0150112de0f4cd161e3d7fc4b2d8a5542ffc35f099a2562aecb35c",
    "0.25.1": "f346ff70e7dbfd675fe90590b92d59ef2de15a8779ae305ebcbfd3f0caf59be4",
    "0.26.4": "598beaf3cc6fdd9a5dfb1630c2800c7acd31df7aaf0f565796fba2b53ca1af1b",
}


def crate_source(version: str) -> Path:
    """Materialize pinned nix sources, preferring an existing cargo cache.

    The crate is a path dependency, so it must exist before cargo resolves
    the workspace; a cold cache therefore downloads it from crates.io.
    """
    import os

    bases = []
    cargo_home = os.environ.get("CARGO_HOME")
    if cargo_home:
        bases.append(Path(cargo_home) / "registry" / "src")
    bases.append(Path.home() / ".cargo" / "registry" / "src")
    bases.append(ROOT / ".ci-cache" / "cargo" / "registry" / "src")
    for base in bases:
        if not base.is_dir():
            continue
        for candidate in base.glob(f"*/nix-{version}"):
            if candidate.is_dir():
                return candidate

    download = ROOT / "artifacts" / f"nix-{version}"
    if not (download / "Cargo.toml").is_file():
        url = f"https://static.crates.io/crates/nix/nix-{version}.crate"
        with tempfile.TemporaryDirectory(prefix="nix-crate-") as temporary:
            archive = Path(temporary) / "nix.crate"
            subprocess.run(["curl", "-fL", "--retry", "3", url, "-o", str(archive)], check=True)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != VERSIONS[version]:
                raise RuntimeError(f"nix {version}: crate checksum mismatch")
            download.parent.mkdir(parents=True, exist_ok=True)
            with tarfile.open(archive) as bundle:
                bundle.extractall(download.parent, filter="data")
    return download


def patch(source: Path, destination: Path, version: str) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for original in source.rglob("*"):
        if not original.is_file():
            continue
        target = destination / original.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        if original.relative_to(source).as_posix() == "src/sys/ioctl/linux.rs":
            continue
        if not target.is_file() or original.read_bytes() != target.read_bytes():
            shutil.copy2(original, target)
    # nix 0.26 在 lib.rs 顶层 #![deny(unused)]；较新 rustc 会在非 Linux 平台
    # 把仅 BSD/macOS 使用的辅助结构判为 dead_code，直接编译失败。
    # 只放宽 vendored 副本的该 lint，不改依赖语义。
    original_lib = source / "src" / "lib.rs"
    lib = destination / "src" / "lib.rs"
    if original_lib.is_file():
        text = original_lib.read_text(encoding="utf-8")
        if "#![deny(unused)]" in text and "// YooDesk: dead_code" not in text:
            text = text.replace(
                "#![deny(unused)]",
                "// YooDesk: dead_code in macOS/BSD-only helpers is not an error\n"
                "#![deny(unused)]\n#![allow(dead_code)]",
                1,
            )
        if not lib.is_file() or lib.read_text(encoding="utf-8") != text:
            lib.write_text(text, encoding="utf-8")
    original_ioctl = source / "src" / "sys" / "ioctl" / "linux.rs"
    ioctl = destination / "src" / "sys" / "ioctl" / "linux.rs"
    text = original_ioctl.read_text(encoding="utf-8")
    if 'target_arch = "loongarch64"' not in text:
        # riscv64 also appears in the mips/powerpc group, which uses a
        # different ioctl layout, so anchor on the "Generic" comment.
        anchor = text.index('// "Generic" ioctl protocol')
        head, tail = text[:anchor], text[anchor:]
        marker = 'target_arch = "aarch64",'
        offset = tail.index(marker)
        if tail[:offset].count('mod consts') != 0:
            raise SystemExit("nix ioctl architecture list changed; review the patch")
        tail = (tail[:offset] + marker + '\n    target_arch = "loongarch64",'
                + tail[offset + len(marker):])
        text = head + tail
    if not ioctl.is_file() or ioctl.read_text(encoding="utf-8") != text:
        ioctl.write_text(text, encoding="utf-8")
    print(f"nix {version} patched at {destination}")


def main() -> int:
    for version in VERSIONS:
        directory = "nix-patched" if version == "0.26.4" else f"nix-{version}-patched"
        patch(crate_source(version), ROOT / "artifacts" / directory, version)
    return 0


if __name__ == "__main__":
    sys.exit(main())
