# desk-engine

把 RustDesk 的端侧能力（桌面采集、H.264 编码、输入注入、光标、音频、隐私屏）
封装成一个小而稳定的 C ABI，供 `web-service` 通过自己的 HTTP/WS 协议暴露。

Linux服务优先目标用户X11根窗口实抓（包括Wayland会话的Xwayland），排除全黑帧；
否则逐CRTC实抓通过才启DRM-only root helper，余下走用户RemoteDesktop+ScreenCast Portal。
uinput服务独立普通用户子进程，不与Portal共享状态。DRM模式不启动Portal采集会话。

Portal授权、token和Clipboard v1由本层维护。token位于
`~/.local/share/{product_id}/web-service/{service}/portal/remote-desktop.json`，目录0700/文件0600。
剪贴板授权独立于键鼠；拒绝时能力关闭并返回403，uinput不能读取剪贴板。
拒绝/撤销后重新授权目前需重建worker。`patch_portal_api.py`开放流描述构造、
活动用户缓存刷新及健康客户端主动重建标记；`patch_engine_lifecycle.py`区分正常SWITCH与故障退避。
Windows正常SWITCH保留原有桌面切换动作，不改变UAC/登录桌面的平台处理。
Portal查询只读已授权几何，不重复创建采集管线。授权业务不改上游；既有headless/DRM适配补丁仍保留，
升级时锚点不匹配会明确失败，不能承诺任意版本无适配升级。

## 混合稳定性回归

从仓库根准备独立测试目录，不把配置、凭据或报告加入Git：

```sh
npm install --prefix artifacts/desktop-stability --save-exact playwright@1.64.1
cp web-service/scripts/desktop_stability.mjs artifacts/desktop-stability/
npx --prefix artifacts/desktop-stability playwright install chromium
node artifacts/desktop-stability/desktop_stability.mjs /path/to/service/config.json /path/to/report.json drm 30 32
```

工具针对已部署服务，需读取其配置、公开证书、访问密钥文件与发行二进制。
最后两个参数为混合重连和全部观众退出后重连轮数（各1..100）。第三个参数是报告标签，
不会替你切换后端；Linux的drm/portal标签会核对目标worker实际环境及helper。
Portal测试必须实际设置测试实例配置`enable_drm=false`并在测试后恢复，不能只设置DRM环境变量。

包含两个保留观众及一个重连观众；半数突断，文件回读校验SHA256，聊天校验幂等，
剪贴板图片使用不透明RGBA以避免系统格式转换对隐藏颜色的影响。基线/混合/恢复各阶段采样
FPS、实际VBR码率、WebSocket RTT、解码耗时与首帧时间；RTT不是端到端交互延迟。
额外轮询原生线程与RSS，不能将短时回归等同于长期无泄漏或带宽上限。

Linux可先以图形用户运行`web-service/scripts/desktop_stability_gtk.py`，明确设置正确的
`WAYLAND_DISPLAY`及用户运行目录（依赖GTK3/GObject Introspection）；在运行Node时设置
`YOODESK_STABILITY_FIXTURE_DIR=$XDG_RUNTIME_DIR/yoodesk-wayland-test`，即加入真实原生应用的
双向文本/图片剪贴板往返。夹具保存的只是测试指令和布尔结果，不记录原剪贴板内容。
Windows可在交互桌面运行`desktop_stability_windows.ps1`提供动态画面；完成后关闭夹具窗口。

## 为什么是独立 workspace

RustDesk 依赖它自己 fork 的 webrtc/tungstenite，并需要静态 ffmpeg；这些与
`web-service` 自身的依赖图冲突。因此本 crate 独立构建，产物是静态库，
`web-service/build.rs` 在启用 `desk-engine` feature 时链接它。两边唯一的耦合是
`src/lib.rs` 里的 `extern "C"` 函数和一条有界缓存。

## 不包含官方 GUI

构建启用 RustDesk 的 `headless` feature：Sciter/tao/tray-icon/whiteboard 全部不参与
编译。端侧代码对这些模块的引用由 `src/headless_*.rs` 顶替；隐私屏使用本进程的
Win32 窗口，仍需要 User32/GDI 等系统接口。

## 构建

```bash
# 1) 从 RustDesk 源码树构建（需要 libclang、静态 ffmpeg、libmfx；见运行说明）
python web-service/scripts/build_engine.py

# 2) 再构建 WebService
cargo build --manifest-path web-service/Cargo.toml --locked --release --features desk-engine
```

许可：RustDesk 主体为 AGPL-3.0。链接本库的发行物整体按 AGPL-3.0 分发。

以上命令从仓库根运行；使用隔离的 RustDesk 源码副本。Windows 静态 CRT 与
Linux X11 的系统库/会话环境配置见 `docs/operations/web-service-runtime.md` §8。
剪贴板、共享 AudioService 和 TransferJob 在 `src/endpoint.rs`；Windows 隐私窗口
在 `src/privacy.rs`，不需要注入 DLL。HDR 桌面通过 RustDesk 的 HDR→SDR 路径处理。

视频桥在 `src/video_bridge.rs`：每显示器共享原生编码器，每连接独立缓存与单调 PTS。
`yd_engine_subscribe(display, conn, fps, bitrate)` 接受实际进程配置；poll、光标重放和
关键帧请求均带 conn。网页真实解码后通过 `yd_engine_video_ack(conn, pts)` 释放帧，
丢失参考链通过 `yd_engine_video_refresh(conn)` 请求完整新 IDR；两侧 ABI 必须同步构建。
原生压缩光标在桥内解压为网页契约的 RGBA8。不要把发送成功当解码确认，
也不要重放陈旧 IDR 后跳接缺少参考帧的 delta。
RustDesk复用光标发送CursorId，桥通过上游`cursor_data_message`还原完整形状和热点；
该消息仍更新每会话的最近光标，重放不会停在旧拖拽/文本光标，不增加另一套形状缓存。

静止 X11 新观众在原生 subscribe 阶段主动请求 IDR，不能等 WS 先开放。
输入的 HTTP 兼容键号由 `http_control_key` 按名称映射原生枚举；
`yd_engine_layout`/`yd_engine_key_with_modifiers` 保留字符按下/抬起与左右修饰键，
Text 用一次 UTF-8 Seq。WebService 与本库必须一起更新，关闭时释放已按下的键。

Windows普通字母走成对布局键，桥内按当前前台键盘布局转换为RustDesk Translate VK事件，
让被控端IME处理拼音。Legacy Chr在Windows非组合字符分支仍会提交Unicode文本，不能用于此流程。
组合键与普通字母共用VK路径，避免先抬起修饰键时改变字符的释放路径；文本Seq仍供完整文字注入使用。
Ubuntu网页保留原文本输入行为，实际libpinyin选词已单独验证。

## 一键运行桌面压测

把 `desktop_stability.py`、`desktop_stability.mjs` 和对应的系统夹具放在同一个目录。目标机需要 Python 3.9+、Node.js、Playwright 和 Chromium；首次准备测试目录时运行：

```text
npm install playwright@1.64.1
npx playwright install chromium
```

Windows 使用当前已登录桌面的账号运行，可通过该账号的 SSH 运行。先打开宿主机显示器并保持桌面解锁：

```powershell
python desktop_stability.py --report windows.json
```

Linux 安装 GTK 3 的 Python GI 绑定（Ubuntu 为 `python3-gi`、`gir1.2-gtk-3.0`），在目标机运行：

```bash
sudo python3 desktop_stability.py --backend drm --browsers-path ~/.cache/ms-playwright --report drm.json
sudo python3 desktop_stability.py --backend portal --browsers-path ~/.cache/ms-playwright --report portal.json
sudo python3 desktop_stability.py --backend x11 --browsers-path ~/.cache/ms-playwright --report x11.json
```

默认各做 12 次混合重连和全断开，可用 `--cycles` 和 `--full-cycles` 调整。省略 `--backend` 时测试当前实际后端；X11 需已登录 X11，DRM 需实抓成功。显式要求 Portal/DRM 时，脚本仅在需要时临时修改 DRM 设置并重启服务，结束后恢复原配置。自定义路径使用 `--node`、`--config` 或 `--service`。业务报告与 `*.motion.json` 分开保存；Windows 动画数据来自可见屏幕像素，Linux 来自 GTK 动画心跳。省略 `--report` 会为每次运行生成独立的报告名。
