#!/usr/bin/env bash
# Cross-compile the desk plugin WITH the RustDesk engine for loongarch64 or
# mips64el inside the Ubuntu 18.04 container.
#
# Usage: ci/cross-engine-build.sh <platform> <target>
#
# This complements ci/cross-build.sh (which builds the engine-less desk
# plugin for the other component repos): desk itself needs the full engine,
# so this script drives scripts/build_engine.py against the foreign sysroot
# and then builds the plugin with default features.
#
# Toolchain pairings (see ci/cross-toolchains.md):
#   loongarch64: Loongson GCC 14.2 cross + Debian sid loong64 sysroot
#                (snapshot-pinned, glibc 2.43, ffmpeg 9, full X/GStreamer set)
#   mips64el:    Debian buster gcc-8 cross + Loongnix DaoXiangHu sysroot
#                (glibc 2.28, ffmpeg 4.1.6 — the versions shipping on
#                Loongson 2K systems)
set -euo pipefail
platform="$1"
target="$2"

export DEBIAN_FRONTEND=noninteractive
# Retry individual downloads, rather than fetching the full dependency set
# again after one HTTP connection fails. Keep downloaded packages for retries.
cat > /etc/apt/apt.conf.d/80yoodesk-downloads <<'EOF'
Acquire::Retries "3";
Acquire::http::Timeout "20";
Acquire::https::Timeout "20";
Binary::apt-get::APT::Keep-Downloaded-Packages "true";
EOF
# archive.ubuntu.com is unreliable from CI runners (connection refused or
# timeout). Canonical's Azure mirror serves the same bionic archive.
if [[ -f /etc/apt/sources.list ]]; then
    sed -i 's|http://archive.ubuntu.com/ubuntu|http://azure.archive.ubuntu.com/ubuntu|g; s|http://security.ubuntu.com/ubuntu|http://azure.archive.ubuntu.com/ubuntu|g' /etc/apt/sources.list
fi
# archive.ubuntu.com occasionally refuses connections from runners; retry
# the index update before declaring the build environment unusable.
for attempt in 1 2 3; do
    apt-get update && break
    if [[ "$attempt" == 3 ]]; then
        echo "apt-get update failed after 3 attempts" >&2
        exit 1
    fi
    sleep $((attempt * 15))
done
# The base container has no CA bundle. Bootstrap that small dependency first,
# then use TLS for the large download to avoid the mirror's HTTP proxy failures.
apt-get install -y --no-install-recommends ca-certificates
sed -i 's|http://azure.archive.ubuntu.com/ubuntu|https://azure.archive.ubuntu.com/ubuntu|g' /etc/apt/sources.list
for attempt in 1 2 3; do
    apt-get install -y --no-install-recommends \
        build-essential gcc-8 g++-8 ca-certificates curl git unzip zip xz-utils \
        pkg-config python3 binutils file dpkg autoconf automake libtool ninja-build nasm cmake \
        clang-10 libclang-10-dev llvm-10-dev libasound2-dev libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev libgtk-3-dev libpulse-dev libva-dev \
        libxcb-randr0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxcb-shm0-dev \
        libxdo-dev libxfixes-dev libxrandr-dev libxtst-dev libxkbcommon-dev \
        libudev-dev libdbus-1-dev libegl1-mesa-dev libgles2-mesa-dev libgbm-dev \
        libdrm-dev meson python3-pip zlib1g-dev libffi-dev libssl-dev libbz2-dev \
        liblzma-dev libreadline-dev libsqlite3-dev && break
    if [[ "$attempt" == 3 ]]; then
        echo "dependency install failed after 3 attempts" >&2
        exit 1
    fi
    apt-get update && sleep $((attempt * 15))
done

tools=/opt/yoodesk-tools
mkdir -p "$tools"
# Compiler wrappers: they add the target sysroot and the multiarch
# include/library directories, and drop -m64 (which cc-rs derives from the
# build host). The implementation is scripts/cc-wrapper.sh so the quoting
# rules are testable; bash arrays keep values like -DOPENSSL_PIC="1" intact,
# which a plain string join would split.
cp /workspace/scripts/cc-wrapper.sh "$tools/cc-wrapper"
chmod +x "$tools/cc-wrapper"
for entry in     "loong64:/opt/yoodesk-tools/cross-tools/bin/loongarch64-unknown-linux-gnu:/opt/yoodesk-tools/sysroot-loong64:loongarch64-linux-gnu"     "mips64el:/opt/yoodesk-tools/bin/mips64el-linux-gnuabi64:/opt/yoodesk-tools/sysroot-mips64el:mips64el-linux-gnuabi64"; do
    IFS=: read -r suffix bindir sysroot multiarch <<< "$entry"
    for language in cc cxx; do
        case "$language" in
            cc) compiler="$bindir-gcc" ;;
            cxx) compiler="$bindir-g++" ;;
        esac
        wrapper="$tools/meson-${language}-${suffix}"
        {
            echo '#!/bin/bash'
            echo "exec /opt/yoodesk-tools/cc-wrapper '$compiler' '$sysroot' '$multiarch' \"\$@\""
        } > "$wrapper"
        chmod +x "$wrapper"
    done
done

case "$platform" in
linux-loong64)
    kind=loongarch64
    ;;
linux-mips64el)
    kind=mips64el
    ;;
*)
    echo "Unsupported cross platform: $platform" >&2
    exit 1
    ;;
esac

bash "$(dirname "$0")/cross-tools.sh" "$kind" "$tools"

# Prebuilt CPython (python-build-standalone) instead of a 13-minute source
# build: the build runs in a fresh container on every job, and the standalone
# build targets glibc 2.17+, so it runs on this 18.04 image unchanged.
if [[ ! -x "$tools/bin/python3.13" ]]; then
    python_tarball=/tmp/python-standalone.tar.gz
    python_url='https://github.com/YooDesk/desk/releases/download/sysroot-mips64el-loongnix-d10/python-3.13.18-standalone.tar.gz'
    python_sha='126343c54daff4068efc1ac269518d0cc202d57857794948103dd1454e8881be'
    curl -fL --retry 3 "$python_url" -o "$python_tarball"
    echo "$python_sha  $python_tarball" | sha256sum -c -
    mkdir -p "$tools"
    tar -xzf "$python_tarball" -C "$tools" --strip-components=1
    rm -f "$python_tarball"
fi
export PATH="$tools/bin:$PATH"
python3.13 -m pip install 'meson==1.9.0'
# Ubuntu 18.04 ships CMake 3.10, below libyuv's minimum; install a
# current CMake into the tools prefix and prefer it on PATH.
if [[ ! -x "$tools/bin/cmake" ]]; then
    python3.13 -m pip install 'cmake==3.31.6'
fi

# The sysroot build compiles libyuv with CMake, so it must run after the
# tooling above is in place.
bash "$(dirname "$0")/cross-sysroot.sh" "$kind" "$tools"

if [[ ! -x "$tools/bin/cargo" ]]; then
    curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-${RUST_VERSION:-1.99.0}-x86_64-unknown-linux-gnu.tar.xz" -o /tmp/rust.tar.xz
    tar -xf /tmp/rust.tar.xz -C /tmp
    "/tmp/rust-${RUST_VERSION:-1.99.0}-x86_64-unknown-linux-gnu/install.sh" --prefix="$tools" --without=rust-docs
fi
export PATH="$tools/bin:$PATH"
export CARGO_HOME=/workspace/.ci-cache/cargo
export CARGO_BUILD_TARGET="$target"
# Build scripts are compiled for the host; pin the host compiler under the
# name cc-rs actually reads so they never inherit the target cross compiler.
export HOST_CC=/usr/bin/gcc-8
export HOST_CXX=/usr/bin/g++-8
export CC_x86_64_unknown_linux_gnu=/usr/bin/gcc-8
export CXX_x86_64_unknown_linux_gnu=/usr/bin/g++-8
export AR_x86_64_unknown_linux_gnu=/usr/bin/ar
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/gcc-8
# Native build dependencies (libgit2/libz) must probe the Ubuntu host,
# while the engine's target dependencies keep the foreign PKG_CONFIG_*.
export HOST_PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
export HOST_PKG_CONFIG_LIBDIR="$HOST_PKG_CONFIG_PATH"
export HOST_PKG_CONFIG_SYSROOT_DIR=/

cd /workspace
git config --global --add safe.directory /workspace
git config --global --add safe.directory /workspace/desk-engine/target/drmtap/source

sysroot_loong="$tools/sysroot-loong64/usr"
sysroot_mips="$tools/sysroot-mips64el/usr/lib/mips64el-linux-gnuabi64"

case "$platform" in
linux-loong64)
    if [[ ! -d "$tools/lib/rustlib/$target" ]]; then
        curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION:-1.99.0}-loongarch64-unknown-linux-gnu.tar.xz" -o /tmp/rust-std.tar.xz
        tar -xf /tmp/rust-std.tar.xz -C /tmp
        "/tmp/rust-std-${RUST_VERSION:-1.99.0}-loongarch64-unknown-linux-gnu/install.sh" --prefix="$tools" --without=rust-docs
    fi
    export CC="$tools/meson-cc-loong64"
    export CXX="$tools/meson-cxx-loong64"
    export CC_loongarch64_unknown_linux_gnu="$tools/meson-cc-loong64"
    export CXX_loongarch64_unknown_linux_gnu="$tools/meson-cxx-loong64"
    export AR="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-ar"
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER="$CC"
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_AR="$AR"
    # Pass the foreign sysroot to the GCC linker driver only (rustc's own
    # --sysroot would redirect std discovery away from the installed target).
    # Link paths come from the compiler wrapper (target-only).
    export QEMU="$tools/bin/qemu-loongarch64"
    export QEMU_LD_PREFIX="$tools/sysroot-loong64/usr"
    export PKG_CONFIG_PATH="$sysroot_loong/lib/loongarch64-linux-gnu/pkgconfig:$sysroot_loong/share/pkgconfig"
    export PKG_CONFIG_SYSROOT_DIR="$tools/sysroot-loong64"
    export PKG_CONFIG_LIBDIR="$sysroot_loong/lib/loongarch64-linux-gnu/pkgconfig:$sysroot_loong/share/pkgconfig"
    # bindgen parses vpx/aom/yuv headers with clang-10, which has no
    # loongarch target. All our targets are LP64 little-endian, so layouts
    # match x86_64; parse with that triple using the sysroot headers.
    export BINDGEN_EXTRA_CLANG_ARGS="--target=x86_64-unknown-linux-gnu --sysroot=$tools/sysroot-loong64"
    # meson cross file for libdrmtap: meson must not run target probes.
    cat > "$tools/meson-cross-loong64.ini" <<EOF
[binaries]
c = '$tools/meson-cc-loong64'
cpp = '$tools/meson-cxx-loong64'
ar = '$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-ar'
strip = '$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-strip'
pkg-config = 'pkg-config'
[properties]
needs_exe_wrapper = true
[host_machine]
system = 'linux'
cpu_family = 'loongarch64'
cpu = 'loongarch64'
endian = 'little'
EOF
    export YOODESK_MESON_CROSS_FILE="$tools/meson-cross-loong64.ini"
    build_args=()
    ;;
linux-mips64el)
    if [[ ! -d "$tools/lib/rustlib/src/rust" ]]; then
        curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-src-${RUST_VERSION:-1.99.0}.tar.xz" -o /tmp/rust-src.tar.xz
        tar -xf /tmp/rust-src.tar.xz -C /tmp
        "/tmp/rust-src-${RUST_VERSION:-1.99.0}/install.sh" --prefix="$tools"
    fi
    export CC="$tools/meson-cc-mips64el"
    export CXX="$tools/meson-cxx-mips64el"
    export CC_mips64el_unknown_linux_gnuabi64="$tools/meson-cc-mips64el"
    export CXX_mips64el_unknown_linux_gnuabi64="$tools/meson-cxx-mips64el"
    export AR="$tools/bin/mips64el-linux-gnuabi64-ar"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_LINKER="$CC"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_AR="$AR"
    # The linker driver gets the Loongnix sysroot plus the toolchain's own
    # multiarch libdir, which is where libgcc_s.so.1 lives.
    mips_libdir="$tools/mips-root/usr/mips64el-linux-gnuabi64/lib"
    # Link paths come from the compiler wrapper (target-only).
    export QEMU="$tools/bin/qemu-mips64el"
    # Debian's cross binutils keeps libopcodes/libbfd for the host in the
    # multiarch directory; `as` and `ld` fail to start without it.
    export LD_LIBRARY_PATH="$tools/mips-root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export QEMU_LD_PREFIX="$tools/sysroot-mips64el"
    export PKG_CONFIG_PATH="$sysroot_mips/pkgconfig:$tools/sysroot-mips64el/usr/share/pkgconfig"
    export PKG_CONFIG_SYSROOT_DIR="$tools/sysroot-mips64el"
    # LIBDIR overrides PATH entirely, so it must list both directories:
    # the X11 protocol .pc files live under usr/share/pkgconfig.
    export PKG_CONFIG_LIBDIR="$sysroot_mips/pkgconfig:$tools/sysroot-mips64el/usr/share/pkgconfig"
    # clang-10 supports mips64el directly.
    export BINDGEN_EXTRA_CLANG_ARGS="--target=mips64el-linux-gnuabi64 --sysroot=$sysroot_mips"
    cat > "$tools/meson-cross-mips64el.ini" <<EOF
[binaries]
c = '$tools/meson-cc-mips64el'
cpp = '$tools/meson-cxx-mips64el'
ar = '$tools/bin/mips64el-linux-gnuabi64-ar'
strip = '$tools/bin/mips64el-linux-gnuabi64-strip'
pkg-config = 'pkg-config'
[properties]
needs_exe_wrapper = true
[host_machine]
system = 'linux'
cpu_family = 'mips64el'
cpu = 'mips64'
endian = 'little'
EOF
    export YOODESK_MESON_CROSS_FILE="$tools/meson-cross-mips64el.ini"
    # Loongnix ships libdrm 2.4.97; drmtap needs drmModeFB2 (2.4.99+), so the
    # DRM capture runtime is not built for this port. X11/Wayland capture
    # inside the engine still works.
    export YOODESK_SKIP_DRMTAP=1
    export RUSTC_BOOTSTRAP=1
    # The engine workspace builds std from source too: no prebuilt mips64 std.
    export YOODESK_ENGINE_CARGO_ARGS="-Z build-std=std,panic_abort"
    build_args=(-Z build-std=std,panic_abort)
    ;;
esac

rustc --version
"$QEMU" --version | head -n1

# Source preparation shared with the native Linux flow: engine patches,
# hwcodec source, libdrmtap. linux-pkg-config probes resolve through the
# sysroot via PKG_CONFIG_* set above.
# Diagnose pkg-config resolution once, so a failure names the missing path.
echo "--- pkg-config probe (loong64/mips64el sysroot) ---"
echo "PKG_CONFIG_PATH=$PKG_CONFIG_PATH"
echo "PKG_CONFIG_LIBDIR=$PKG_CONFIG_LIBDIR"
echo "PKG_CONFIG_SYSROOT_DIR=$PKG_CONFIG_SYSROOT_DIR"
ls -1 "$PKG_CONFIG_LIBDIR" 2>/dev/null | head -5 || echo "(libdir missing)"
PKG_CONFIG_PATH="$PKG_CONFIG_PATH" PKG_CONFIG_SYSROOT_DIR="$PKG_CONFIG_SYSROOT_DIR" pkg-config --modversion libdrm || echo "libdrm.pc NOT FOUND"
pkg-config --variable pc_path pkg-config 2>/dev/null | head -1 || true
echo "--- end probe ---"
python3.13 scripts/prepare_sources.py
python3.13 scripts/prepare_linux_native.py
python3.13 scripts/patch_hwcodec.py
# Materialize the patched nix crate (adds loongarch64 ioctl constants)
python3.13 scripts/prepare_nix.py

# Build the engine static library for the cross target.
python3.13 scripts/build_engine.py

export YOODESK_DESK_ENGINE_LIB_DIR="/workspace/desk-engine/target/$target/release"
export YOODESK_DESK_ENGINE=1

runner_var="CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_RUNNER"
export "$runner_var=$QEMU"

cargo build --locked --release "${build_args[@]}" --lib
cargo test --locked "${build_args[@]}" --lib
python3.13 ci/package.py desk --platform "$platform"

# Tools validated end to end; let the workflow's tools-cache save step keep
# the cross toolchain and sysroot (both are resumable via their markers).
printf "%s
" "cmake-meson rust-1.99.0 cross-$platform sysroot" > "$tools/.ready"
