# Hardware capture and 144 FPS validation

Windows LAN acceptance used the exact artifact from
[run 46051372948](https://github.com/YooDesk/desk/actions/runs/46051372948),
source `fb2381afa5a646937a1d5934149742754885ae54`, DLL SHA256
`968d37bead619ca490d5b3849c1b8ac70d11536d6679820704f445b0ea1814a9`.

On the local RTX 5060 Ti machine, the physical display remained at 2560 × 1440,
144 Hz. A visible full-DPI animation was captured through native LAN HTTPS and
decoded by headless Edge WebCodecs to avoid recursive capture. Over 30 seconds
after warm-up, source FPS was 144.0002, captured/encoded FPS was 144.012 and
decoded FPS was 144.0016. GPU texture capture and NVENC remained active. There
were zero decoder errors or reconfigurations, including a deliberate 40 ms
viewer JavaScript pause. This measures decoded throughput; headless decoding
does not establish the presentation rate of a visible browser window.

Separate hardware and forced-software static-image tests verified the first
image and final update against known source pixels. Those tests establish image
correctness and fallback, not software encoding throughput at 144 FPS.

Local evidence is under `artifacts/hardware-pipeline/`:

- `lan-ci-46051372948-1795805791/report.json`: motion throughput.
- `lan-ci-46051372948-1795806090/report.json`: hardware static-image checks.
- `lan-ci-46051372948-1795806109/report.json`: software static-image checks.

## GitHub component builds

All runs use build-only mode and do not publish releases.

| Target | Run | Source | Result |
| --- | --- | --- | --- |
| Windows x64 | [46051372948](https://github.com/YooDesk/desk/actions/runs/46051372948) | `fb2381a` | Passed; local LAN acceptance above |
| Windows x86 | [46059404585](https://github.com/YooDesk/desk/actions/runs/46059404585) | `58dd35e` | Passed |
| Windows ARM64 | [46059407842](https://github.com/YooDesk/desk/actions/runs/46059407842) | `58dd35e` | Passed |
| Linux x64 | [46054146764](https://github.com/YooDesk/desk/actions/runs/46054146764) | `fb2381a` | Passed |
| Linux ARM64 | [46059414121](https://github.com/YooDesk/desk/actions/runs/46059414121) | `58dd35e` | Passed |
| macOS ARM64 | [46055359068](https://github.com/YooDesk/desk/actions/runs/46055359068) | `86538f4` | Passed |
| macOS x64 | [46059410883](https://github.com/YooDesk/desk/actions/runs/46059410883) | `58dd35e` | Passed |
| Android ARM64 component | [46054155358](https://github.com/YooDesk/desk/actions/runs/46054155358) | `fb2381a` | Passed |

The changes after the Windows-tested source enable the missing macOS x264
dependency and guard SDK publication in build-only mode. Windows runtime code
is unchanged by those changes. Dependencies use the pinned RustDesk/vcpkg
recipes with reviewed adapters in isolated build worktrees. Port and dependency
ABI changes invalidate the relevant binary cache entries.

Windows x86 additionally passed 11 SDK video tests, the actual FFmpeg input-pool
ownership test, six bitmap tests and 110 main-library tests (seven ignored).
Its exact artifact was checked for source and content hashes, x86 PE architecture,
system DLL dependencies and absence of dynamically linked CRTs.

Windows ARM64 passed 11 SDK video tests, six bitmap tests and 110 main-library
tests (seven ignored). Its exact DLL passed ARM64 PE architecture, source/content
hashes, system DLL dependency and static CRT checks. The archive check on this
architecture verifies x264 software fallback; it does not establish availability
of a hardware encoder on a Windows ARM64 device.

Linux ARM64 passed 11 SDK video tests and 110 main-library tests (eight ignored).
Both shipped ELF files were independently inspected: the plugin requires at most
glibc 2.27 and libdrmtap at most glibc 2.17; both are AArch64.

macOS x64 passed 11 SDK video tests, 102 main-library tests (four ignored) and
14 Python checks. Its exact artifact passed hash, x86_64 Mach-O, system-framework
dependency and build-rpath checks. The FFmpeg archive contains both x264 and
VideoToolbox H.264 encoders.

The Android CI artifact exports all four encoded v1 bridge functions. Current
client JNI glue successfully cross-links against that exact artifact with NDK
26.1.10909125, API 24, warnings as errors, no undefined strong symbols and
16 KiB load alignment. A separate local APK packaging/signature/payload check
uses the six pinned released components. The older released Desk component
keeps software fallback until a matching new component is released. No release
pins were replaced with unpublished artifacts.

## Outstanding acceptance

Other-platform physical throughput remains for the user's machines. These build
results do not establish their actual capture, encode or decode rate.

The independent WebService and Android APK GitHub runs were prevented from
starting by the account billing/spending restriction. Local host configuration
allows 144 FPS and passed its boundary test; local APK compilation, DEX,
resources, signature and actual payload checks passed. Cloud validation remains
outstanding.

Fresh build-only attempts confirmed the same restriction:
[WebService 46067496993](https://github.com/YooDesk/webservice/actions/runs/46067496993)
at `08593d1`, and
[APK 46067491935](https://github.com/YooDesk/yoodeskclient/actions/runs/46067491935)
at `89075b0`. Both were stopped before any build step executed. Their check-run
annotations explicitly report failed account payments or the spending limit.
Both release jobs were skipped.

The remote Gateway's public HTTPS and STUN binding response were verified, but
the provided account login returned HTTP 401. Authorized remote Gateway/STUN
desktop acceptance at 144 FPS remains outstanding. No remote deployment or
configuration change was made for these checks.
