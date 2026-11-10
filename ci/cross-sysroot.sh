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


restore_so_links() { # restore_so_links <lib-dir>
    local directory="$1"
    [[ -d "$directory" ]] || return 0
    local versioned
    for versioned in "$directory"/*.so.*; do
        [[ -f "$versioned" ]] || continue
        local base
        base="$(basename "$versioned")"
        # libfoo.so.1.2.3 -> libfoo.so ; skip libfoo.so.6 (already a soname link)
        local stem="${base%%.so.*}"
        local link="$directory/$stem.so"
        if [[ ! -e "$link" ]]; then
            ln -s "$base" "$link"
        fi
    done
}

prepare_sysroot() {
    local root="$1" multiarch="$2"
    # Run this for cached and freshly unpacked roots alike: an archive's
    # .done marker does not supply the development metadata or linker inputs.
    if [[ "$multiarch" == mips64el-linux-gnuabi64 ]]; then
        mkdir -p "$root/usr/$multiarch" "$root/usr/include"
        cp -a "$prefix/mips-root/usr/$multiarch/." "$root/usr/$multiarch/"
        cp -a "$prefix/mips-root/usr/$multiarch/include/." "$root/usr/include/"
        mkdir -p "$root/lib64"
        cp -L "$prefix/mips-root/usr/$multiarch/lib64/ld.so.1" "$root/lib64/ld.so.1"
        # The prebuilt Loongnix archive contains the distro packages only;
        # it does not include libyuv, so the fast/cache paths need this too.
        bash "$(dirname "$0")/cross-sysroot-build-mips64el.sh" "$root"
    fi
    local libdir="$root/usr/lib/$multiarch"
    restore_so_links "$libdir"
    restore_so_links "$root/usr/lib64"
    # Debian libyuv-dev and the upstream CMake install omit a .pc file,
    # but scrap's linux-pkg-config feature requires it.
    if [[ ! -f "$libdir/pkgconfig/libyuv.pc" ]]; then
        test -f "$root/usr/include/libyuv.h"
        test -e "$libdir/libyuv.so" || test -f "$libdir/libyuv.a"
        mkdir -p "$libdir/pkgconfig"
        local version
        version=$(awk '/^#define LIBYUV_VERSION / {print $3}' "$root/usr/include/libyuv/version.h")
        cat > "$libdir/pkgconfig/libyuv.pc" <<EOF
prefix=/usr
libdir=\${prefix}/lib/$multiarch
includedir=\${prefix}/include

Name: libyuv
Description: YUV conversion and scaling library
Version: ${version:-0}
Libs: -L\${libdir} -lyuv
Cflags: -I\${includedir}
EOF
    fi
}

case "$kind" in
loongarch64)
    root="$prefix/sysroot-loong64"
    # The marker alone is not enough: a cached tree can exist without the
    # pkg-config files the engine needs, and the marker then pins the
    # broken state for every later run.
    if [[ -f "$root/.done" && -d "$root/usr/lib/loongarch64-linux-gnu/pkgconfig" ]]; then
        prepare_sysroot "$root" loongarch64-linux-gnu
        exit 0
    fi
    rm -f "$root/.done"
    mkdir -p "$root"
    # Fast path: the pre-built sysroot is one tarball. It was assembled from
    # the same pinned snapshot packages (see the comment below) and verified
    # to contain the full pkg-config set; fetching it avoids 400+ archive
    # requests that snapshot.debian.org rate-limits.
    sysroot_url='https://github.com/YooDesk/desk/releases/download/sysroot-mips64el-loongnix-d10/sysroot-loong64-sid.tar.xz'
    sysroot_sha='33a1b719f918395c6b954f1297bf9d61a69b292470882d7fd648207457b2c771'
    if curl -fL --retry 3 --connect-timeout 30 "$sysroot_url" -o /tmp/loong64-sysroot.tar.xz; then
        if echo "$sysroot_sha  /tmp/loong64-sysroot.tar.xz" | sha256sum -c -; then
            tar -xJf /tmp/loong64-sysroot.tar.xz -C "$root"
            rm -f /tmp/loong64-sysroot.tar.xz
            if [[ -d "$root/usr/lib/loongarch64-linux-gnu/pkgconfig" ]]; then
                # Debian keeps the arch-specific headers under
                # usr/include/<multiarch>; gcc's own include search expects
                # usr/include/bits. Link them so <bits/*.h> resolves.
                if [[ -d "$root/usr/include/loongarch64-linux-gnu" && ! -e "$root/usr/include/bits" ]]; then
                    cp -a "$root/usr/include/loongarch64-linux-gnu/." "$root/usr/include/"
                fi
                restore_so_links "$root/usr/lib/loongarch64-linux-gnu"
                restore_so_links "$root/usr/lib64"
                prepare_sysroot "$root" loongarch64-linux-gnu
                touch "$root/.done"
                exit 0
            fi
            echo "prebuilt loong64 sysroot lacks pkg-config; falling back to per-package fetch" >&2
        fi
        rm -f /tmp/loong64-sysroot.tar.xz
    fi
    snapshot='https://snapshot.debian.org/archive/debian/20261108T000000Z'
    # Resolve the dev-package closure from the same immutable index.
    index="$prefix/loong64-Packages"
    curl -fsL --retry 3 "$snapshot/dists/sid/main/binary-loong64/Packages.gz" | gunzip > "$index"
    # Strip carriage returns: a Windows checkout can leave CRLF in helper
    # output, and a trailing 
 breaks every download path.
    python3 - "$(dirname "$0")/cross-sysroot-roots.py" "$index" loong64 | tr -d '
' > "$prefix/loong64-files"
    echo "loong64 sysroot: $(wc -l < "$prefix/loong64-files") packages resolved"
    count=0
    while read -r file; do
        mkdir -p "$prefix/debs/$(dirname "$file")"
        # snapshot serves /archive/<ts>/<path>; fall back to the live archive
        # if a file is missing there.
        if [[ ! -s "$prefix/debs/$file" ]]; then
            # snapshot.debian.org rate-limits bursts, so retry with backoff and
            # fall back to the live archive before giving up on a package.
            fetched=0
            for attempt in 1 2 3 4; do
                if curl -fsL --retry 2 --connect-timeout 30 "$snapshot/$file" -o "$prefix/debs/$file"; then
                    fetched=1; break
                fi
                if curl -fsL --retry 2 --connect-timeout 30 "https://deb.debian.org/debian/$file" -o "$prefix/debs/$file"; then
                    fetched=1; break
                fi
                sleep $((attempt * 5))
            done
            [[ "$fetched" == 1 ]] || { echo "cannot fetch $file" >&2; exit 1; }
        fi
        dpkg-deb -x "$prefix/debs/$file" "$root"
        count=$((count + 1))
        if (( count % 50 == 0 )); then echo "loong64 sysroot: $count packages unpacked"; fi
    done < "$prefix/loong64-files"
    rm -rf "$prefix/debs" "$index" "$prefix/loong64-files"
    # Refuse to mark an incomplete tree as done.
    if [[ ! -d "$root/usr/lib/loongarch64-linux-gnu/pkgconfig" ]]; then
        echo "loong64 sysroot incomplete: no pkgconfig directory after extraction" >&2
        ls -l "$root/usr/lib" 2>&1 | head -5 >&2
        exit 1
    fi
    prepare_sysroot "$root" loongarch64-linux-gnu
    touch "$root/.done"
    ;;
mips64el)
    root="$prefix/sysroot-mips64el"
    if [[ -f "$root/.done" && -d "$root/usr/lib/mips64el-linux-gnuabi64/pkgconfig" ]]; then
        prepare_sysroot "$root" mips64el-linux-gnuabi64
        exit 0
    fi
    rm -f "$root/.done"
    mkdir -p "$root"
    # Fast path: pre-built from the same pinned Loongnix packages.
    mips_url='https://github.com/YooDesk/desk/releases/download/sysroot-mips64el-loongnix-d10/sysroot-mips64el-loongnix.tar.xz'
    mips_sha='7ddc16199f5a7bc9d9f44409e93accb425b59736626c7a6a6637aae3066f7b77'
    if curl -fL --retry 3 --connect-timeout 30 "$mips_url" -o /tmp/mips-sysroot.tar.xz; then
        if echo "$mips_sha  /tmp/mips-sysroot.tar.xz" | sha256sum -c -; then
            tar -xJf /tmp/mips-sysroot.tar.xz -C "$root"
            rm -f /tmp/mips-sysroot.tar.xz
            if [[ -d "$root/usr/lib/mips64el-linux-gnuabi64/pkgconfig" ]]; then
                mkdir -p "$root/lib64"
                cp -L "$prefix/mips-root/usr/mips64el-linux-gnuabi64/lib64/ld.so.1" "$root/lib64/ld.so.1" 2>/dev/null || true
                restore_so_links "$root/usr/lib/mips64el-linux-gnuabi64"
                prepare_sysroot "$root" mips64el-linux-gnuabi64
                touch "$root/.done"
                exit 0
            fi
            echo "prebuilt mips64el sysroot lacks pkg-config; falling back" >&2
        fi
        rm -f /tmp/mips-sysroot.tar.xz
    fi
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
        # The manifest records the mirror's asset names: GitHub rewrites '+'
        # and '~' in uploaded asset names, so these are not the upstream
        # file names. Fetch the mirror first, the upstream archive second.
        if ! fetch "$base/$(basename "$name")" "$sha" "$file"; then
            fetch "$fallback/$name" "$sha" "$file"
        fi
        dpkg-deb -x "$file" "$root"
    done < "$prefix/mips-files"
    rm -rf "$prefix/mips-debs" "$prefix/mips-files"
    # libyuv is absent from the archive; build it with the buster cross GCC
    # into the same sysroot.
    mkdir -p "$root/usr/mips64el-linux-gnuabi64" "$root/usr/include"
    cp -a "$prefix/mips-root/usr/mips64el-linux-gnuabi64/." "$root/usr/mips64el-linux-gnuabi64/"
    cp -a "$prefix/mips-root/usr/mips64el-linux-gnuabi64/include/." "$root/usr/include/"
    bash "$(dirname "$0")/cross-sysroot-build-mips64el.sh" "$root"
    test -d "$root/usr/lib/mips64el-linux-gnuabi64/pkgconfig"
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

    prepare_sysroot "$root" mips64el-linux-gnuabi64
    touch "$root/.done"
    ;;
*)
    echo "Unknown sysroot kind: $kind" >&2
    exit 1
    ;;
esac
