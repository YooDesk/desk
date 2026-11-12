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
