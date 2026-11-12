"""Compile the pinned RustDesk Android capture library in an isolated checkout."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
ROOT = Path(__file__).resolve().parents[1]
TARGET = "aarch64-linux-android"
API = 24

def build_rustdesk_capture(clang, target_dir, env, vcpkg):
    """编译锁定的 RustDesk Android 采集与 JNI 库；不触碰原始子模块。"""
    from prepare_sources import checkout
    locked = next(repo["commit"] for repo in json.loads(
        (ROOT / "dependencies.lock.json").read_text(encoding="utf-8"))["repositories"]
        if repo["name"] == "rustdesk")
    tree = target_dir.parent / "rustdesk-src"
    checkout("rustdesk", tree)
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=tree, text=True)
    if actual.strip() != locked:
        raise ValueError(f"RustDesk Android worktree is not at {locked}: {tree}")
    subprocess.run(["git", "submodule", "update", "--init", "--recursive"], cwd=tree, check=True)
    if subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=tree):
        raise ValueError(f"RustDesk Android worktree contains tracked changes: {tree}")
    triplet = target_dir.parent / "triplets" / "arm64-android.cmake"
    triplet.parent.mkdir(parents=True, exist_ok=True)
    # AOM 会启用 ASM；显式使用 NDK clang，避免 CMake 在 Windows 查找宿主 as。
    triplet.write_text(
        "set(VCPKG_TARGET_ARCHITECTURE arm64)\n"
        "set(VCPKG_CRT_LINKAGE dynamic)\n"
        "set(VCPKG_LIBRARY_LINKAGE static)\n"
        "set(VCPKG_CMAKE_SYSTEM_NAME Android)\n"
        f"set(VCPKG_CMAKE_SYSTEM_VERSION {API})\n"
        "set(VCPKG_BUILD_TYPE release)\n"
        f'set(VCPKG_MAKE_BUILD_TRIPLET "--host={TARGET}")\n'
        "set(VCPKG_CMAKE_CONFIGURE_OPTIONS -DANDROID_ABI=arm64-v8a "
        f'"-DCMAKE_ASM_COMPILER={clang.as_posix()}" '
        f"-DCMAKE_ASM_COMPILER_TARGET={TARGET}{API})\n",
        encoding="utf-8",
    )
    cache = target_dir.parent
    installed = cache / "vcpkg-installed"
    libraries = installed / "arm64-android"
    env = dict(env, VCPKG_INSTALLED_ROOT=str(installed),
               SODIUM_LIB_DIR=str(libraries / "lib"),
               AARCH64_LINUX_ANDROID_OPENSSL_DIR=str(libraries),
               AARCH64_LINUX_ANDROID_OPENSSL_STATIC="1")
    env.setdefault("VCPKG_ROOT", str(vcpkg.parent))
    sysroot = clang.parent.parent / "sysroot"
    resource_dir = Path(subprocess.check_output(
        [str(clang), "-print-resource-dir"], text=True).strip()).as_posix()
    env.setdefault("BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android",
                   shlex.join((f"--sysroot={sysroot.as_posix()}", f"-resource-dir={resource_dir}")))
    subprocess.run(
        [str(vcpkg), "install", "--classic",
         *(f"{port}:arm64-android" for port in ("libvpx", "aom", "libyuv", "libsodium", "openssl")),
         f"--overlay-triplets={triplet.parent}", f"--overlay-ports={tree / 'res/vcpkg'}",
         f"--x-install-root={installed}", f"--x-buildtrees-root={cache / 'vcpkg-build'}",
         f"--x-packages-root={cache / 'vcpkg-packages'}"],
        cwd=cache, env=env, check=True,
    )
    if sys.platform == "win32":
        # libsodium-sys 0.2.7 用构建脚本的 MSVC host 判断库名，而 Android
        # 链接器会再添加 lib 前缀。只在隔离目录提供同一 Android 静态库的别名。
        sodium = cache / "sodium-link"
        sodium.mkdir(exist_ok=True)
        for name in ("libsodium.a", "liblibsodium.a"):
            shutil.copy2(libraries / "lib/libsodium.a", sodium / name)
        env["SODIUM_LIB_DIR"] = str(sodium)
    ffi = tree / "libs/scrap/src/android/ffi.rs"
    original = ffi.read_bytes()
    source = original.decode("utf-8").replace("\r\n", "\n")
    hook = "            VIDEO_RAW.lock().unwrap().update(data, len);"
    if source.count(hook) != 1:
        raise ValueError("Locked RustDesk video callback no longer matches Android sink hook")
    sink = (ROOT / "engine-patches/android-frame-sink.rs").read_text(encoding="utf-8")
    patched = source.replace(hook, f"            if !yoodesk_android_video(data, len) {{\n{hook}\n            }}")
    try:
        ffi.write_bytes((patched + "\n" + sink).encode("utf-8"))
        subprocess.run(["cargo", "rustc", "--locked", "--release", "--target", TARGET,
                        "--target-dir", str(target_dir), "-p", "scrap", "--lib",
                        "--crate-type", "cdylib", "--", "-C", "link-arg=-lc++abi"],
                       cwd=tree, env=env, check=True)
    finally:
        ffi.write_bytes(original)
    return target_dir / TARGET / "release" / "libscrap.so"

