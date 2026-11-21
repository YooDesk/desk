#!/usr/bin/env bash
# Loongson's old-world branch implements the legacy 128-signal kernel ABI.
# The generic build-tools emulator only implements the new-world signal ABI.
set -euo pipefail
prefix="${1:?tool cache required}"
revision=fe2ec9f4b69e6f6272ff72d68b07964bcd232ffd
if [[ -x "$prefix/qemu-oldworld/bin/qemu-loongarch64" && -f "$prefix/qemu-oldworld/$revision" ]]; then
    exit 0
fi
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    build-essential ninja-build pkg-config python3-venv libglib2.0-dev \
    libpcre2-dev zlib1g-dev git
mkdir -p "$prefix/qemu-source"
git -C "$prefix/qemu-source" init
git -C "$prefix/qemu-source" fetch --depth 1 https://github.com/loongson/qemu.git "$revision"
git -C "$prefix/qemu-source" checkout --detach FETCH_HEAD
test "$(git -C "$prefix/qemu-source" rev-parse HEAD)" = "$revision"
mkdir -p "$prefix/qemu-build"
cd "$prefix/qemu-build"
"$prefix/qemu-source/configure" --prefix="$prefix/qemu-oldworld" \
    --target-list=loongarch64-linux-user --static --disable-system \
    --disable-tools --disable-docs --disable-guest-agent --disable-plugins \
    --disable-capstone --disable-slirp --disable-vnc --disable-werror
ninja -j"$(nproc)" qemu-loongarch64
mkdir -p "$prefix/qemu-oldworld/bin" "$prefix/qemu-oldworld/share/licenses"
cp qemu-loongarch64 "$prefix/qemu-oldworld/bin/"
cp "$prefix/qemu-source/COPYING" "$prefix/qemu-source/COPYING.LIB" "$prefix/qemu-oldworld/share/licenses/"
printf '%s\n' "https://github.com/loongson/qemu/commit/$revision" > "$prefix/qemu-oldworld/SOURCE"
sha256sum "$prefix/qemu-oldworld/bin/qemu-loongarch64"
touch "$prefix/qemu-oldworld/$revision"
