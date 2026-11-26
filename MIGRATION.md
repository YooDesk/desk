
## 2026-11-12 Android and account devices

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` from the read-only `E:/YooDesk` checkout.
Android capture, input, consent deadlines and frame ownership live in this plugin. The Android build disables the desktop engine feature, exports the existing plugin ABI plus the Android capture C bridge, and builds pinned RustDesk scrap in an ignored checkout. Browser connection deadlines allow local capture consent.

## 2026-11-20 Viewer carrier resume and binary Desk files

Read-only migration source: `E:/YooDesk`, commit `bdd69273495ec7cfe9b6b2750416959c7777ec39`.

Migrated binary streaming upload/download, file block engine RPC, five-session default and plugin-owned browser assets. The browser enables streaming explicitly. Host authentication callbacks and the versioned plugin ABI remain authoritative.

2026-11-25 shared transport sync: bounded browser receive/resumable windows and current API descriptions synchronized; bundled shared SFTP source aligned. Desk business streaming unchanged. 42 browser tests passed, including real JS/Rust resume replay. No release.
## 2026-11-27 共享采集及文件接纳

文件 I/O 保留 8 个执行槽，增加 32 个异步等待名额，取消等待不启动操作。`scripts/frame_wait.rs` 由构建适配器复制到隔离 RustDesk 工作树使用，同一实现也由引擎单元测试验证；嵌入共享源最多等待真实解码确认 250 ms，独立模式仍为 3 秒，每路缓存上限与隐私检查保留。构建增加对旧原生引擎的拒绝检查；可选择的 QoS 诊断仅输出帧率、时间及延迟，不输出身份或协议内容。
