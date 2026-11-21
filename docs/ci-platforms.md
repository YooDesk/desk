# Additional release platforms

`windows-x86` uses `i686-pc-windows-msvc`, the `x86-windows-static`
vcpkg triplet and the x64-to-x86 MSVC environment. Rust, the engine and native
dependencies use the static CRT. `SODIUM_LIB_DIR` selects the target vcpkg
library, because libsodium-sys's bundled Windows library otherwise selects
the x64 host architecture. Release checks accept PE32 and PE32+ while rejecting
CRT and third-party DLL imports in both formats.

`linux-loong64-oldworld` is LoongArch ABI 1.0, with ELF flags `0x3`, glibc
2.28 and `/lib64/ld.so.1`. It has a separate platform name, cache and archive
from the existing new-world `linux-loong64` platform. Both use Rust's
`loongarch64-unknown-linux-gnu` target name; their compiler, standard library,
native dependencies and runtime must remain separate.

The old-world toolchain is published at
[oldworld-toolchain-v1](https://github.com/YooDesk/desk/releases/tag/oldworld-toolchain-v1).
`ci/oldworld-tools.lock.json` pins the exact archive and SHA256 for consumers.
The archive combines the previously verified Qt/CEF glibc 2.28 C toolchain,
Loongson Rust 1.99.0's x64 host compiler and matching ABI 1.0 standard library,
and Loongson QEMU commit `fe2ec9f4b69e6f6272ff72d68b07964bcd232ffd`.
The old-world QEMU branch implements the 128-signal ABI; the previously used
build-tools emulator rejects those signal operations with `EINVAL`.

The toolchain publication requires C and Rust execution, Rust TCP/filesystem/
thread checks, ELF/glibc audits, and agreement between the C headers and
crates.io libc's `stat`, `sigaction` and `sigset_t` layouts. Product tests run
inside the target filesystem with the source checkout mounted at its original
path, so the loader and tests see the same files. These checks exercise QEMU;
they do not establish desktop capture or GPU compatibility on real hardware.

`ci/oldworld-sysroot.lock.json` pins the Loongnix 20 package closure. AOM 3.14.1
and the engine's pinned libyuv revision are built statically against that
sysroot. Their licenses accompany the plugin archive. Loongnix 20's libdrm
2.4.97 lacks `drmModeFB2`, so this platform retains X11/Portal capture without
the optional DRM capture library, as the existing MIPS build does.

Single-platform workflow dispatches are diagnostic builds. The normal release
requires all ten platform archives and their checksums, with matching source
commit metadata. Native SDKs and toolchains have separate immutable release
tags, leaving the latest complete plugin release available to downstream jobs.
