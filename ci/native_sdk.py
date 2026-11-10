"""Package and verify immutable, reusable cross-platform native library SDKs."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"linux-loong64": "loongarch64-unknown-linux-gnu",
           "linux-mips64el": "mips64el-unknown-linux-gnuabi64"}
VERSION = "v1"
OPENH264_SHA = "414f5bd53da84e126dc9449eb6f0003bb6994b2de63c48119b0c9b7a8b28086a"
DRM_REVISION = "5936a7a88b94aa43516a3dd0eb509d2383f31e2d"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify(root, platform):
    manifest = json.loads((root / "yoodesk-native-sdk.json").read_text(encoding="utf-8"))
    if manifest["version"] != VERSION or manifest["platform"] != platform:
        raise ValueError("Native SDK version/platform mismatch")
    if manifest["openh264_source_sha256"] != OPENH264_SHA or manifest["drmtap_revision"] != DRM_REVISION:
        raise ValueError("Native SDK dependency versions changed; rebuild the SDK")
    if manifest.get("mozjpeg_sys_version") != "2.2.2":
        raise ValueError("Native SDK mozjpeg version mismatch")
    for name, checksum in manifest["files"].items():
        if digest(root / name) != checksum:
            raise ValueError("Native SDK checksum mismatch: " + name)


def package(platform):
    target = TARGETS[platform]
    root = Path("/opt/yoodesk-tools") / ("sysroot-" + platform.removeprefix("linux-"))
    lock = json.loads((ROOT / "ci/native-sdk.lock.json").read_text(encoding="utf-8"))
    if platform in lock["platforms"]:
        verify(root, platform)
        print("Reused published native SDK:", platform, VERSION)
        return
    extras = root / "yoodesk-native"
    h264 = extras / "openh264"
    h264.mkdir(parents=True, exist_ok=True)
    for library in ("common", "processing", "decoder", "encoder"):
        name = "libopenh264_" + library + ".a"
        outputs = list((ROOT / "target" / target / "release/build").glob("openh264-sys2-*/out/" + name))
        if outputs:
            shutil.copy2(max(outputs, key=lambda item: item.stat().st_mtime), h264 / name)
        if not (h264 / name).is_file():
            raise ValueError("OpenH264 archive is not complete: " + name)
    (h264 / "VERSION").write_text("0.9.9\n", encoding="utf-8")
    shutil.copy2(ROOT / "artifacts/openh264-sys2-patched/upstream/LICENSE", h264 / "LICENSE")
    jpeg = extras / "mozjpeg"
    jpeg.mkdir(exist_ok=True)
    outputs = list((ROOT / "desk-engine/target" / target / "release/build").glob("mozjpeg-sys-*/out/libmozjpeg62.a"))
    if outputs:
        built = max(outputs, key=lambda item: item.stat().st_mtime)
        shutil.copy2(built, jpeg / built.name)
        shutil.copytree(built.parent / "include", jpeg / "include", dirs_exist_ok=True)
        for header in (ROOT / "artifacts/mozjpeg-engine-patched/vendor").glob("*.h"):
            shutil.copy2(header, jpeg / "include" / header.name)
    if not (jpeg / "libmozjpeg62.a").is_file():
        raise ValueError("Native mozjpeg SDK is incomplete")
    (jpeg / "VERSION").write_text("2.2.2\n", encoding="utf-8")
    shutil.copy2(ROOT / "artifacts/mozjpeg-engine-patched/LICENSE", jpeg / "LICENSE")
    if platform == "linux-loong64":
        drm = extras / "drmtap"
        drm.mkdir(exist_ok=True)
        for name in ("libdrmtap.so.0", "libdrmtap.manifest.json", "LICENSE.libdrmtap"):
            shutil.copy2(ROOT / "target/release" / name, drm / name)
    files = {path.relative_to(root).as_posix(): digest(path)
             for path in extras.rglob("*") if path.is_file()}
    manifest = {"schema_version": 1, "version": VERSION, "platform": platform,
                "target": target, "openh264_source_sha256": OPENH264_SHA,
                "drmtap_revision": DRM_REVISION, "mozjpeg_sys_version": "2.2.2", "files": files,
                "aom_version": "3.14.1", "runtime_baseline": "2.43" if platform == "linux-loong64" else "2.28"}
    (root / "yoodesk-native-sdk.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    output = ROOT / "artifacts/native-sdk"
    output.mkdir(parents=True, exist_ok=True)
    archive = output / ("native-sdk-" + VERSION + "-" + platform + ".tar.xz")
    subprocess.run(["tar", "--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner",
                    "-I", "xz -T0", "-cf", str(archive), "-C", str(root), "."], check=True)
    checksum = digest(archive)
    (output / (archive.name + ".sha256")).write_text(checksum + "  " + archive.name + "\n", encoding="utf-8")
    (output / (archive.name + ".json")).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print("Packaged reusable native SDK:", archive.name, checksum)


if __name__ == "__main__":
    action, platform = sys.argv[1:3]
    platform = "linux-loong64" if platform == "loongarch64" else "linux-mips64el" if platform == "mips64el" else platform
    if action == "package":
        package(platform)
    elif action == "verify":
        verify(Path(sys.argv[3]), platform)
    elif action == "location":
        lock = json.loads((ROOT / "ci/native-sdk.lock.json").read_text(encoding="utf-8"))
        record = lock["platforms"].get(platform)
        if record:
            print(record["url"], record["sha256"])
