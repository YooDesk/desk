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
# The vendor also publishes an x64 host compiler with the legacy LLVM backend.
# Its target standard library must come from the same vendor build.
while read -r url sha; do
    archive="${url##*/}"
    oldworld_fetch "$url" "$sha" "$oldworld_tools/$archive"
    if [[ ! -f "$oldworld_tools/.installed-$sha" ]]; then
        tar -xJf "$oldworld_tools/$archive" -C "$oldworld_tools"
        bash "$oldworld_tools/${archive%.tar.xz}/install.sh" \
            --prefix="$oldworld_tools/rust" --disable-ldconfig
        touch "$oldworld_tools/.installed-$sha"
    fi
done < <(python3 - "$(dirname "${BASH_SOURCE[0]}")/oldworld-rust.lock.json" <<'PY'
import json, sys
for entry in json.load(open(sys.argv[1]))['files']:
    print(entry['url'], entry['sha256'])
PY
)
export QEMU_LD_PREFIX="$oldworld_tools/toolchain/sysroot"
export YOODESK_OLDWORLD_ROOT="$QEMU_LD_PREFIX"
export YOODESK_OLDWORLD_COMPILER="$oldworld_tools/toolchain/bin/clang"
bash "$(dirname "${BASH_SOURCE[0]}")/oldworld-qemu.sh" "$oldworld_tools"
export YOODESK_OLDWORLD_QEMU="$oldworld_tools/qemu-oldworld/bin/qemu-loongarch64"
export YOODESK_OLDWORLD_RUSTC="$oldworld_tools/rust/bin/rustc"
test -x "$YOODESK_OLDWORLD_COMPILER"
test -x "$YOODESK_OLDWORLD_QEMU"
test -f "$QEMU_LD_PREFIX/lib64/ld.so.1"
export PATH="$oldworld_tools/toolchain/bin:$PATH"
bash "$(dirname "${BASH_SOURCE[0]}")/oldworld-cargo.sh" "$oldworld_tools"
export PATH="$oldworld_tools/upstream-cargo/bin:$PATH"
