# YooDesk desk

RustDesk 桌面、音视频、输入、剪贴板、文件和聊天；保留 Windows 会话 worker 与 Linux DRM/Portal/uinput worker。

插件导出 `yd_plugin_v1`，独立拥有 runtime 和业务会话，通过版本化 C ABI 的宿主回调共享访问权威。
webservice 在运行时加载本库，转发 HTTP/WebSocket；本仓库不依赖宿主 Rust crate 或兄弟仓库路径。
源码来源提交见 `SOURCE.json`，公共接口快照保留于 `protocol/`。

## 构建与发布

`.github/workflows/build.yml` 在 push/手动运行时构建 Windows x64/arm64、Linux x64/arm64。
Linux 原生代码及依赖在 Ubuntu 18.04 中编译，发布包验证 glibc <= 2.27。
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
