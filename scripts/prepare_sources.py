"""Materialize pinned upstream sources inside this repository's ignored build area."""
from pathlib import Path
import json
import os
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def checkout(name, target, modules=()):
    entries = json.loads((ROOT / "deps/lock.json").read_text(encoding="utf-8"))["repositories"]
    entry = next(item for item in entries if item["name"] == name)
    target = target.resolve()
    if not target.is_relative_to(ROOT):
        raise ValueError("Build checkout must stay inside this repository")
    target.parent.mkdir(parents=True, exist_ok=True)
    if not (target / ".git").exists():
        subprocess.run(["git", "init", str(target)], check=True)
        subprocess.run(["git", "-C", str(target), "remote", "add", "origin", entry["url"]], check=True)
        subprocess.run(["git", "-C", str(target), "fetch", "--depth", "1", "origin", entry["commit"]], check=True)
        subprocess.run(["git", "-C", str(target), "checkout", "--detach", "FETCH_HEAD"], check=True)
    revision = subprocess.check_output(["git", "-C", str(target), "rev-parse", "HEAD"], text=True).strip()
    if revision != entry["commit"]:
        raise RuntimeError(f"{name}: expected {entry['commit']}, got {revision}; refusing to replace checkout")
    for module in modules:
        subprocess.run(["git", "-C", str(target), "submodule", "update", "--init", "--depth", "1", "--", module], check=True)


def main():
    tree = ROOT / "artifacts/rustdesk-engine-src"
    checkout("rustdesk", tree, ["libs/hbb_common"])
    checkout("hwcodec", ROOT / "deps/hwcodec", ["externals"])
    environment = dict(os.environ, YOODESK_RUSTDESK_DIR=str(tree))
    subprocess.run([sys.executable, str(ROOT / "scripts/build_engine.py"), "--prepare-only"], env=environment, check=True)
    if sys.platform == "win32":
        subprocess.run([sys.executable, str(ROOT / "scripts/prepare_windows_native.py"), str(tree)], check=True)


if __name__ == "__main__":
    main()
