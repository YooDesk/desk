"""Link the RustDesk source tree used by the optional `desk-engine` feature.

Cargo cannot read an environment variable in a path dependency, so this script
materialises `../artifacts/rustdesk-engine-src` as a directory junction (Windows) or symlink
(Unix) pointing at the checkout named by `YOODESK_RUSTDESK_DIR` (default `../rustdesk`).

It also verifies the engine patch is applied; without it the endpoint services are not
reachable from an embedding crate and the build fails with `module is private`.
"""
from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
LINK = ROOT / "artifacts/rustdesk-engine-src"
PATCH = ROOT / "engine-patches" / "rustdesk-1.5.1-engine.patch"
MARKER = "PublicService"


def rustdesk_dir() -> pathlib.Path:
    configured = os.environ.get("YOODESK_RUSTDESK_DIR")
    if configured:
        return pathlib.Path(configured).resolve()
    return (ROOT / "deps/rustdesk").resolve()


def patch_applied(tree: pathlib.Path) -> bool:
    server = tree / "src" / "server.rs"
    if not server.exists():
        return False
    return MARKER in server.read_text(encoding="utf-8", errors="replace")


def link_tree(target: pathlib.Path) -> None:
    LINK.parent.mkdir(parents=True, exist_ok=True)
    if LINK.exists() or LINK.is_symlink():
        current = LINK.resolve() if not LINK.is_symlink() else pathlib.Path(os.readlink(LINK))
        if current == target:
            return
        print(f"removing stale link {LINK} -> {current}")
        if LINK.is_symlink():
            LINK.unlink()
        else:
            subprocess.run(["cmd", "/c", "rmdir", str(LINK)], check=True)
    if os.name == "nt":
        subprocess.run(["cmd", "/c", "mklink", "/J", str(LINK), str(target)], check=True)
    else:
        LINK.symlink_to(target, target_is_directory=True)
    print(f"linked {LINK} -> {target}")


def main() -> int:
    tree = rustdesk_dir()
    if not (tree / "Cargo.toml").exists():
        print(f"RustDesk tree not found: {tree}", file=sys.stderr)
        print("Set YOODESK_RUSTDESK_DIR to the checkout, or place it at ../rustdesk")
        return 1
    if not patch_applied(tree):
        print(
            f"engine patch is not applied in {tree}.\n"
            f"Apply it first:  git -C {tree} apply {PATCH}",
            file=sys.stderr,
        )
        return 1
    link_tree(tree)
    print("ok: desk-engine dependencies are reachable")
    return 0


if __name__ == "__main__":
    sys.exit(main())
