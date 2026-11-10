"""准备 Linux root worker 使用的固定版本 DRM 运行库，不修改系统 RustDesk 安装。"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
from tempfile import TemporaryDirectory

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/rustdesk-org/libdrmtap"
REVISION = "5936a7a88b94aa43516a3dd0eb509d2383f31e2d"


def prepare(tree: Path, destination: Path) -> Path:
    """构建并验证固定版本、带 EGL 且禁用特权 helper 的共享库。"""
    work = ROOT / "desk-engine" / "target" / "drmtap"
    source = work / "source"
    build = work / "build"
    work.mkdir(parents=True, exist_ok=True)
    if not (source / ".git").exists():
        subprocess.run(["git", "clone", "--no-checkout", REPOSITORY, str(source)], check=True)
        subprocess.run(["git", "-C", str(source), "checkout", "--detach", REVISION], check=True)
    revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if revision != REVISION:
        raise RuntimeError(f"DRM checkout must be {REVISION}, got {revision}; refusing to replace it")
    dirty = subprocess.check_output(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=no"], text=True)
    if dirty:
        raise RuntimeError("DRM checkout has tracked modifications; refusing to build")
    # Always reconfigure: meson records the compiler command in the build
    # directory, so a cached build tree would keep using a stale toolchain
    # (and would not pick up a changed cross file).
    command = ["meson", "setup", str(build), str(source), "--buildtype=release",
               "-Dhelper=disabled", "--wipe"]
    # Cross builds pass a meson cross file through YOODESK_MESON_CROSS_FILE:
    # meson must not execute target binaries during its compiler probes.
    cross_file = os.environ.get("YOODESK_MESON_CROSS_FILE")
    if cross_file:
        command += ["--cross-file", cross_file]
    subprocess.run(command, check=True)
    subprocess.run(["meson", "compile", "-C", str(build), "drmtap:shared_library",
                    "drmtap:static_library"], check=True)
    # Meson strips the build RUNPATH when installing the library into a staging root.
    with TemporaryDirectory(prefix="yoodesk-drmtap-") as temporary:
        subprocess.run(["meson", "install", "-C", str(build), "--no-rebuild",
                        "--destdir", temporary], check=True)
        libraries = [path for path in Path(temporary).rglob("libdrmtap.so.0.*")
                     if path.is_file() and not path.is_symlink()]
        if len(libraries) != 1:
            raise RuntimeError(f"expected one installed DRM library, got {libraries}")
        library = libraries[0]
        dynamic = subprocess.check_output(["readelf", "-d", str(library)], text=True)
        if "(RPATH)" in dynamic or "(RUNPATH)" in dynamic:
            raise RuntimeError("Installed DRM library must use system library lookup")
        blob = library.read_bytes()
        if not all(marker in blob for marker in (b"libEGL.so.1", b"eglCreateImageKHR")):
            raise RuntimeError("DRM library lacks EGL detiling; install libegl-dev/libgles2-mesa-dev and rebuild")
        import importlib.util
        specification = importlib.util.spec_from_file_location("rustdesk_drm_build", tree / "build.py")
        if specification is None or specification.loader is None:
            raise RuntimeError("RustDesk DRM build verifier unavailable")
        verifier = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(verifier)
        verifier._assert_so_has_no_helper(str(library))
        verifier.assert_so_satisfies_the_runtime_abi_gate(str(library))
    destination.mkdir(parents=True, exist_ok=True)
    output = destination / "libdrmtap.so.0"
    output.write_bytes(blob)
    output.chmod(0o755)
    manifest = {"repository": REPOSITORY, "revision": REVISION, "sha256": hashlib.sha256(blob).hexdigest(), "helper": False, "egl": True, "license": "MIT"}
    (destination / "libdrmtap.manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    shutil.copy2(source / "LICENSE", destination / "LICENSE.libdrmtap")
    return output


def patch_loader(tree: Path) -> None:
    """保留上游 root 仅绝对路径加载规则，路径由品牌构建时明确指定。"""
    from patch_uinput_mouse import patch_mouse
    patch_mouse(tree)
    loader = tree / "libs" / "scrap" / "src" / "common" / "drmtap_dl.rs"
    content = loader.read_text(encoding="utf-8")
    old = 'const INSTALLED: &str = "/usr/lib/rustdesk/libdrmtap.so.0";'
    new = 'const INSTALLED: &str = match option_env!("YOODESK_DRMTAP_PATH") { Some(path) => path, None => "/usr/lib/yoodesk/libdrmtap.so.0" };'
    if new not in content:
        if old not in content:
            raise RuntimeError("RustDesk DRM loader contract changed; review the absolute-path adapter")
        loader.write_text(content.replace(old, new, 1), encoding="utf-8")
    # 本产品 root helper 不需要上游通用服务的跨用户可连接权限；保留其 peercred
    # 授权，额外在 bind 时就使用 0710/0600，避免随后 chmod 的启动竞态。
    adaptations = [
        ("src/server/wayland.rs", 'pub(super) async fn ensure_inited() -> ResultType<()> {', 'pub(super) async fn ensure_inited() -> ResultType<()> {\n    #[cfg(feature = "drm")]\n    if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some() && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() == Ok("1") {\n        update_uinput_resolution().await;\n        return Ok(());\n    }'),
        ("src/server/wayland.rs", 'pub(super) async fn check_init() -> ResultType<()> {', 'pub(super) async fn check_init() -> ResultType<()> {\n    if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some() && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() == Ok("1") {\n        bail!("DRM capture unavailable; Portal fallback is disabled for this worker");\n    }'),
        ("src/server/wayland.rs", 'pub(super) fn get_capturer_for_display(\n    display_idx: usize,\n) -> ResultType<super::video_service::CapturerInfo> {', 'pub(super) fn get_capturer_for_display(\n    display_idx: usize,\n) -> ResultType<super::video_service::CapturerInfo> {\n    #[cfg(feature = "drm")]\n    if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some() && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() == Ok("1") {\n        return super::drm_capturer::get_capturer_info(display_idx);\n    }'),
        ("src/server.rs", 'pub(crate) mod drm_capturer;', 'pub(crate) mod drm_capturer;\n\n#[cfg(all(target_os = "linux", feature = "drm"))]\npub fn yoodesk_drm_display_infos() -> Option<Vec<base::message_proto::DisplayInfo>> {\n    if drm_capturer::is_available() { drm_capturer::get_display_infos() } else { None }\n}'),
        ("src/common.rs", 'static ref IS_SERVER: bool = std::env::args().nth(1) == Some("--server".to_owned());', 'static ref IS_SERVER: bool = std::env::args().nth(1) == Some("--server".to_owned()) || (std::env::args().nth(1).as_deref() == Some("--rustdesk-worker") && std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some());'),
        ("libs/hbb_common/src/config.rs", 'pub fn is_service_ipc_postfix(postfix: &str) -> bool {', 'pub fn is_service_ipc_postfix(postfix: &str) -> bool {\n    if std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() == Ok("1") && postfix.starts_with("_uinput_") { return false; }'),
        ("src/server/drm_capturer.rs", 'pub(crate) fn is_available_cached() -> bool {', 'pub(crate) fn is_available_cached() -> bool {\n    if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some() && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() != Ok("1") { return false; }'),
        ("src/server/drm_capturer.rs", 'pub(crate) fn is_available() -> bool {', 'pub(crate) fn is_available() -> bool {\n    if std::env::var_os("YOODESK_ENGINE_NAMESPACE").is_some() && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() != Ok("1") { return false; }'),
        ("src/ipc/fs.rs", "if config::is_service_ipc_postfix(postfix) {\n        0o0711", "if config::is_service_ipc_postfix(postfix) {\n        if std::env::var_os(\"YOODESK_ENGINE_HELPER_UID\").is_some() { return 0o0710; }\n        0o0711"),
        ("src/ipc.rs", "let socket_mode = if config::is_service_ipc_postfix(postfix) {\n                    0o0666", "let socket_mode = if config::is_service_ipc_postfix(postfix) {\n                    if std::env::var_os(\"YOODESK_ENGINE_HELPER_UID\").is_some() { 0o0600 } else { 0o0666 }"),
        ("src/ipc/drm.rs", "std::fs::Permissions::from_mode(0o666)", "std::fs::Permissions::from_mode(if std::env::var_os(\"YOODESK_ENGINE_HELPER_UID\").is_some() { 0o600 } else { 0o666 })"),
    ]
    for relative, before, after in adaptations:
        path = tree / relative
        text = path.read_text(encoding="utf-8")
        if after not in text:
            if before not in text:
                raise RuntimeError(f"RustDesk privileged IPC contract changed: {relative}")
            path.write_text(text.replace(before, after, 1), encoding="utf-8")


def main() -> None:
    """单独准备运行库，也供统一品牌构建入口调用。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, default=ROOT / "artifacts/rustdesk-engine-src")
    parser.add_argument("--destination", type=Path, default=ROOT / "target" / "release")
    args = parser.parse_args()
    patch_loader(args.tree.resolve())
    print(prepare(args.tree.resolve(), args.destination.resolve()))


if __name__ == "__main__":
    main()
