# YooDesk desk

RustDesk 桌面、音视频、输入、剪贴板、文件和聊天；保留 Windows 会话 worker 与 Linux DRM/Portal/uinput worker。

插件导出 `yd_plugin_v1`，独立拥有 runtime 和业务会话，通过版本化 C ABI 的宿主回调共享访问权威。
webservice 在运行时加载本库，转发 HTTP/WebSocket；本仓库不依赖宿主 Rust crate 或兄弟仓库路径。
源码来源提交见 `SOURCE.json`，公共接口快照保留于 `protocol/`。

## 构建与发布

`.github/workflows/build.yml` 在 push/手动运行时构建 Windows x64/arm64、Linux x64/arm64 和 macOS arm64/x64。
Linux 原生代码及依赖在 Ubuntu 18.04 中编译，发布包验证 glibc <= 2.27。
macOS 在原生 runner 上构建，只允许链接 Apple 系统框架，x64 切片由 arm64 runner 交叉编译。
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
