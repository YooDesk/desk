"""Build the desk-engine static library against a RustDesk source checkout.

The engine is a separate Cargo workspace so RustDesk's forked dependencies
(webrtc, tungstenite, static ffmpeg) never enter web-service's dependency graph.
This script applies the engine patch if needed, links the source tree, and builds
the static library that `build.rs` links.
"""
from __future__ import annotations

import os
import json
import pathlib
import shlex
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
# Isolated Python omits the script directory from its module search path.
sys.path.insert(0, str(ROOT / 'scripts'))
PATCH = ROOT / "engine-patches" / "rustdesk-1.5.1-engine.patch"



def rustdesk_dir() -> pathlib.Path:
    configured = os.environ.get("YOODESK_RUSTDESK_DIR")
    linked = ROOT / "artifacts/rustdesk-engine-src"
    base = pathlib.Path(configured) if configured else linked if linked.exists() else ROOT / "deps/rustdesk"
    return base.resolve()


def library_deps() -> dict[str, str]:
    """Environment the engine build needs (static ffmpeg, libclang, MSVC CRT)."""
    return {
        # ffmpeg/ffmpeg deps live in the vcpkg-style tree built for this machine.
        "VCPKG_ROOT": os.environ.get("VCPKG_ROOT", ""),
        "VCPKG_INSTALLED_ROOT": os.environ.get("VCPKG_INSTALLED_ROOT", ""),
        # bindgen needs libclang; VS ships one.
        "LIBCLANG_PATH": os.environ.get("LIBCLANG_PATH", ""),
        # ffmpeg static libs are /MT, so the Rust side must match.
        "RUSTFLAGS": os.environ.get("RUSTFLAGS", "-C target-feature=+crt-static" if os.name == "nt" else ""),
    }


def main() -> int:
    tree = rustdesk_dir()
    if not (tree / "Cargo.toml").exists():
        print(f"RustDesk tree not found: {tree}", file=sys.stderr)
        print("Set YOODESK_RUSTDESK_DIR or place the checkout at ../rustdesk", file=sys.stderr)
        return 1

    applied = subprocess.run(["git", "-C", str(tree), "apply", "--reverse", "--check", "--ignore-space-change", str(PATCH)], capture_output=True).returncode == 0
    if not applied and not (tree / "src" / "headless_client.rs").is_file():
        print(f"applying engine patch to {tree}")
        result = subprocess.run(["git", "-C", str(tree), "apply", "--ignore-space-change", str(PATCH)])
        if result.returncode != 0:
            print("patch failed (already partially applied?)", file=sys.stderr)
            return 1

    overlays = ["rustdesk-headless-trusted-devices.patch", "rustdesk-x265-pixel-format.patch",
                "rustdesk-build-version-host.patch", "rustdesk-scrap-va-vdpau-optional.patch"]
    if sys.platform == "darwin":
        overlays.append("rustdesk-macos-headless-update.patch")
        overlays.append("rustdesk-macos-loginwindow-input.patch")
        overlays.append("rustdesk-macos-layout-key-events.patch")
    elif os.name == "nt":
        overlays.append("rustdesk-windows-keypad-meta.patch")
    for name in overlays:
        overlay = PATCH.parent / name
        if subprocess.run(["git", "-C", str(tree), "apply", "--reverse", "--check", "--ignore-space-change", str(overlay)], capture_output=True).returncode:
            subprocess.run(["git", "-C", str(tree), "apply", "--ignore-space-change", str(overlay)], check=True)
    from patch_engine_lifecycle import patch as patch_engine_lifecycle
    patch_engine_lifecycle(tree)

    if sys.platform == "linux":
        from patch_portal_api import patch as patch_portal_api
        patch_portal_api(tree)
        from patch_uinput_mouse import patch_mouse
        patch_mouse(tree)
        from prepare_drmtap import patch_loader, prepare
        patch_loader(tree)
        # The DRM capture runtime needs libdrm >= 2.4.99 (drmModeFB2). The
        # Loongson 2K mips64el systems ship 2.4.97, so that backend is not
        # built for them; X11 and Wayland capture are unaffected.
        if os.environ.get("YOODESK_SKIP_DRMTAP") != "1":
            prepare(tree, ROOT / "target" / "release")
        audio_patch = PATCH.parent / "rustdesk-default-audio-monitor.patch"
        audio_source = tree / "src" / "platform" / "linux.rs"
        audio_content = audio_source.read_text(encoding="utf-8")
        # 新上游已通过独立函数选择默认输出monitor；不向已修复源码重复套旧补丁。
        upstream_default_monitor = ("if let Some(monitor) = get_default_pa_monitor()" in audio_content
                                    and "fn get_default_pa_monitor() -> Option<String>" in audio_content
                                    and ".default_sink_name?" in audio_content)
        if ".and_then(|sink| sink.monitor_name)" not in audio_content and not upstream_default_monitor:
            subprocess.run(["git", "-C", str(tree), "apply", "--ignore-space-change",
                            str(audio_patch)], check=True)
        headless = tree / "src" / "headless_client.rs"
        if "SCRAP_XDP_PORTAL_UNAVAILABLE" not in headless.read_text(encoding="utf-8"):
            subprocess.run(["git", "-C", str(tree), "apply", "--ignore-space-change",
                            str(PATCH.parent / "rustdesk-headless-linux.patch")], check=True)

    # The engine crate uses a fixed relative path to keep Cargo manifests deterministic.
    link = ROOT / "artifacts/rustdesk-engine-src"
    if link.exists() and link.resolve() != tree:
        print(f"Engine source link {link} points to {link.resolve()}, expected {tree}", file=sys.stderr)
        return 1
    if not (link.exists() or link.is_symlink()):
        link.parent.mkdir(parents=True, exist_ok=True)
        if os.name == "nt":
            subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(tree)], check=True)
        else:
            link.symlink_to(tree, target_is_directory=True)
        print(f"linked {link} -> {tree}")

    if "--prepare-only" in sys.argv:
        return 0

    env = dict(os.environ)
    # libsodium-sys links statically by default; SODIUM_STATIC is a fatal deprecated option.
    env.pop("SODIUM_STATIC", None)
    env.pop("SODIUM_SHARED", None)
    if sys.platform == "linux":
        product = "yoodesk"
        if env.get("PRODUCT_BRAND_FILE"):
            product = json.loads(pathlib.Path(env["PRODUCT_BRAND_FILE"]).read_text(encoding="utf-8"))["product_id"]
        if not product or any(char not in "abcdefghijklmnopqrstuvwxyz0123456789-" for char in product):
            raise ValueError("invalid product_id for trusted DRM library path")
        env["YOODESK_DRMTAP_PATH"] = f"/usr/lib/{product}/libdrmtap.so.0"
    for key, value in library_deps().items():
        if value:
            env[key] = value
    if os.name == "nt":
        env.setdefault("RUSTFLAGS", "-C target-feature=+crt-static")

    # 先获取锁定源码，再应用原生低延迟/Linux FFmpeg 补丁，避免复用旧原生产物。
    subprocess.run([sys.executable, str(ROOT / "scripts" / "patch_hwcodec.py")], env=env, check=True)
    # The patched nix crate is a path dependency outside the lockfile; it must
    # exist before cargo resolves the workspace, and adding it requires letting
    # cargo refresh the lock once.
    subprocess.run([sys.executable, str(ROOT / "scripts" / "prepare_nix.py")], env=env, check=True)
    subprocess.run([sys.executable, str(ROOT / "scripts" / "prepare_ring.py")], env=env, check=True)
    subprocess.run([sys.executable, str(ROOT / "scripts" / "prepare_mozjpeg.py")], env=env, check=True)
    fetch = ["cargo", "fetch", "--locked"]
    if os.name == "nt" or sys.platform == "darwin":
        target = env.get("CARGO_BUILD_TARGET")
        if not target:
            info = subprocess.check_output(["rustc", "-vV"], env=env, text=True)
            target = next(line.removeprefix("host: ") for line in info.splitlines() if line.startswith("host: "))
        fetch.extend(["--target", target])
    subprocess.run(fetch, cwd=ROOT / "desk-engine", env=env, check=True)

    # Cross ports may need extra flags (e.g. -Z build-std when the Rust
    # distribution has no prebuilt std for the target).
    target = env.get("CARGO_BUILD_TARGET")
    if sys.platform == "darwin":
        # rustc reports the frameworks and dylibs that a staticlib consumer must link.
        # A separate rustc invocation also reports them when the dependencies are cached.
        command = ["cargo", "rustc", "--release", "--locked", "--lib"]
        # x64 is cross-compiled on the arm64 runner; the host target needs no flag.
        if target:
            command += ["--target", target]
        command += ["--", "--print", "native-static-libs"]
    else:
        command = ["cargo", "build", "--release", "--locked"]
    command.extend(shlex.split(os.environ.get("YOODESK_ENGINE_CARGO_ARGS", "")))
    print("running:", " ".join(command))
    flags = []
    if sys.platform == "darwin":
        with subprocess.Popen(command, cwd=ROOT / "desk-engine", env=env,
                              stderr=subprocess.PIPE, text=True) as build:
            for line in build.stderr:
                sys.stderr.write(line)
                if line.startswith("note: native-static-libs: "):
                    flags.append(line.removeprefix("note: native-static-libs: ").strip())
            returncode = build.wait()
    else:
        returncode = subprocess.run(command, cwd=ROOT / "desk-engine", env=env).returncode
    if returncode != 0:
        return returncode
    output = ROOT / "desk-engine" / "target"
    if target := env.get("CARGO_BUILD_TARGET"):
        output /= target
    library = output / "release" / ("yoodesk_desk_engine.lib" if os.name == "nt" else "libyoodesk_desk_engine.a")
    if sys.platform == "darwin":
        if len(flags) != 1:
            raise RuntimeError("rustc did not report the macOS Desk static library link flags")
        library.with_name("native-static-libs.txt").write_text(flags[0] + "\n", encoding="utf-8")
    print(f"built {library} ({library.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
