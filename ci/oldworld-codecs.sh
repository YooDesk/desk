#!/usr/bin/env bash
# Match the pinned engine's codec APIs while retaining the Loongnix 20 ABI.
set -euo pipefail
tools="${YOODESK_OLDWORLD_TOOLS:?old-world tools required}"
root="$tools/toolchain/sysroot"
libdir="$root/usr/lib/loongarch64-linux-gnu"
work="$tools/codec-build"
mkdir -p "$work" "$libdir/pkgconfig"
fetch() {
    local url="$1" sha="$2" path="$3"
    if [[ ! -f "$path" ]] || ! echo "$sha  $path" | sha256sum -c - >/dev/null 2>&1; then
        curl -fL --retry 3 --connect-timeout 30 "$url" -o "$path"
        echo "$sha  $path" | sha256sum -c -
    fi
}
cmake_common=(
    -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=loongarch64
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY
    -DCMAKE_C_COMPILER="$tools/toolchain/bin/clang"
    -DCMAKE_CXX_COMPILER="$tools/toolchain/bin/clang++"
    -DCMAKE_AR="$tools/toolchain/llvm/bin/llvm-ar"
    -DCMAKE_RANLIB="$tools/toolchain/llvm/bin/llvm-ranlib"
    -DCMAKE_SYSROOT="$root" -DCMAKE_POSITION_INDEPENDENT_CODE=ON
    -DCMAKE_C_FLAGS=-mcmodel=large -DCMAKE_CXX_FLAGS=-mcmodel=large
    -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER
    -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY
    -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_INSTALL_PREFIX=/usr -DCMAKE_INSTALL_LIBDIR=lib/loongarch64-linux-gnu
    -DBUILD_SHARED_LIBS=OFF
)
if [[ ! -f "$root/.yoodesk-libyuv-d98915a" ]]; then
    fetch 'https://codeload.github.com/lemenkov/libyuv/tar.gz/d98915a654d3564e4802a0004add46221c4e4348' \
        '7c14bda4ae0283b1e12e678f1d81a4dac0d7799ec8b2101de3e8619431b80095' "$work/yuv.tar.gz"
    mkdir -p "$work/yuv-source"
    tar -xzf "$work/yuv.tar.gz" -C "$work/yuv-source" --strip-components=1
    cmake -S "$work/yuv-source" -B "$work/yuv-build" "${cmake_common[@]}" \
        -DCMAKE_DISABLE_FIND_PACKAGE_JPEG=ON
    cmake --build "$work/yuv-build" --target yuv --parallel "$(nproc)"
    cp "$work/yuv-build/libyuv.a" "$libdir/libyuv.a"
    cp -a "$work/yuv-source/include/." "$root/usr/include/"
    mkdir -p "$root/usr/share/doc/yoodesk-codecs"
    cp "$work/yuv-source/LICENSE" "$root/usr/share/doc/yoodesk-codecs/LICENSE.static-yuv"
    touch "$root/.yoodesk-libyuv-d98915a"
fi
if [[ ! -f "$root/.yoodesk-aom-3.14.1" ]]; then
    fetch 'https://deb.debian.org/debian/pool/main/a/aom/aom_3.14.1.orig.tar.xz' \
        '4ef9882eda023582499511b845f4d4368a34c9886f545a7eea4ca20799d3c161' "$work/aom.tar.xz"
    mkdir -p "$work/aom-source"
    tar -xJf "$work/aom.tar.xz" -C "$work/aom-source"
    cmake -S "$work/aom-source" -B "$work/aom-build" "${cmake_common[@]}" \
        -DAOM_TARGET_CPU=generic -DENABLE_DOCS=OFF -DENABLE_EXAMPLES=OFF \
        -DENABLE_TESTDATA=OFF -DENABLE_TESTS=OFF -DENABLE_TOOLS=OFF
    cmake --build "$work/aom-build" --parallel "$(nproc)"
    DESTDIR="$root" cmake --install "$work/aom-build"
    if [[ -f "$root/usr/lib/libaom.a" ]]; then
        mv "$root/usr/lib/libaom.a" "$libdir/libaom.a"
    fi
    cat "$work/aom-source/LICENSE" "$work/aom-source/PATENTS" \
        > "$root/usr/share/doc/yoodesk-codecs/LICENSE.static-aom"
    touch "$root/.yoodesk-aom-3.14.1"
fi
for codec in libyuv aom; do
    library=yuv
    version=$(awk '/^#define LIBYUV_VERSION / {print $3}' "$root/usr/include/libyuv/version.h")
    [[ "$codec" != aom ]] || { library=aom; version=3.14.1; }
    cat > "$libdir/pkgconfig/$codec.pc" <<EOF
prefix=/usr
libdir=\${prefix}/lib/loongarch64-linux-gnu
includedir=\${prefix}/include
Name: $codec
Description: YooDesk old-world static $codec
Version: $version
Libs: -L\${libdir} -l$library
Cflags: -I\${includedir}
EOF
done
