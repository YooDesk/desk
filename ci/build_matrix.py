"""Select native targets; only the complete matrix is eligible for a release."""
import json
import os

targets = [
    dict(platform='windows-x64', os='windows-2022', target='x86_64-pc-windows-msvc', triplet='x64-windows-static', msvc='x64'),
    dict(platform='windows-arm64', os='windows-11-arm', target='aarch64-pc-windows-msvc', triplet='arm64-windows-static', msvc='x64_arm64'),
    dict(platform='linux-x64', os='ubuntu-22.04', target='x86_64-unknown-linux-gnu', triplet='x64-linux'),
    dict(platform='linux-arm64', os='ubuntu-22.04-arm', target='aarch64-unknown-linux-gnu', triplet='arm64-linux'),
    dict(platform='linux-loong64', os='ubuntu-22.04', target='loongarch64-unknown-linux-gnu', triplet='loongarch64-linux', cross=True),
    dict(platform='linux-mips64el', os='ubuntu-22.04', target='mips64el-unknown-linux-gnuabi64', triplet='mips64el-linux', cross=True),
    # macOS 使用系统 vcpkg 安装的静态库；x64 在 arm64 runner 上交叉编译。
    dict(platform='macos-arm64', os='macos-14', target='aarch64-apple-darwin', triplet='arm64-osx', macos=True),
    dict(platform='macos-x64', os='macos-14', target='x86_64-apple-darwin', triplet='x64-osx', macos=True),
]
platform = os.environ.get('PLATFORM', 'all')
selected = [target for target in targets if platform in ['all', target['platform']]]
if not selected:
    raise ValueError(f'Unknown platform: {platform}')
print('matrix=' + json.dumps({'include': selected}, separators=(',', ':')))
