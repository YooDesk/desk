#!/usr/bin/env bash
# Source on an Ubuntu x64 runner. Keep ABI 1.0 tools and caches isolated.
set -euo pipefail
oldworld_tools="${YOODESK_OLDWORLD_TOOLS:-$PWD/.ci-cache/oldworld}"
mkdir -p "$oldworld_tools"
oldworld_tools="$(cd "$oldworld_tools" && pwd)"
oldworld_fetch() {
    local url="$1" sha="$2" destination="$3"
    if [[ ! -s "$destination" ]] || ! echo "$sha  $destination" | sha256sum -c - >/dev/null 2>&1; then
        curl -fL --retry 3 --connect-timeout 30 --max-time 1800 "$url" -o "$destination.partial"
        echo "$sha  $destination.partial" | sha256sum -c -
        mv "$destination.partial" "$destination"
    fi
}
oldworld_fetch \
    'https://github.com/cxxzhang/qt5.15.17/releases/download/linux-toolchains-v1/toolchain-linux-loong64-oldworld-glibc228-61698744d55f.tar.xz' \
    '61698744d55f79f771e357cacf65ffed067d59ea86de5b609c11d8f4243ab90a' \
    "$oldworld_tools/toolchain.tar.xz"
if [[ ! -f "$oldworld_tools/toolchain/toolchain.json" ]]; then
    mkdir -p "$oldworld_tools/toolchain"
    tar -xJf "$oldworld_tools/toolchain.tar.xz" -C "$oldworld_tools/toolchain"
fi
oldworld_fetch \
    'https://ftp.loongnix.cn/toolchain/rust/rust-1.99/2026-10-09/abi1.0/rust-1.99.0-loongarch64-unknown-linux-gnu.tar.xz' \
    '127a4130c0462e524659590b32ba79e705133b23c9dc978ae91276f653eb9dcc' \
    "$oldworld_tools/rust.tar.xz"
if [[ ! -x "$oldworld_tools/rust/bin/rustc" ]]; then
    tar -xJf "$oldworld_tools/rust.tar.xz" -C "$oldworld_tools"
    bash "$oldworld_tools/rust-1.99.0-loongarch64-unknown-linux-gnu/install.sh" \
        --prefix="$oldworld_tools/rust" --components=rustc,rust-std-loongarch64-unknown-linux-gnu --disable-ldconfig
fi
export QEMU_LD_PREFIX="$oldworld_tools/toolchain/sysroot"
export YOODESK_OLDWORLD_ROOT="$QEMU_LD_PREFIX"
export YOODESK_OLDWORLD_COMPILER="$oldworld_tools/toolchain/bin/clang"
export YOODESK_OLDWORLD_QEMU="$oldworld_tools/toolchain/bin/qemu"
export YOODESK_OLDWORLD_RUSTC="$oldworld_tools/rust/bin/rustc"
test -x "$YOODESK_OLDWORLD_COMPILER"
test -x "$YOODESK_OLDWORLD_QEMU"
test -f "$QEMU_LD_PREFIX/lib64/ld.so.1"
export PATH="$oldworld_tools/toolchain/bin:$PATH"
