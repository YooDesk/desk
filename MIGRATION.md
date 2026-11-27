
## 2026-11-12 Android and account devices

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` from the read-only `E:/YooDesk` checkout.
Android capture, input, consent deadlines and frame ownership live in this plugin. The Android build disables the desktop engine feature, exports the existing plugin ABI plus the Android capture C bridge, and builds pinned RustDesk scrap in an ignored checkout. Browser connection deadlines allow local capture consent.

## 2026-11-20 Viewer carrier resume and binary Desk files

Read-only migration source: `E:/YooDesk`, commit `bdd69273495ec7cfe9b6b2750416959c7777ec39`.

Migrated binary streaming upload/download, file block engine RPC, five-session default and plugin-owned browser assets. The browser enables streaming explicitly. Host authentication callbacks and the versioned plugin ABI remain authoritative.

2026-11-25 shared transport sync: bounded browser receive/resumable windows and current API descriptions synchronized; bundled shared SFTP source aligned. Desk business streaming unchanged. 42 browser tests passed, including real JS/Rust resume replay. No release.
## 2026-11-27 共享采集及文件接纳

文件 I/O 保留 8 个执行槽，增加 32 个异步等待名额，取消等待不启动操作。`scripts/frame_wait.rs` 由构建适配器复制到隔离 RustDesk 工作树使用，同一实现也由引擎单元测试验证；嵌入共享源最多等待真实解码确认 250 ms，独立模式仍为 3 秒，每路缓存上限与隐私检查保留。构建增加对旧原生引擎的拒绝检查；可选择的 QoS 诊断仅输出帧率、时间及延迟，不输出身份或协议内容。
# Hardware-first video adapter (2026-11-27)

The pinned RustDesk `flutter-build.yml` uses the manifest's static vcpkg triplets
and enables `vram` for supported Windows builds. The embedded engine now follows
that selection, probes hardware through RustDesk, and appends software encoders
instead of replacing the probe result. `scripts/patch_video_pipeline.py` applies
the H.264-preserving failure policy and the 144 FPS QoS ceiling only in the
ignored engine worktree. `scripts/patch_hwcodec.py` supplies the pinned codec's
missing encoder-owned GPU-frame repeat adapter and MediaSDK loader compilation.

macOS's isolated FFmpeg port adds libx264 fallback; Android uses a versioned
encoded-frame bridge from a MediaProjection/MediaCodec Surface, preserving the
consent epoch when falling back to raw capture and OpenH264. The host must raise
its matching configuration validation ceiling to 144 FPS.

Native caches use a new generation and a port ABI revision. Archive checks reject
missing native encoders. Windows desktop acceptance and GitHub builds must be
recorded separately from the passing policy/unit regressions; a cap of 144 does
not itself establish 144 FPS throughput.


## Bounded Windows high-FPS pipeline

The Windows NVENC adapter pipelines one frame above 60 FPS without B pictures
or decoder reordering. Three separately allocated NV12 surfaces are retained
by the encoder; FFmpeg's input-reference count prevents reuse while an input
is still pending. CPU submission/output-wait timings no longer represent the
complete latency of an individual GPU encode.

Accepted input without a packet does not produce a video message or decode
acknowledgment. Bounded warm-up and static repeats preserve first and final
images; persistent no-output falls back instead of warming up forever. Static
repeats copy encoder-owned NV12, never a released DXGI pointer. Tests exercise
actual FFmpeg frame-reference ownership, exhaustion, release and partial cleanup.

Capture and SDK receive threads use scoped Windows MMCSS Capture tasks. The
capture frame controller reuses its Tokio runtime. A valid first DXGI snapshot
can seed a static session; fractional acquisition timeouts round up. The native
refresh clock is used only below the requested frame-rate cap. The business
broadcast capacity matches the native 18-frame window (Tokio rounds its storage
to 32 slots), while lagging readers still trigger bounded recovery.

Local validation uses the exact pinned CI dependency SDK. Final Windows
acceptance must also use a plugin artifact matching its successful CI commit;
local diagnostic builds are explicitly labeled separately.
