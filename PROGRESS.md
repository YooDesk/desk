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
