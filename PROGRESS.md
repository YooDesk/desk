# 2026-11-12 macOS build follow-up

Run `43131536269` compiled the macOS arm64 engine successfully, but its build
driver failed to recognize the colored `native-static-libs` note from rustc.
The driver now accepts both plain and ANSI-colored notes and disables Cargo
color for the captured invocation. Two focused parser regressions pass locally.
The macOS thread adapter also borrows its owned socket path when invoking the
Unix worker, matching the worker's `&str` interface.

Existing Linux toolchain and published SDK reuse scripts and locks are unchanged.
The LoongArch job in that run restored both tool/native compilation caches and
completed successfully. Other platforms and the repaired macOS flow still need
the subsequent complete workflow result; no desktop runtime claim is made here.

## Android/device migration validation (2026-11-12)

Upstream c0975f2 migrated with standalone repository adapters. Local validation and actual GitHub run/release results are tracked in `E:/YooDeskGroup/.ci-tmp/android-build-state.json` and the final acceptance report.

Local Windows no-default-features Cargo library suite: 100 passed, including actual Android frame delivery, H.264 decode, input queue and consent lifecycle. Desk browser connection lifecycle suite: 11 passed.

## 2026-11-12 final Android/device migration acceptance

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` with the standalone repository boundaries preserved.
- build-android.yml: success; release `android-v0.1.0-ci.2`; run https://github.com/YooDesk/desk/actions/runs/43189034948; tested commit `66e429c7160867ad35fe338f028d5a9bb98e0296`.
- build.yml: success; release `v0.1.0-ci.134`; run https://github.com/YooDesk/desk/actions/runs/43190276794; tested commit `66e429c7160867ad35fe338f028d5a9bb98e0296`.
Android builds use ARM64, API 24 and NDK 26.1.10909125 with 16 KiB ELF load alignment. Actual Android-device acceptance remains pending.
Original repository visibility and workflow activation were restored after the build. Documentation-only acceptance commits do not change the tested executable source.

## 2026-11-20 local migration validation

Independent release builds and the real five-route concurrent Windows Desk pressure test passed. Each route completed 500 MiB in each direction and 100 small-file round trips with zero decode/chunk errors. The four Gateway routes covered every directed WSS/STUN/TURN switch and in-flight RTC recovery. Native Host media reconnect restored decoding in 1,241 ms. Details: `E:/YooDeskGroup/gateway/docs/acceptance-20261120.md`.


## 2026-11-21 invitation startup and connection diagnostics

Desk invitation startup keeps the password form hidden during automatic validation and reveals it on authentication failure. The connection tooltip shows only TCP, UDP, TCP P2P or UDP P2P, sampled from the current selected carrier and ICE pair rather than the initial WebSocket snapshot. Diagnostics retain 180 credential-free events with transport transitions, ICE candidate types/protocols and byte counters, 10-second video/input timing samples and an export action. No invitation, password, grant, network address or input contents are included.

Validation: browser fixtures passed for valid/invalid invitations, password access, direct access, carrier changes and diagnostic export/redaction. A real isolated Global/Edge, Connector, WebService and released native Desk stack decoded video on LAN, WSS, STUN, TURN and automatic WSS-to-WebRTC upgrade; all five channel labels and diagnostic exports passed. These are local transport tests, not proof of cross-network STUN connectivity.


## 2026-11-21 viewer title and toolbar

After connection becomes active, the tab shows controlled hostName(ip), using backend machine metadata. The hidden top toolbar requires a stationary 1-second pointer dwell; movement resets it, leaving cancels it, and keyboard focus/pinning remain immediate. Browser fixtures cover dwell timing, movement and cancellation as well as invitation/authentication and diagnostics.


## 2026-11-21 reload recovery

Connector viewers retain only a server-issued five-minute refresh proof in per-device sessionStorage, never an access password or invitation secret. After refresh the normal device entry creates a fresh page authorization and the proof unlocks its new signed Connector scope. Proofs refresh with access renewal at most every two minutes, are cleared on logout/failed recovery, and expired proofs fall back to authentication. LAN continues to use its HttpOnly access cookie. Gateway SDK source is synchronized with its standalone Desk navigation fix. Real LAN, WSS, STUN, TURN and resume connections each recovered decoded video after two consecutive reloads; password-based remote WSS did too.


## 2026-11-21 automatic low-FPS diagnostics

The existing ten-second viewer performance snapshot is also sent to the authenticated controlled host. It contains only typed numeric timings/counters and carrier kinds, including safe candidate-pair counters; no password, invitation, SDP, addresses or input content. Older hosts returning 404/405 disable these submissions for the page. This supports investigation without manual diagnostic exports. Existing browser fixtures and real resume/reload sampling passed.


## 2026-11-21 MIPS plugin startup correction

WebService dynamic-plugin acceptance exposed startup failure in released MIPS plugins. Vendor the exact libc 0.2.191 archive with Connector's reviewed five MIPS64 architecture-selector corrections (socket options/types, signals, sigaction and pthread mutex ABI). Keep upstream licenses and PATCHES provenance. Remove the old MIPS socket-test exclusions. Add an exported-C-ABI startup regression that binds the actual plugin loopback listener, verifies missing transport token returns HTTP 403, stops the runtime and checks the listener is released. Full platform builds and WebService dynamic integration remain required before replacing released dependencies.

2026-11-25 shared transport sync: bounded browser receive/resumable windows and current API descriptions synchronized; bundled shared SFTP source aligned. Desk business streaming unchanged. 42 browser tests passed, including real JS/Rust resume replay. No release.

QEMU release acceptance (2026-11-25, explicit user decision): cross-compile, inspect target ABI/static or shared-library dependencies, and perform bounded startup/config/plugin-load smoke checks. Do not run full Rust/Go test suites or throughput gates under emulation. Existing native-runner regression policy is unchanged. Full-suite results and physical throughput must be established on native hardware.
