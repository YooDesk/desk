"""Adapt Loongnix glibc's profiling clock in the CI sysroot for QEMU."""
from pathlib import Path
import struct
import sys


PATCH_COUNTS = {
    "ld-2.28.so": 18,
    "libc-2.28.so": 2,
    "libpthread-2.28.so": 2,
    "libresolv-2.28.so": 2,
}


def prepare(path, expected):
    data = bytearray(path.read_bytes())
    if data[:6] != b"\x7fELF\x02\x01":
        raise ValueError(f"{path}: expected a 64-bit little-endian ELF")
    _, machine, _, _, _, section_table, _, _, _, _, section_size, section_count, _ = (
        struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
    )
    if machine != 8:
        raise ValueError(f"{path}: expected MIPS ELF")
    changed = 0
    for index in range(section_count):
        _, kind, flags, _, offset, size, *_ = struct.unpack_from(
            "<IIQQQQIIQQ", data, section_table + index * section_size
        )
        if kind != 1 or not (flags & 4):  # SHT_PROGBITS and SHF_EXECINSTR
            continue
        for address in range(offset, offset + size - 3, 4):
            instruction, = struct.unpack_from("<I", data, address)
            if instruction & ~0x001F0000 == 0x7C00F03B:
                # Preserve the destination; read QEMU's HW register 2
                # (cycle count) instead of Loongnix's HW register 30.
                struct.pack_into("<I", data, address, (instruction & ~(31 << 11)) | (2 << 11))
                changed += 1
    if changed != expected:
        raise ValueError(f"{path}: expected {expected} profiling clock instructions, found {changed}")
    path.write_bytes(data)


if __name__ == "__main__":
    libdir = Path(sys.argv[1]) / "lib/mips64el-linux-gnuabi64"
    for filename, count in PATCH_COUNTS.items():
        prepare(libdir / filename, count)
    print("Prepared the Loongnix glibc profiling clock for QEMU")
