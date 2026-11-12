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
