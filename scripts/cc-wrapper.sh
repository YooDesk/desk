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
        # The MIPS sysroot can expose libdbus-1.a in the dev directory
        # while its shared library lives in the runtime directory.
        if [[ "$multiarch" == mips64el-linux-gnuabi64 && "$argument" == -ldbus-1 ]]; then
            argument=-l:libdbus-1.so.3
        fi
        arguments+=("$argument")
    fi
done

extra_search=()
linker_options=()
if [[ "$multiarch" == mips64el-linux-gnuabi64 ]]; then
    # Prefer Buster's complete dynamic libc/pthread inputs. The distro
    # development directory can contain a static archive before its dynamic
    # link is available; searching it first silently links non-PIC pthread
    # objects into Rust cdylibs.
    extra_search=(-L"$sysroot/usr/$multiarch/lib" -Wl,-rpath-link,"$sysroot/usr/$multiarch/lib"
                  -L"$sysroot/lib/$multiarch" -Wl,-rpath-link,"$sysroot/lib/$multiarch")
    # GCC 8 emits MIPS64 exception-frame relocations that Rust's default
    # LLVM linker rejects even for -fPIC C++ objects. Use the matching GNU
    # linker for the complete target, retaining C++ exception support.
    linker_options=(-fuse-ld=bfd)
fi

exec "$compiler" \
    --sysroot="$sysroot" \
    -isystem "$sysroot/usr/include/$multiarch" \
    "${extra_search[@]}" \
    -L"$sysroot/usr/lib/$multiarch" \
    -Wl,-rpath-link,"$sysroot/usr/lib/$multiarch" \
    "${arguments[@]}" \
    "${linker_options[@]}"
