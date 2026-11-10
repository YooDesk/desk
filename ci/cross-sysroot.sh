#!/usr/bin/env bash
# Install the pinned foreign-architecture sysroot used to cross-compile the
# desk engine (RustDesk tree) for ports without a native toolchain.
#
# Usage: ci/cross-sysroot.sh <loongarch64|mips64el> <install-dir>
#
# loongarch64: Debian sid loong64 packages from snapshot.debian.org at a
#   pinned timestamp (reproducible, immutable). The full dependency closure
#   is resolved from the same snapshot's Packages index, so the sysroot is
#   internally consistent. Provides ffmpeg 9, libyuv, x264/x265, libvpx,
#   opus, sodium, gtk3, gstreamer, pulse, alsa, va, vdpau, gbm, drm,
#   dbus, x11 stack — everything the engine's linux-pkg-config path probes.
#   glibc baseline: 2.43 (any shipped LoongArch system is >= 2.36).
#
# mips64el: Loongson 2K1000 targets run Loongnix "DaoXiangHu" (Debian 10
#   fork, glibc 2.28). The sysroot debs come from the Loongnix archive's
#   frozen pool (DaoXiangHu-stable has empty apt indexes, so packages are
#   fetched by direct URL with recorded checksums). libyuv is absent from
#   that archive and is built from source with the buster cross GCC.
set -euo pipefail
kind="$1"
prefix="$2"
mkdir -p "$prefix"

fetch() { # fetch <url> <sha256> <output>
    local url="$1" want="$2" out="$3"
    if [[ -s "$out" && "$(sha256sum "$out" | cut -d' ' -f1)" == "$want" ]]; then
        return 0
    fi
    if ! curl -fL --retry 3 --connect-timeout 30 "$url" -o "$out"; then
        rm -f "$out"
        return 1
    fi
    local got
    got="$(sha256sum "$out" | cut -d' ' -f1)"
    [[ "$got" == "$want" ]] || { echo "Checksum mismatch for $url: $got" >&2; exit 1; }
}

case "$kind" in
loongarch64)
    root="$prefix/sysroot-loong64"
    if [[ -f "$root/.done" ]]; then exit 0; fi
    mkdir -p "$root"
    snapshot='https://snapshot.debian.org/archive/debian/20261108T000000Z'
    # Resolve the dev-package closure from the same immutable index.
    index="$prefix/loong64-Packages"
    curl -fsL --retry 3 "$snapshot/dists/sid/main/binary-loong64/Packages.gz" | gunzip > "$index"
    python3 - "$(dirname "$0")/cross-sysroot-roots.py" "$index" loong64 > "$prefix/loong64-files"
    while read -r file; do
        mkdir -p "$prefix/debs/$(dirname "$file")"
        # snapshot serves /archive/<ts>/<path>; fall back to the live archive
        # if a file is missing there.
        if [[ ! -s "$prefix/debs/$file" ]]; then
            if ! curl -fsL --retry 3 "$snapshot/$file" -o "$prefix/debs/$file"; then
                curl -fsL --retry 3 "https://deb.debian.org/debian/$file" -o "$prefix/debs/$file"
            fi
        fi
        dpkg-deb -x "$prefix/debs/$file" "$root"
    done < "$prefix/loong64-files"
    rm -rf "$prefix/debs" "$index" "$prefix/loong64-files"
    touch "$root/.done"
    ;;
mips64el)
    root="$prefix/sysroot-mips64el"
    if [[ -f "$root/.done" ]]; then exit 0; fi
    mkdir -p "$root"
    # ftp.loongnix.cn is unreachable from GitHub Actions, so the pinned
    # packages are mirrored on the desk repository's sysroot release. The
    # original archive stays as a fallback (it serves the same bytes).
    base='https://github.com/YooDesk/desk/releases/download/sysroot-mips64el-loongnix-d10'
    fallback='https://ftp.loongnix.cn/os/loongnix/20/mips64el/pool/main'
    # Checksummed direct downloads from the frozen Loongnix pool (see
    # ci/cross-toolchains.md for how the list was produced and validated).
    # Manifest lines are "<pool-path> <sha256>"; comments are skipped.
    mkdir -p "$prefix/mips-debs"
    grep -vE '^\s*(#|$)' "$(dirname "$0")/cross-sysroot-mips64el.debs" > "$prefix/mips-files"
    while read -r name sha; do
        file="$prefix/mips-debs/$(basename "$name")"
        if ! fetch "$base/$(basename "$name")" "$sha" "$file"; then
            # GitHub normalises '~' in asset names to '_'; the upstream
            # archive uses the original name.
            original="$(echo "$name" | sed 's/_deb10u1/~deb10u1/')"
            fetch "$fallback/$original" "$sha" "$file"
        fi
        dpkg-deb -x "$file" "$root"
    done < "$prefix/mips-files"
    rm -rf "$prefix/mips-debs" "$prefix/mips-files"
    # libyuv is absent from the archive; build it with the buster cross GCC
    # into the same sysroot.
    bash "$(dirname "$0")/cross-sysroot-build-mips64el.sh" "$root"
    # Unify the QEMU runtime prefix: the absolute loader path is
    # /lib64/ld.so.1 and glibc itself comes from the buster cross sysroot
    # (same 2.28 the Loongnix libraries were built against).
    # The Loongnix sysroot has the runtime libraries, but the final link also
    # needs the C runtime startup files (crt1.o, libc_nonshared.a, libc.so
    # linker script) from the buster cross sysroot.
    buster_lib="$prefix/mips-root/usr/mips64el-linux-gnuabi64/lib"
    for name in crt1.o crti.o crtn.o crtbegin.o crtend.o libc_nonshared.a libc.so libm.so libpthread.so libdl.so librt.so libutil.so; do
        if [[ -e "$buster_lib/$name" && ! -e "$root/usr/mips64el-linux-gnuabi64/lib/$name" ]]; then
            mkdir -p "$root/usr/mips64el-linux-gnuabi64/lib"
            cp -L "$buster_lib/$name" "$root/usr/mips64el-linux-gnuabi64/lib/" 2>/dev/null || true
        fi
    done
    # libc.so references /lib64/ld.so.1; make sure the loader is present.
    mkdir -p "$root/lib64"
    cp -L "$prefix/mips-root/usr/mips64el-linux-gnuabi64/lib64/ld.so.1" "$root/lib64/ld.so.1"

    touch "$root/.done"
    ;;
*)
    echo "Unknown sysroot kind: $kind" >&2
    exit 1
    ;;
esac
