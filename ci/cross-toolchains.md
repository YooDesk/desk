# YooDesk cross toolchains for the Linux ports (loong64, mips64el)

All artifacts are pinned by exact URL and SHA256 (or by immutable archive
timestamp); any mismatch fails the build. Host tools run inside the Ubuntu
18.04 container, so they only need glibc 2.27 compatibility on x86_64.

## Platform targets

- `linux-loong64` — Loongson 3A5000/3C5000+ desktop and server systems
  (Loongnix, Deepin, openEuler, Kylin, Debian). Rust target:
  `loongarch64-unknown-linux-gnu` (Tier 2, official std).
- `linux-mips64el` — Loongson 2K1000 systems running Loongnix
  "DaoXiangHu" (Debian 10 fork). Rust target:
  `mips64el-unknown-linux-gnuabi64` (Tier 3; std is built from rust-src with
  `-Z build-std=std,panic_abort`, the same approach EasyTier uses).

## Toolchains (ci/cross-tools.sh)

- loongarch64: official Loongson cross-tools 2025.02.21 (binutils 2.44,
  GCC 14.2.0, glibc 2.41 sysroot, x86_64 host).
- mips64el: Debian buster cross gcc-8 8.3.0 / binutils 2.31.1 with a
  glibc 2.28 sysroot, from the frozen archive.debian.org pool.
- User-mode QEMU for tests and smoke checks: Loongson's static
  qemu-loongarch64 and buster's qemu-mips64el-static. Invoked explicitly
  (no binfmt_misc in the container).

## Foreign sysroots (ci/cross-sysroot.sh — desk engine only)

The desk engine (RustDesk tree) links dynamically against the target
system's media stack through the `linux-pkg-config` feature chain:

- loongarch64: Debian sid loong64 packages from snapshot.debian.org at a
  pinned timestamp (reproducible). ffmpeg 9, libyuv, x264/x265, libvpx,
  opus, sodium, gtk3, gstreamer, pulse, alsa, va, vdpau, dbus, X11, drm,
  gbm — the complete set. glibc baseline 2.43.
- mips64el: Loongnix "DaoXiangHu" pool (frozen; the archive's apt indexes
  are empty, so packages are fetched by direct URL with recorded SHA256 —
  see cross-sysroot-mips64el.debs). ffmpeg 4.1.6, x264/x265, aom, opus,
  gtk3, gstreamer 1.14, pulse, alsa, dbus — the exact versions shipping on
  Loongson 2K systems. libyuv is missing from that archive and is built
  from source with the cross GCC (at the same commit the desk vcpkg
  manifest pins).

## bindgen strategy

hwcodec and scrap run bindgen 0.59 against libclang 10 (Ubuntu 18.04).
- mips64el: clang 10 knows the target; `--target=mips64el-linux-gnuabi64`.
- loongarch64: clang 10 predates LoongArch. Every bindgen'd header set in
  the tree (vpx, aom, yuv, ffmpeg_ffi, mux_ffi) is plain C with no
  arch-conditional layout, and loongarch64 is LP64 little-endian exactly
  like x86_64, so headers are parsed with `--target=x86_64` — the
  generated layouts match.

## glibc baselines per platform

- linux-x64 / linux-arm64: 2.27 (Ubuntu 18.04, unchanged)
- linux-loong64: 2.43 (Debian sid sysroot; every LoongArch distro ≥ 2.36)
- linux-mips64el: 2.28 (Debian buster / Loongnix DaoXiangHu)

## Verification performed before pinning (2026-11-10)

- ring 0.17.14 (TLS for every Rust component) falls back to its portable
  C implementation on unknown architectures (`asm_target = None` outside
  x86/x86_64/arm/aarch64; `sources_for_arch` then takes only the
  arch-independent C sources). No loongarch/mips assembly is referenced.
- openssl-src 300.5.3 (vendored openssl 3.5.4, used by hbb_common via
  reqwest native-tls) has explicit targets for both
  `loongarch64-unknown-linux-gnu` (linux-generic64) and
  `mips64el-unknown-linux-gnuabi64` (linux64-mips64).
- libsodium-sys 0.2.7 cross-builds its bundled source with CC/--host and
  skips `make check` when cross-compiling.
- libvpx 1.8.2 `configure` auto-detects `mips64el-linux-gnuabi64` from CC
  and endianness from a compiled probe; no explicit target string needed.
- The Loongnix mips64el pool really ships the pinned versions (checked by
  direct download + sha256); its apt indexes are empty, hence URL pinning.
- snapshot.debian.org serves the full sid loong64 archive at
  20261108T000000Z (both the Packages index and every resolved deb).

## CI environment requirements for the cross builds

The cross jobs download a SHA256-pinned standalone Python 3.13.18 package
from the sysroot release. Host tools stay separate from the target sysroot.

## Reusable native libraries

`ci/native-sdk.lock.json` pins the complete SDK archives by URL and SHA256.
The build downloads these archives before considering source recipes.
They include the target sysroot's headers and libraries, OpenH264 archives
and public headers, mozjpeg archives and headers, and the LoongArch DRM
runtime. MIPS also includes PIC static AOM 3.14.1 and libyuv.

SDK releases use `native-sdk-v1-linux-loong64` and
`native-sdk-v1-linux-mips64el`; they do not replace the latest desk plugin
release. Existing assets are immutable. Public OpenH264 headers are also
available as a separate archive for SDKs published before header packaging.

For an intentional dependency upgrade, bump the SDK version and dependency
pins, remove the affected platform's lock entry, run the cross build to
publish the new SDK, and record its actual URL and SHA256. Ordinary builds
keep the lock entries and use precompiled OpenH264, mozjpeg, DRM, AOM and
libyuv. Audits check the pkg-config closure, target headers, executable and
shared-library links, ELF architecture, and the runtime dependency closure.
