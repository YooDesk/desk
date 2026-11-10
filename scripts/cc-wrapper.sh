#!/bin/bash
# Cross-compiler wrapper used by cargo's cc-rs and meson.
#
#   cc-wrapper.sh <compiler> <sysroot> <multiarch> [args...]
#
# It passes the target sysroot, the multiarch include and library
# directories (Debian keeps both under usr/{include,lib}/<multiarch>, which
# the driver does not search by default) and the loader search path. The
# -m64 flag that cc-rs derives from the build host is dropped, because
# neither the LoongArch nor the MIPS cross gcc accepts it.
#
# Arguments are kept in an array so values containing quotes, such as
# -DOPENSSL_PIC="1", reach the compiler unchanged.
compiler=$1
sysroot=$2
multiarch=$3
shift 3

arguments=()
for argument in "$@"; do
    if [ "$argument" != "-m64" ]; then
        arguments+=("$argument")
    fi
done

extra_search=()
if [[ "$multiarch" == mips64el-linux-gnuabi64 ]]; then
    # Buster's libc linker scripts and runtime archives live under this
    # cross-toolchain layout, alongside the Loongnix media libraries.
    extra_search=(-L"$sysroot/usr/$multiarch/lib" -Wl,-rpath-link,"$sysroot/usr/$multiarch/lib")
fi

exec "$compiler" \
    --sysroot="$sysroot" \
    -isystem "$sysroot/usr/include/$multiarch" \
    -L"$sysroot/usr/lib/$multiarch" \
    -Wl,-rpath-link,"$sysroot/usr/lib/$multiarch" \
    "${extra_search[@]}" \
    "${arguments[@]}"
