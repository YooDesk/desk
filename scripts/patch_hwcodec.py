"""Prepare the pinned hwcodec source without modifying Cargo's Git cache."""
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def git(directory, *args, **kwargs):
    return subprocess.run(["git", "-C", str(directory), *args], check=True, **kwargs)


def copy_changed(source, destination):
    destination = Path(destination)
    if not destination.is_file() or Path(source).read_bytes() != destination.read_bytes():
        shutil.copy2(source, destination)
    return str(destination)


def prepare():
    source = ROOT / "deps/hwcodec"
    lock = json.loads((ROOT / "dependencies.lock.json").read_text(encoding="utf-8"))
    commit = next(entry["commit"] for entry in lock["repositories"] if entry["name"] == "hwcodec")
    revision = git(source, "rev-parse", "HEAD", capture_output=True, text=True).stdout.strip()
    if revision != commit:
        raise RuntimeError(f"Initialize deps/hwcodec at {commit}")
    archive = git(source, "archive", commit, capture_output=True).stdout
    # git archive不会展开submodule；Linux CUDA动态加载声明仍依赖其固定头文件。
    externals_commit = git(source, "rev-parse", commit + ":externals", capture_output=True, text=True).stdout.strip()
    externals = source / "externals"
    if git(externals, "rev-parse", "HEAD", capture_output=True, text=True).stdout.strip() != externals_commit:
        raise RuntimeError("Initialize hwcodec externals at pinned submodule revision " + externals_commit)
    externals_archive = git(externals, "archive", externals_commit, capture_output=True).stdout
    destination = ROOT / "artifacts/hwcodec-patched"
    destination.parent.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temp:
        tree = Path(temp)
        with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
            bundle.extractall(tree, filter="data")
        with tarfile.open(fileobj=io.BytesIO(externals_archive)) as bundle:
            bundle.extractall(tree / "externals", filter="data")
        git(tree, "init", "-q")
        patch_dir = ROOT / "engine-patches"
        patches = ["hwcodec-lowlatency.patch", "hwcodec-vcpkg-installed-root.patch"]
        if sys.platform == "win32":
            patches.append("hwcodec-windows-software-encoder-link.patch")
        if sys.platform == "linux":
            patches.insert(0, "hwcodec-linux-ffmpeg.patch")
        for patch in patches:
            # Windows检出补丁可能带CRLF，Git归档始终LF；规范补丁换行后严格检查。
            payload = (patch_dir / patch).read_text(encoding="utf-8").encode("utf-8")
            git(tree, "apply", "--check", "-", input=payload)
            git(tree, "apply", "-", input=payload)
        for source_file in tree.rglob("*"):
            relative = source_file.relative_to(tree)
            if relative.parts[0] == ".git" or not source_file.is_file():
                continue
            target_file = destination / relative
            target_file.parent.mkdir(parents=True, exist_ok=True)
            copy_changed(source_file, target_file)
    print(f"hwcodec {commit} prepared at {destination}")


if __name__ == "__main__":
    prepare()
