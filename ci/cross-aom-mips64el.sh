#!/usr/bin/env bash
# Match the AOM API pinned by RustDesk's native vcpkg manifest. Loongnix's
# AOM 1.0 cannot supply its realtime AV1 encoder controls.
set -euo pipefail
root="$1"
tools=/opt/yoodesk-tools
libdir="$root/usr/lib/mips64el-linux-gnuabi64"
if [[ -f "$root/.aom-static-3.14.1" && -s "$libdir/libaom.a" ]]; then
    exit 0
fi
work="$tools/aom-mips64el-3.14.1"
mkdir -p "$work"
archive="$work/aom_3.14.1.orig.tar.xz"
sha=4ef9882eda023582499511b845f4d4368a34c9886f545a7eea4ca20799d3c161
if [[ ! -s "$archive" ]] || [[ "$(sha256sum "$archive" | cut -d' ' -f1)" != "$sha" ]]; then
    curl -fL --retry 3 --connect-timeout 30 \
        'https://deb.debian.org/debian/pool/main/a/aom/aom_3.14.1.orig.tar.xz' -o "$archive"
fi
echo "$sha  $archive" | sha256sum -c -
mkdir -p "$work/source"
tar -xJf "$archive" -C "$work/source"
cmake -S "$work/source" -B "$work/build" \
    -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=mips64el \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
    -DCMAKE_C_COMPILER="$tools/bin/mips64el-linux-gnuabi64-gcc" \
    -DCMAKE_CXX_COMPILER="$tools/bin/mips64el-linux-gnuabi64-g++" \
    -DCMAKE_SYSROOT="$root" \
    -DCMAKE_EXE_LINKER_FLAGS="-L$root/usr/mips64el-linux-gnuabi64/lib -Wl,-rpath-link,$root/usr/mips64el-linux-gnuabi64/lib" \
    -DCMAKE_SHARED_LINKER_FLAGS="-L$root/usr/mips64el-linux-gnuabi64/lib -Wl,-rpath-link,$root/usr/mips64el-linux-gnuabi64/lib" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DCMAKE_INSTALL_PREFIX=/usr -DCMAKE_INSTALL_LIBDIR=lib/mips64el-linux-gnuabi64 \
    -DAOM_TARGET_CPU=generic -DBUILD_SHARED_LIBS=OFF \
    -DENABLE_DOCS=OFF -DENABLE_EXAMPLES=OFF -DENABLE_TESTDATA=OFF \
    -DENABLE_TESTS=OFF -DENABLE_TOOLS=OFF
cmake --build "$work/build" --target aom --parallel "$(nproc)"
DESTDIR="$root" cmake --install "$work/build"
# AOM versions differ in whether GNUInstallDirs is honored.
if [[ -f "$root/usr/lib/libaom.a" ]]; then
    mkdir -p "$libdir"
    mv "$root/usr/lib/libaom.a" "$libdir/libaom.a"
fi
test -s "$libdir/libaom.a"
mkdir -p "$libdir/pkgconfig"
cat > "$libdir/pkgconfig/aom.pc" <<'EOF'
prefix=/usr
libdir=${prefix}/lib/mips64el-linux-gnuabi64
includedir=${prefix}/include
Name: aom
Description: AOM AV1 codec with realtime encoding support
Version: 3.14.1
Libs: -L${libdir} -laom
Libs.private: -lm -lpthread
Cflags: -I${includedir}
EOF
mkdir -p "$root/usr/share/doc/libaom-dev"
cp "$work/source/LICENSE" "$root/usr/share/doc/libaom-dev/LICENSE.static-aom"
touch "$root/.aom-static-3.14.1"
