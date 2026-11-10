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
for attempt in 1 2 3; do
    apt-get install -y --no-install-recommends \
        build-essential gcc-8 g++-8 ca-certificates curl git unzip zip xz-utils \
        pkg-config python3 binutils file dpkg autoconf automake libtool ninja-build nasm \
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
export PATH="$tools/bin:$PATH"
export LIBCLANG_PATH=/usr/lib/llvm-10/lib
export VCPKG_MAX_CONCURRENCY=3 CARGO_BUILD_JOBS=3

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
bash "$(dirname "$0")/cross-sysroot.sh" "$kind" "$tools"

if [[ ! -x "$tools/bin/python3.13" ]]; then
    curl -fL --retry 3 https://www.python.org/ftp/python/3.13.5/Python-3.13.5.tgz -o /tmp/python.tgz
    tar -xf /tmp/python.tgz -C /tmp
    pushd /tmp/Python-3.13.5
    ./configure --prefix="$tools" --with-ensurepip=install
    make -j3
    make install
    popd
fi
export PATH="$tools/bin:$PATH"
python3.13 -m pip install 'meson==1.9.0'

if [[ ! -x "$tools/bin/cargo" ]]; then
    curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-${RUST_VERSION:-1.99.0}-x86_64-unknown-linux-gnu.tar.xz" -o /tmp/rust.tar.xz
    tar -xf /tmp/rust.tar.xz -C /tmp
    "/tmp/rust-${RUST_VERSION:-1.99.0}-x86_64-unknown-linux-gnu/install.sh" --prefix="$tools" --without=rust-docs
fi
export PATH="$tools/bin:$PATH"
export CARGO_HOME=/workspace/.ci-cache/cargo
export CARGO_BUILD_TARGET="$target"

cd /workspace
git config --global --add safe.directory /workspace
git config --global --add safe.directory /workspace/desk-engine/target/drmtap/source

sysroot_loong="$tools/sysroot-loong64/usr"
sysroot_mips="$tools/sysroot-mips64el/usr/mips64el-linux-gnuabi64"

case "$platform" in
linux-loong64)
    if [[ ! -d "$tools/lib/rustlib/$target" ]]; then
        curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION:-1.99.0}-loongarch64-unknown-linux-gnu.tar.xz" -o /tmp/rust-std.tar.xz
        tar -xf /tmp/rust-std.tar.xz -C /tmp
        "/tmp/rust-std-${RUST_VERSION:-1.99.0}-loongarch64-unknown-linux-gnu/install.sh" --prefix="$tools" --without=rust-docs
    fi
    export CC="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-gcc"
    export CXX="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-g++"
    export AR="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-ar"
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER="$CC"
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_AR="$AR"
    # Pass the foreign sysroot to the GCC linker driver only (rustc's own
    # --sysroot would redirect std discovery away from the installed target).
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--sysroot=$tools/sysroot-loong64 -C link-arg=-L$sysroot_loong/lib/loong64-linux-gnu -C link-arg=-Wl,-rpath-link,$sysroot_loong/lib/loong64-linux-gnu"
    export QEMU="$tools/bin/qemu-loongarch64"
    export QEMU_LD_PREFIX="$tools/sysroot-loong64/usr"
    export PKG_CONFIG_PATH="$sysroot_loong/lib/loong64-linux-gnu/pkgconfig:$sysroot_loong/share/pkgconfig"
    export PKG_CONFIG_SYSROOT_DIR="$tools/sysroot-loong64"
    export PKG_CONFIG_LIBDIR="$sysroot_loong/lib/loong64-linux-gnu/pkgconfig"
    # bindgen parses vpx/aom/yuv headers with clang-10, which has no
    # loongarch target. All our targets are LP64 little-endian, so layouts
    # match x86_64; parse with that triple using the sysroot headers.
    export BINDGEN_EXTRA_CLANG_ARGS="--target=x86_64-unknown-linux-gnu --sysroot=$tools/sysroot-loong64"
    # meson cross file for libdrmtap: meson must not run target probes.
    cat > "$tools/meson-cross-loong64.ini" <<EOF
[binaries]
c = '$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-gcc'
cpp = '$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-g++'
ar = '$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-ar'
strip = '$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-strip'
pkgconfig = 'pkg-config'
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
    export CC="$tools/bin/mips64el-linux-gnuabi64-gcc"
    export CXX="$tools/bin/mips64el-linux-gnuabi64-g++"
    export AR="$tools/bin/mips64el-linux-gnuabi64-ar"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_LINKER="$CC"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_AR="$AR"
    # Same linker-driver-only sysroot passing as the loong64 branch above.
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_RUSTFLAGS="-C link-arg=--sysroot=$tools/sysroot-mips64el -C link-arg=-L$sysroot_mips/lib -C link-arg=-L$sysroot_mips/lib/mips64el-linux-gnuabi64 -C link-arg=-Wl,-rpath-link,$sysroot_mips/lib"
    export QEMU="$tools/bin/qemu-mips64el"
    export QEMU_LD_PREFIX="$tools/sysroot-mips64el"
    export PKG_CONFIG_PATH="$sysroot_mips/lib/pkgconfig:$sysroot_mips/lib/mips64el-linux-gnuabi64/pkgconfig"
    export PKG_CONFIG_SYSROOT_DIR="$tools/sysroot-mips64el"
    export PKG_CONFIG_LIBDIR="$sysroot_mips/lib/pkgconfig:$sysroot_mips/lib/mips64el-linux-gnuabi64/pkgconfig"
    # clang-10 supports mips64el directly.
    export BINDGEN_EXTRA_CLANG_ARGS="--target=mips64el-linux-gnuabi64 --sysroot=$sysroot_mips"
    cat > "$tools/meson-cross-mips64el.ini" <<EOF
[binaries]
c = '$tools/bin/mips64el-linux-gnuabi64-gcc'
cpp = '$tools/bin/mips64el-linux-gnu-g++'
ar = '$tools/bin/mips64el-linux-gnuabi64-ar'
strip = '$tools/bin/mips64el-linux-gnuabi64-strip'
pkgconfig = 'pkg-config'
[properties]
needs_exe_wrapper = true
[host_machine]
system = 'linux'
cpu_family = 'mips64el'
cpu = 'mips64'
endian = 'little'
EOF
    export YOODESK_MESON_CROSS_FILE="$tools/meson-cross-mips64el.ini"
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
python3.13 scripts/prepare_sources.py
python3.13 scripts/prepare_linux_native.py
python3.13 scripts/patch_hwcodec.py

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
