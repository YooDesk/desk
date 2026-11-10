"""Materialize the patched nix crate used by webrtc-util and zbus.

nix 0.26 hard-codes the ioctl constant layout per architecture and omits
loongarch64, so the crate fails to compile for that target (unresolved
import `self::consts`). The patch adds loongarch64 to the "generic"
ioctl group, whose layout it shares.

The crate is vendored into artifacts/ (git-ignored) the same way the
patched hwcodec tree is, and [patch.crates-io] points at it.
"""
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.26.4"
DESTINATION = ROOT / "artifacts" / "nix-patched"


def crate_source() -> Path:
    """Locate the nix 0.26.4 sources in a cargo registry cache."""
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
        for candidate in base.glob(f"*/nix-{VERSION}"):
            if candidate.is_dir():
                return candidate
    raise SystemExit(
        f"nix {VERSION} sources not found; run `cargo fetch` in desk-engine first"
    )


def patch(source: Path, destination: Path) -> None:
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination)

    ioctl = destination / "src" / "sys" / "ioctl" / "linux.rs"
    text = ioctl.read_text(encoding="utf-8")
    if 'target_arch = "loongarch64"' not in text:
        # riscv64 also appears in the mips/powerpc group, which uses a
        # different ioctl layout, so anchor on the "Generic" comment.
        anchor = text.index('// "Generic" ioctl protocol')
        head, tail = text[:anchor], text[anchor:]
        marker = 'target_arch = "riscv64"\n'
        offset = tail.index(marker)
        if tail[:offset].count('mod consts') != 0:
            raise SystemExit("nix ioctl architecture list changed; review the patch")
        tail = (tail[:offset] + 'target_arch = "riscv64",\n    target_arch = "loongarch64"\n'
                + tail[offset + len(marker):])
        ioctl.write_text(head + tail, encoding="utf-8")
    print(f"nix {VERSION} patched at {destination}")


def main() -> int:
    DESTINATION.parent.mkdir(parents=True, exist_ok=True)
    patch(crate_source(), DESTINATION)
    return 0


if __name__ == "__main__":
    sys.exit(main())
