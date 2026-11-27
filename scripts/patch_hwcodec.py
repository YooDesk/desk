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
            # Keep the pinned codec compatible with FFmpeg 7+ profile names
            # and the AVFrame flags used after key_frame was removed.
            patches.append("hwcodec-ffmpeg9-compat.patch")
        for patch in patches:
            # Windows检出补丁可能带CRLF，Git归档始终LF；规范补丁换行后严格检查。
            payload = (patch_dir / patch).read_text(encoding="utf-8").encode("utf-8")
            git(tree, "apply", "--check", "-", input=payload)
            git(tree, "apply", "-", input=payload)
        patch_vram_repeat(tree)
        patch_interactive_latency(tree)
        if sys.platform == "darwin":
            build_file = tree / "build.rs"
            text = build_file.read_text(encoding="utf-8")
            text = text.replace('let mut static_libs = vec!["avcodec", "avutil", "avformat"];',
                                'let mut static_libs = vec!["avcodec", "avutil", "avformat"];\n            if target_os == "macos" { static_libs.push("x264"); }')
            build_file.write_text(text, encoding="utf-8")
        for source_file in tree.rglob("*"):
            relative = source_file.relative_to(tree)
            if relative.parts[0] == ".git" or not source_file.is_file():
                continue
            target_file = destination / relative
            target_file.parent.mkdir(parents=True, exist_ok=True)
            copy_changed(source_file, target_file)
    print(f"hwcodec {commit} prepared at {destination}")


def patch_vram_repeat(tree):
    """Repeat the encoder-owned GPU frame, never a released DXGI texture pointer."""
    source = tree / "src/vram/encode.rs"
    text = source.read_text(encoding="utf-8")
    anchor = '    extern "C" fn callback('
    if text.count(anchor) != 1:
        raise RuntimeError("Review hwcodec VRAM callback adapter")
    text = text.replace(anchor, '''    pub fn encode_repeat(&mut self, ms: i64) -> Result<&mut Vec<EncodeFrame>, i32> {
        if self.ctx.f.driver != FFMPEG { return Err(-1); }
        self.encode(std::ptr::null_mut(), ms)
    }

''' + anchor)
    source.write_text(text, encoding="utf-8")

    source = tree / "cpp/ffmpeg_vram/ffmpeg_vram_encode.cpp"
    text = source.read_text(encoding="utf-8")
    text = text.replace('  AVFrame *frame_ = NULL;', '  AVFrame *frame_ = NULL;\n  bool has_frame_ = false;')
    old = '''    if (!convert(texture))
      return -1;

    return do_encode(callback, obj, ms);'''
    new = '''    if (texture) {
      if (!convert(texture)) return -1;
      has_frame_ = true;
    } else if (!has_frame_) {
      return -1;
    }
    return do_encode(callback, obj, ms);'''
    if text.count(old) != 1:
        raise RuntimeError("Review hwcodec GPU-frame ownership for repeat encoding")
    source.write_text(text.replace(old, new), encoding="utf-8")
    source = tree / "build.rs"
    text = source.read_text(encoding="utf-8")
    anchor = '        builder.include(mfx_path.join("windows").join("include"));'
    if text.count(anchor) != 1:
        raise RuntimeError("Review MediaSDK dispatcher linkage")
    text = text.replace(anchor, anchor + '\n        builder.file(mfx_path.join("windows/src/mfx_driver_store_loader.cpp"));')
    source.write_text(text, encoding="utf-8")


def patch_interactive_latency(tree):
    """Signal zero reordering in the bitstream, not just zero encoder queue delay."""
    source = tree / "cpp/common/util.cpp"
    text = source.read_text(encoding="utf-8")
    anchor = '    if ((ret = av_opt_set(priv_data, "delay", "0", 0)) < 0) {'
    if text.count(anchor) != 1:
        raise RuntimeError("Review NVENC interactive latency options")
    text = text.replace(anchor, '''    // WebCodecs cannot force AV_CODEC_FLAG_LOW_DELAY on its decoder. The
    // stream itself must advertise zero reorder delay, or per-frame ACK waits
    // for pictures that the decoder buffers until subsequent inputs arrive.
    if (av_opt_set(priv_data, "preset", "p1", 0) < 0 ||
        av_opt_set(priv_data, "tune", "ull", 0) < 0 ||
        av_opt_set_int(priv_data, "rc-lookahead", 0, 0) < 0 ||
        av_opt_set_int(priv_data, "zerolatency", 1, 0) < 0) {
      LOG_ERROR(std::string("NVENC interactive latency options unavailable"));
      return false;
    }
''' + anchor)
    source.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    prepare()
