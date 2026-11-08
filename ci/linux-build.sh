#!/usr/bin/env bash
set -euo pipefail
kind="$1"
platform="$2"
target="$3"
triplet="$4"
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential gcc-8 g++-8 ca-certificates curl git unzip zip xz-utils pkg-config libssl-dev zlib1g-dev libffi-dev libbz2-dev liblzma-dev libreadline-dev libsqlite3-dev autoconf automake libtool ninja-build nasm musl
export CC=gcc-8 CXX=g++-8
export VCPKG_MAX_CONCURRENCY=3 CARGO_BUILD_JOBS=3
# Never acquire host executables built against a newer glibc.
export VCPKG_FORCE_SYSTEM_BINARIES=1
mkdir -p /opt/yoodesk-tools
if [[ ! -x /opt/yoodesk-tools/bin/python3.13 ]]; then
    curl -fL --retry 3 https://www.python.org/ftp/python/3.13.5/Python-3.13.5.tgz -o /tmp/python.tgz
    tar -xf /tmp/python.tgz -C /tmp
    pushd /tmp/Python-3.13.5
    ./configure --prefix=/opt/yoodesk-tools --with-ensurepip=install
    make -j3
    make install
    popd
fi
export PATH="/opt/yoodesk-tools/bin:$PATH"
if [[ ! -x /opt/yoodesk-tools/bin/cargo ]]; then
    curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-1.99.0-${target}.tar.xz" -o /tmp/rust.tar.xz
    tar -xf /tmp/rust.tar.xz -C /tmp
    "/tmp/rust-1.99.0-${target}/install.sh" --prefix=/opt/yoodesk-tools --without=rust-docs
fi
cd /workspace
git config --global --add safe.directory /workspace
# Actions restores this pinned checkout as the runner user; the Ubuntu container runs as root.
git config --global --add safe.directory /workspace/desk-engine/target/drmtap/source
export CARGO_BUILD_TARGET="$target"
if [[ "$kind" == desk ]]; then
    # System dependency list and LLVM/vcpkg pins come from RustDesk's flutter-build.yml.
    apt-get install -y --no-install-recommends clang-10 libclang-10-dev llvm-10-dev libasound2-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgtk-3-dev libpulse-dev libva-dev libxcb-randr0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxcb-shm0-dev libxdo-dev libxfixes-dev libxrandr-dev libxtst-dev libxkbcommon-dev libudev-dev libdbus-1-dev libegl1-mesa-dev libgles2-mesa-dev libgbm-dev libdrm-dev
    export LIBCLANG_PATH=/usr/lib/llvm-10/lib
    python3.13 -m pip install 'meson==1.9.0'
    # The pinned vcpkg requires CMake 4.4, whose bundled curl requires OpenSSL 3; Ubuntu 18.04 provides OpenSSL 1.1.
    # Build this tooling dependency against the same old glibc instead of changing the build image.
    if [[ ! -f /opt/yoodesk-tools/openssl/lib/libssl.a ]]; then
        curl -fL --retry 3 https://github.com/openssl/openssl/releases/download/openssl-3.5.4/openssl-3.5.4.tar.gz -o /tmp/openssl.tar.gz
        tar -xf /tmp/openssl.tar.gz -C /tmp
        pushd /tmp/openssl-3.5.4
        ./Configure no-shared no-tests --prefix=/opt/yoodesk-tools/openssl --libdir=lib
        make -j3
        make install_sw
        popd
    fi
    if [[ ! -x /opt/yoodesk-tools/bin/cmake ]] || [[ "$(cmake --version | head -n1)" != "cmake version 4.4.0" ]]; then
        curl -fL --retry 3 https://github.com/Kitware/CMake/releases/download/v4.4.0/cmake-4.4.0.tar.gz -o /tmp/cmake.tar.gz
        tar -xf /tmp/cmake.tar.gz -C /tmp
        pushd /tmp/cmake-4.4.0
        ./bootstrap --prefix=/opt/yoodesk-tools --parallel=3 -- -DCMAKE_USE_OPENSSL=ON -DOPENSSL_ROOT_DIR=/opt/yoodesk-tools/openssl -DOPENSSL_USE_STATIC_LIBS=TRUE
        make -j3
        make install
        popd
    fi
    if [[ "$platform" == linux-x64 && ! -x /opt/yoodesk-tools/bin/nasm ]]; then
        # CMake's NASM include flags require the modern directory handling used by vcpkg.
        curl -fL --retry 3 https://www.nasm.us/pub/nasm/releasebuilds/3.01/nasm-3.01.tar.xz -o /tmp/nasm.tar.xz
        tar -xf /tmp/nasm.tar.xz -C /tmp
        pushd /tmp/nasm-3.01
        ./configure --prefix=/opt/yoodesk-tools --disable-doc
        make -j3
        make install
        popd
    fi
    python3.13 scripts/prepare_sources.py
    python3.13 scripts/prepare_linux_native.py
    export VCPKG_ROOT=/workspace/artifacts/vcpkg
    export VCPKG_INSTALLED_ROOT="$VCPKG_ROOT/installed"
    export VCPKG_DEFAULT_TRIPLET="$triplet" VCPKG_DEFAULT_HOST_TRIPLET="$triplet"
    export VCPKG_BINARY_SOURCES="clear;files,/workspace/.ci-cache/vcpkg,readwrite"
    if [[ ! -d "$VCPKG_ROOT/.git" ]]; then
        git init "$VCPKG_ROOT"
        git -C "$VCPKG_ROOT" remote add origin https://github.com/microsoft/vcpkg.git
        git -C "$VCPKG_ROOT" fetch origin 9e593bb18ea69cc5095e012465dcd675a822ed0d
        git -C "$VCPKG_ROOT" checkout --detach FETCH_HEAD
    fi
    # Versioned manifest ports refer to historical git trees, beyond the pinned HEAD tree.
    if [[ "$(git -C "$VCPKG_ROOT" rev-parse --is-shallow-repository)" == true ]]; then
        git -C "$VCPKG_ROOT" fetch --unshallow origin 9e593bb18ea69cc5095e012465dcd675a822ed0d
    fi
    # Build the C++17 tool on both CPUs: a musl executable cannot load glibc libcurl.
    # Tools and target dependencies use the Ubuntu 18.04 glibc compiler/sysroot.
    python3.13 scripts/prepare_vcpkg_tool.py
    if [[ -x /opt/yoodesk-tools/bin/yoodesk-vcpkg ]]; then
        cp /opt/yoodesk-tools/bin/yoodesk-vcpkg "$VCPKG_ROOT/vcpkg"
    else
        "$VCPKG_ROOT/bootstrap-vcpkg.sh" -musl -disableMetrics
        cp "$VCPKG_ROOT/vcpkg" /opt/yoodesk-tools/bin/yoodesk-vcpkg
    fi
    "$VCPKG_ROOT/vcpkg" version --disable-metrics
    printf "%s\n" "cmake-4.4.0 rust-1.99.0 vcpkg-9e593 libdl" > /opt/yoodesk-tools/.ready
    # LLVM 10 includes the ARM64 NEON store intrinsics absent from Ubuntu's GCC 8.
    # Keep the Ubuntu 18 system headers, libstdc++ and glibc baseline.
    if [[ "$platform" == linux-arm64 ]]; then
        export CC=clang-10 CXX=clang++-10
        export CFLAGS=-flax-vector-conversions CXXFLAGS=-flax-vector-conversions
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang-10
    fi
    pushd artifacts/rustdesk-engine-src
    if ! "$VCPKG_ROOT/vcpkg" install --triplet "$triplet" --host-triplet "$triplet" --overlay-triplets=/workspace/ci/triplets --x-install-root="$VCPKG_INSTALLED_ROOT"; then
        python3.13 /workspace/ci/native_failure.py "$VCPKG_ROOT/buildtrees"
        exit 1
    fi
    popd
    export PKG_CONFIG_PATH="$VCPKG_INSTALLED_ROOT/$triplet/lib/pkgconfig:$VCPKG_INSTALLED_ROOT/$triplet/share/pkgconfig"
    export SODIUM_LIB_DIR="$VCPKG_INSTALLED_ROOT/$triplet/lib"
    unset SODIUM_STATIC SODIUM_SHARED
    export YOODESK_STATIC_CODEC_DIR="$VCPKG_INSTALLED_ROOT/$triplet/lib"
    export YOODESK_STATIC_FFMPEG_DIR="$VCPKG_INSTALLED_ROOT/$triplet/lib"
    # libyuv's vcpkg port does not ship a pkg-config descriptor.
    mkdir -p "$VCPKG_INSTALLED_ROOT/$triplet/lib/pkgconfig"
    cat > "$VCPKG_INSTALLED_ROOT/$triplet/lib/pkgconfig/libyuv.pc" <<EOF
prefix=$VCPKG_INSTALLED_ROOT/$triplet
libdir=\${prefix}/lib
includedir=\${prefix}/include
Name: libyuv
Description: libyuv from the pinned RustDesk vcpkg manifest
Version: 1.0
Libs: -L\${libdir} -lyuv
Cflags: -I\${includedir}
EOF
    python3.13 scripts/build_engine.py
    export YOODESK_DESK_ENGINE_LIB_DIR="/workspace/desk-engine/target/$target/release"
fi
if [[ "$kind" == webservice ]]; then
    cargo build --locked --release --features desk-engine --bin web-service
    cargo test --locked --lib
    # The independently released VNC DLL is loaded by the real host in the regression test.
    python3.13 ci/unpack_test_plugin.py "$platform"
    export YOODESK_TEST_VNC_LIBRARY="/workspace/artifacts/test-vnc/libyoodesk_vnc.so"
    cargo test --locked --test dynamic_plugins
else
    cargo build --locked --release --lib
    cargo test --locked --lib
fi
python3.13 ci/package.py "$kind" --platform "$platform"
