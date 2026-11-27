# YooDesk desk

RustDesk 桌面、音视频、输入、剪贴板、文件和聊天；保留 Windows 会话 worker 与 Linux DRM/Portal/uinput worker。

插件导出 `yd_plugin_v1`，独立拥有 runtime 和业务会话，通过版本化 C ABI 的宿主回调共享访问权威。
webservice 在运行时加载本库，转发 HTTP/WebSocket；本仓库不依赖宿主 Rust crate 或兄弟仓库路径。
源码来源提交见 `SOURCE.json`，公共接口快照保留于 `protocol/`。

## 构建与发布

`.github/workflows/build.yml` 在 push/手动运行时构建 Windows x86/x64/arm64、Linux x64/arm64/loong64/loong64-oldworld/mips64el 和 macOS arm64/x64。
Linux x64/arm64 的原生代码及依赖在 Ubuntu 18.04 中编译，发布包验证 glibc <= 2.27。
龙芯旧世界 `linux-loong64-oldworld` 使用 ABI 1.0 和 glibc 2.28，与现有新世界产物独立；工具链与验证说明见 [平台构建说明](docs/ci-platforms.md)。
macOS 在对应架构的原生 runner 上构建，只允许链接 Apple 系统框架。
只有全部平台成功才发布最新 Release。每个平台产物为 `yoodesk-desk-<platform>.zip`
及 `.sha256`，包含动态库、`plugin.json`、浏览器 JS/WASM 和相应运行附件/许可证。
webservice 流水线通过公共 Release API 下载这些产物，不需要额外 PAT。

```sh
cargo build --locked --release --lib
cargo test --locked --lib
python ci/package.py desk --platform linux-x64
```

本地打包设置 `CARGO_BUILD_TARGET` 为相应 Rust target，使产物位于 `target/<target>/release`。
浏览器依赖使用 `npm ci --prefix frontend --ignore-scripts`，构建命令是 `node ci/browser.mjs desk`。

原生依赖先用 `python scripts/prepare_sources.py` 准备锁定 RustDesk/hwcodec 与适配，
再按 workflow 安装 LLVM、vcpkg 和媒体依赖，执行 `python scripts/build_engine.py`。
`YOODESK_DESK_ENGINE_LIB_DIR` 必须指向独立引擎的 `target/<target>/release`。
本库含 RustDesk，按 AGPL-3.0 分发；不编译 Flutter/Sciter 官方 UI。

## Hardware video and high refresh rates

The desktop SDK probes RustDesk's hardware encoders and keeps software H.264 at
lowest priority. Windows x86/x64 enables RustDesk's `vram` feature so DXGI textures
can stay on the GPU through encoding. Linux uses the available NVENC/VAAPI/AMF
path, and macOS uses VideoToolbox. Hardware initialization or encoding failures
retain H.264 software fallback instead of switching browser sessions to VP9.
The macOS FFmpeg build includes libx264 alongside VideoToolbox.

Android's host adapter uses MediaProjection directly into a hardware MediaCodec
Surface when supported, then switches to the existing raw-capture/OpenH264 path
on failure. This adapter requires the matching Desk component's versioned
`yd_android_*_v1` encoded-video bridge. Permission revocation and capture epochs
remain authoritative across fallback.

Set `fps` to a value in `1..=144` in the WebService configuration. This is a
target, not a claim about capture, encoder, display, or viewer throughput.
Existing explicit configuration and the default 20 FPS are preserved.
`YOODESK_VIDEO_PIPELINE_DIAGNOSTICS=1` logs actual captured/encoded FPS and stage
times. `YOODESK_VIDEO_ENCODER=software` selects the software diagnostic path.

Native dependencies follow the pinned RustDesk workflow's vcpkg manifest and
static target triplets. CI uses a new native-video cache generation and audits
FFmpeg archives for the required encoder symbols before accepting them.

Windows NVENC H.264 above 60 FPS uses three owned GPU input surfaces with one
frame in flight. A surface is reused only after FFmpeg releases its input
reference. Static first frames and final updates drain through the same owned
surfaces. Producer and receiver threads use scoped MMCSS capture scheduling;
DXGI's refresh clock replaces the software timer only when it respects the
requested FPS cap. Software encoding remains the fallback.

The video broadcast channel accommodates the native 18-frame window, avoiding
encoder resets after short writer stalls. Both frame count and native pending
bytes remain bounded. Build-only Windows x64 runs also export their native
dependency SDK for local builds against the same CI libraries.

## macOS

macOS 原生支持来自上游 `b9fc78a`：引擎使用 VideoToolbox H.264，文件操作锚定目录描述符，
`macos_worker_host` 按 `/dev/console` 所属 UID 在 Aqua 与 LoginWindow 域启动同一个 worker 二进制。
系统安装（LaunchDaemon、LaunchAgent、`/Library/Application Support/<product>-host`）属于
webservice 宿主，本仓库不写系统目录。

构建需要原生 Apple Silicon 或 Intel 工具链、Xcode 命令行工具、Node.js、CMake 4.4+ 和 Ninja：

```sh
python3 scripts/prepare_sources.py
python3 scripts/build_engine.py     # 生成 native-static-libs.txt
YOODESK_DESK_ENGINE_LIB_DIR="$PWD/desk-engine/target/aarch64-apple-darwin/release" \
  cargo build --locked --release --lib --target aarch64-apple-darwin
python3 ci/package.py desk --platform macos-arm64
```

Intel 切片用 `x86_64-apple-darwin` 和 `macos-x64`。`ci/package.py` 拒绝链接
Homebrew 等非系统动态库、带构建 rpath 或架构不符的镜像。

## Android ARM64

Manually dispatch `.github/workflows/build-android.yml` after committing source changes.
The workflow runs component unit tests, cross-compiles with NDK 26.1.10909125 / API 24,
checks ARM64, shared Bionic dependencies and 16 KiB load alignment, and publishes
`yoodesk-desk-android-arm64.zip` plus SHA256 in `android-v0.1.0-ci.<run>`.
These releases do not replace the latest desktop release. Build order is Connector,
Desk, SSH, RDP, VNC, WebService, then `yoodeskclient`'s Android workflow; the installer pins all six
complete Android releases and their commit IDs before packaging.

The Android APK loads Desk, SSH, RDP and VNC plugins. Gateway retains its existing server builds and has no Android workflow.


## 2026-11-21 MIPS plugin startup correction

WebService dynamic-plugin acceptance exposed startup failure in released MIPS plugins. Vendor the exact libc 0.2.191 archive with Connector's reviewed five MIPS64 architecture-selector corrections (socket options/types, signals, sigaction and pthread mutex ABI). Keep upstream licenses and PATCHES provenance. Remove the old MIPS socket-test exclusions. Add an exported-C-ABI startup regression that binds the actual plugin loopback listener, verifies missing transport token returns HTTP 403, stops the runtime and checks the listener is released. Full platform builds and WebService dynamic integration remain required before replacing released dependencies.

QEMU release acceptance (2026-11-25, explicit user decision): cross-compile, inspect target ABI/static or shared-library dependencies, and perform bounded startup/config/plugin-load smoke checks. Do not run full Rust/Go test suites or throughput gates under emulation. Existing native-runner regression policy is unchanged. Full-suite results and physical throughput must be established on native hardware.
