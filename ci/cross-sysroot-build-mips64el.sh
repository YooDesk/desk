#!/usr/bin/env bash
# Build the one native library missing from the Loongnix archive (libyuv)
# with the buster mips64el cross GCC and install it into the sysroot.
# libyuv builds with plain cmake and no assembly hazards.
#
# Usage: ci/cross-sysroot-build-mips64el.sh <sysroot-root>
set -euo pipefail
root="$1"
tools=/opt/yoodesk-tools
cc="$tools/bin/mips64el-linux-gnuabi64-gcc"
cxx="$tools/bin/mips64el-linux-gnuabi64-g++"
[[ -x "$cc" ]] || { echo "mips64el cross GCC missing; run cross-tools.sh first" >&2; exit 1; }

work="$root/.build"
mkdir -p "$work/src"

# libyuv, pinned to the exact commit the desk vcpkg manifest uses, fetched
# from the GitHub mirror (googlesource is not reachable from every network).
if [[ ! -e "$root/usr/lib/mips64el-linux-gnuabi64/libyuv.a" && ! -e "$root/usr/lib/mips64el-linux-gnuabi64/libyuv.so" ]]; then
    curl -fL --retry 3 'https://github.com/lemenkov/libyuv/archive/d98915a654d3564e4802a0004add46221c4e4348.tar.gz' -o "$work/src/libyuv.tar.gz"
    rm -rf "$work/libyuv"
    mkdir -p "$work/libyuv"
    tar -xzf "$work/src/libyuv.tar.gz" -C "$work/libyuv" --strip-components=1
    cd "$work/libyuv"
    mkdir -p build && cd build
    cmake .. -DCMAKE_SYSTEM_NAME=Linux \
        -DCMAKE_SYSTEM_PROCESSOR=mips64el \
        -DCMAKE_C_COMPILER="$cc" -DCMAKE_CXX_COMPILER="$cxx" \
        -DCMAKE_SYSROOT="$root/usr/mips64el-linux-gnuabi64" \
        -DCMAKE_INSTALL_PREFIX=/usr \
        -DCMAKE_INSTALL_LIBDIR=lib/mips64el-linux-gnuabi64 \
        -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON
    make -j"$(nproc)" yuv
    make install
    # Move from the plain /usr/lib layout into the multiarch dir if cmake
    # ignored CMAKE_INSTALL_LIBDIR above.
    if [[ -e "$root/usr/lib/libyuv.so" ]]; then
        mkdir -p "$root/usr/lib/mips64el-linux-gnuabi64"
        mv "$root/usr/lib/libyuv.so"* "$root/usr/lib/mips64el-linux-gnuabi64/"
        mv "$root/usr/lib/libyuv.a" "$root/usr/lib/mips64el-linux-gnuabi64/" 2>/dev/null || true
    fi
fi

rm -rf "$work"
