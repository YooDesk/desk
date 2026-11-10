"""Resolve the sysroot package closure from a Debian Packages index.

Usage: cross-sysroot-roots.py <index-file> <flavor>

Prints pool-relative file names, one per line. The dependency walk mirrors
apt's first-alternative choice and skips packages the index does not carry
(no error: the closure only needs libraries that exist for the flavor).
"""
import re
import sys


def load(index_path):
    packages = {}
    current = {}
    with open(index_path, encoding='utf-8', errors='replace') as stream:
        for line in stream:
            line = line.rstrip('\n')
            if not line:
                if 'Package' in current and 'Filename' in current:
                    packages[current['Package']] = current
                current = {}
                continue
            if ': ' in line:
                key, value = line.split(': ', 1)
                current[key] = value
    if 'Package' in current and 'Filename' in current:
        packages[current['Package']] = current
    return packages


def depends(names):
    result = []
    for clause in names.split(','):
        alternative = clause.split('|')[0].strip()
        match = re.match(r'^([a-z0-9][a-z0-9.+-]*)(?::[a-z0-9-]+)?', alternative)
        if match:
            result.append(match.group(1))
    return result


# Native libraries the engine's linux-pkg-config and build scripts probe,
# plus their development headers. Keep in sync with scripts/build_engine.py.
LOONG64_ROOTS = [
    'libc6-dev', 'linux-libc-dev', 'libstdc++-14-dev', 'libstdc++6',
    'libavcodec-dev', 'libavformat-dev', 'libavutil-dev', 'libswresample-dev',
    'libyuv-dev', 'libaom-dev', 'libopus-dev', 'libsodium-dev', 'libx264-dev', 'libx265-dev',
    'libvpx-dev', 'libgtk-3-dev', 'libasound2-dev', 'libpulse-dev',
    'libgstreamer1.0-dev', 'libgstreamer-plugins-base1.0-dev',
    'libgbm-dev', 'libdrm-dev', 'libxkbcommon-dev', 'libxdo-dev', 'libudev-dev',
    'libdbus-1-dev', 'libva-dev', 'libvdpau-dev', 'libegl1-mesa-dev',
    'libgles2-mesa-dev', 'libxcb1-dev', 'libxcb-randr0-dev', 'libxcb-shm0-dev',
    'libxcb-xfixes0-dev', 'libxcb-shape0-dev', 'libxfixes-dev', 'libxrandr-dev',
    'libxtst-dev', 'libwayland-dev', 'libx11-dev', 'libxext-dev', 'libxi-dev',
    'libxinerama-dev', 'libxcursor-dev', 'libxrender-dev', 'libxss-dev',
]

# Complete the development packages absent from the prebuilt Loongnix root.
# Existing Loongnix files are preserved when this Debian buster closure is merged.
MIPS64EL_SUPPLEMENT_ROOTS = [
    package for package in LOONG64_ROOTS
    if package not in ('libstdc++-14-dev', 'libyuv-dev')
] + [
    'libstdc++-8-dev', 'libharfbuzz-dev', 'libgraphite2-dev',
    'libxcb-shape0-dev', 'libatk-bridge2.0-dev', 'libxcomposite-dev', 'liblz4-dev',
]


def main():
    index_path, flavor = sys.argv[1], sys.argv[2]
    packages = load(index_path)
    if flavor == 'loong64':
        roots = LOONG64_ROOTS
    elif flavor == 'mips64el-supplement':
        roots = MIPS64EL_SUPPLEMENT_ROOTS
    else:
        raise SystemExit('unknown flavor: ' + flavor)
    seen = set()
    queue = list(roots)
    while queue:
        name = queue.pop()
        if name in seen or name not in packages:
            continue
        seen.add(name)
        queue.extend(depends(packages[name].get('Depends', '')))
    for name in sorted(seen):
        fields = packages[name]
        if flavor == 'mips64el-supplement':
            print(fields['Filename'], fields['SHA256'])
        else:
            print(fields['Filename'])


if __name__ == '__main__':
    main()
