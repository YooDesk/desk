"""Audit all desktop engine inputs and the ELF runtime closure before compiling Cargo."""
from __future__ import annotations

import argparse
from collections import deque
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import shlex
import struct
import subprocess
import tarfile

PACKAGES = (
    "libyuv vpx aom opus libsodium libavcodec libavutil libavformat "
    "gstreamer-app-1.0 gstreamer-video-1.0 gio-2.0 gtk+-3.0 libudev "
    "alsa libpulse libpulse-simple dbus-1 libdrm gbm x11 xext xfixes xrandr "
    "xtst xkbcommon xcb xcb-shm xcb-randr xcb-xfixes xcb-shape"
).split()
LIBRARIES = (
    "avformat avcodec swresample avutil x264 x265 yuv vpx aom opus sodium "
    "X11 Xext Xfixes Xrandr Xtst xkbcommon xcb xcb-shm xcb-randr xcb-xfixes "
    "drm gbm asound pulse pulse-simple dbus-1 udev stdc++ dl m pthread "
    "gstapp-1.0 gstvideo-1.0 gio-2.0 gtk-3 gdk-3 gobject-2.0 glib-2.0"
).split()
HEADERS = (
    "libavcodec/avcodec.h libavutil/frame.h libavformat/avformat.h "
    "libswresample/swresample.h vpx/vpx_encoder.h aom/aom_encoder.h libyuv.h "
    "opus/opus.h sodium.h gtk-3.0/gtk/gtk.h gstreamer-1.0/gst/gst.h "
    "libudev.h xf86drm.h gbm.h alsa/asoundlib.h pulse/pulseaudio.h "
    "dbus-1.0/dbus/dbus.h X11/Xlib.h X11/extensions/XTest.h "
    "X11/extensions/Xrandr.h xkbcommon/xkbcommon.h xcb/xcb.h xcb/shm.h "
    "xcb/randr.h xcb/xfixes.h xcb/shape.h wayland-client.h "
    "linux/videodev2.h linux/input.h linux/uinput.h sys/ioctl.h"
).split()


def elf_metadata(data):
    if len(data) < 52 or data[:4] != b"\x7fELF":
        return None
    machine = struct.unpack_from("<H" if data[5] == 1 else ">H", data, 18)[0]
    if data[4:6] != b"\x02\x01" or len(data) < 64:
        return {"machine": machine, "elf_class": data[4], "endian": data[5], "soname": None, "needed": []}
    phoff = struct.unpack_from("<Q", data, 32)[0]
    entry_size, count = struct.unpack_from("<HH", data, 54)
    segments = []
    for index in range(count):
        offset = phoff + index * entry_size
        if offset + 56 > len(data):
            return None
        segments.append(struct.unpack_from("<IIQQQQQQ", data, offset))
    tags = []
    for segment in segments:
        if segment[0] == 2:
            for offset in range(segment[2], segment[2] + segment[5], 16):
                if offset + 16 > len(data):
                    return None
                tag, value = struct.unpack_from("<qQ", data, offset)
                if tag == 0:
                    break
                tags.append((tag, value))
    strings = next((value for tag, value in tags if tag == 5), None)
    base = None
    if strings is not None:
        for segment in segments:
            if segment[0] == 1 and segment[3] <= strings < segment[3] + segment[5]:
                base = segment[2] + strings - segment[3]
                break

    def name(value):
        if base is None or base + value >= len(data):
            return ""
        start = base + value
        end = data.find(b"\0", start)
        return data[start:end if end >= 0 else len(data)].decode("utf-8", "replace")

    return {
        "machine": machine,
        "elf_class": 2,
        "endian": 1,
        "soname": next((name(value) for tag, value in tags if tag == 14), None),
        "needed": [name(value) for tag, value in tags if tag == 1],
    }


class Inventory:
    def __init__(self):
        self.files = set()
        self.links = {}
        self.packages = {}
        self.versions = {}
        self.libraries = {}

    def add(self, filename, link, reader):
        filename = posixpath.normpath("/" + filename.lstrip("/"))
        self.files.add(filename)
        if link is not None:
            self.links[filename] = posixpath.normpath(
                link if link.startswith("/") else posixpath.join(posixpath.dirname(filename), link)
            )
            if filename.endswith(".pc"):
                alias = PurePosixPath(filename).stem
                target = PurePosixPath(self.links[filename]).stem
                if alias != target:
                    self.packages[alias] = [target]
            return
        basename = PurePosixPath(filename).name
        if filename.endswith(".pc"):
            text = reader().decode("utf-8", "replace")
            requirements = []
            for line in text.splitlines():
                if line.startswith(("Requires:", "Requires.private:")):
                    requirements += re.findall(
                        r"(?:^|[,\s])([A-Za-z][\w+.-]*)(?:\s*(?:>=|<=|=|>|<)\s*[^\s,]+)?",
                        line.split(":", 1)[1],
                    )
            self.packages[basename[:-3]] = requirements
            version = re.search(r"(?m)^Version:\s*(.+)$", text)
            if version:
                self.versions[basename[:-3]] = version.group(1).strip()
        elif re.match(r".+\.so(?:\..+)?$", basename):
            metadata = elf_metadata(reader())
            if metadata:
                metadata["path"] = filename
                self.libraries[basename] = metadata
                if metadata["soname"]:
                    self.libraries[metadata["soname"]] = metadata

    def exists(self, path):
        seen = set()
        while path in self.links:
            if path in seen:
                return False
            seen.add(path)
            path = self.links[path]
        return path in self.files


def inventory_archive(path):
    result = Inventory()
    # Read the compressed archive once, without extracting target binaries.
    with tarfile.open(path, "r|*") as archive:
        for member in archive:
            if not (member.isfile() or member.issym() or member.islnk()):
                continue
            link = member.linkname if member.issym() else "/" + member.linkname if member.islnk() else None
            result.add(member.name, link, lambda: archive.extractfile(member).read())
    return result


def inventory_root(root):
    result = Inventory()
    for path in root.rglob("*"):
        if not (path.is_file() or path.is_symlink()):
            continue
        filename = path.relative_to(root).as_posix()
        link = path.readlink().as_posix() if path.is_symlink() else None
        result.add(filename, link, path.read_bytes)
    return result


def audit(inventory, platform):
    expected = {"linux-loong64": 258, "linux-mips64el": 8}[platform]
    missing_packages = set()
    visited_packages = set()
    queue = deque(PACKAGES)
    while queue:
        package = queue.popleft()
        if package in visited_packages:
            continue
        visited_packages.add(package)
        if package not in inventory.packages:
            missing_packages.add(package)
        else:
            queue.extend(inventory.packages[package])

    missing_headers = [header for header in HEADERS if not any(
        name.endswith("/" + header) and inventory.exists(name) for name in inventory.files
    )]
    missing_links = []
    queue = deque()
    for library in LIBRARIES:
        basename = f"lib{library}.so"
        links = [name for name in inventory.files if name.endswith("/" + basename)]
        if not any(inventory.exists(name) for name in links):
            if not any(name.endswith(f"/lib{library}.a") for name in inventory.files):
                missing_links.append(library)
        candidates = [value for key, value in inventory.libraries.items()
                      if key == basename or key.startswith(basename + ".")]
        if candidates:
            queue.append(candidates[0])
    missing_runtime = set()
    wrong_architecture = set()
    visited_runtime = set()
    while queue:
        library = queue.popleft()
        if library["path"] in visited_runtime:
            continue
        visited_runtime.add(library["path"])
        if library["machine"] != expected or library["elf_class"] != 2 or library["endian"] != 1:
            wrong_architecture.add(library["path"])
        for needed in library["needed"]:
            dependency = inventory.libraries.get(needed)
            if dependency is None:
                missing_runtime.add(needed)
            else:
                queue.append(dependency)
    return {
        "platform": platform,
        "files": len(inventory.files),
        "pkg_config_closure": len(visited_packages),
        "runtime_closure": len(visited_runtime),
        "missing_pkg_config": sorted(missing_packages),
        "missing_headers": sorted(missing_headers),
        "missing_link_libraries": sorted(missing_links),
        "missing_runtime_libraries": sorted(missing_runtime),
        "wrong_architecture": sorted(wrong_architecture),
        "incompatible_api_versions": ["AOM 3.14.1 is required by the pinned engine API"]
        if tuple(map(int, re.findall(r"\d+", inventory.versions.get("aom", "0"))[:3])) < (3, 14, 1) else [],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--archive", type=Path)
    source.add_argument("--root", type=Path)
    parser.add_argument("--platform", choices=("linux-loong64", "linux-mips64el"), required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--check-headers", action="store_true")
    args = parser.parse_args()
    inventory = inventory_archive(args.archive) if args.archive else inventory_root(args.root)
    report = audit(inventory, args.platform)
    report["header_compile_errors"] = []
    report["link_compile_errors"] = []
    if args.check_headers and not report["missing_pkg_config"]:
        flags = subprocess.check_output(["pkg-config", "--cflags", *PACKAGES], text=True)
        compiler = shlex.split(os.environ["CC"])
        for header in HEADERS:
            result = subprocess.run(
                [*compiler, *shlex.split(flags), "-fsyntax-only", "-x", "c", "-"],
                input=f"#include <{header}>\n", text=True, capture_output=True, timeout=60,
            )
            if result.returncode:
                report["header_compile_errors"].append({"header": header, "error": result.stderr})
        clang_args = shlex.split(os.environ.get("BINDGEN_EXTRA_CLANG_ARGS", ""))
        for header in ("linux/videodev2.h", "linux/input.h", "linux/uinput.h"):
            result = subprocess.run(
                ["clang-10", *clang_args, *shlex.split(flags), "-fsyntax-only", "-x", "c", "-"],
                input=f"#include <{header}>\n", text=True, capture_output=True, timeout=60,
            )
            if result.returncode:
                report["header_compile_errors"].append({"bindgen_header": header, "error": result.stderr})
        result = subprocess.run(
            [*compiler, "-Wl,--no-as-needed", "-x", "c", "-", "-o", "/tmp/yoodesk-engine-link-probe",
             *[f"-l{library}" for library in LIBRARIES]],
            input="int main(void) { return 0; }\n", text=True, capture_output=True, timeout=60,
        )
        if result.returncode:
            report["link_compile_errors"].append(result.stderr)
        # An executable can accept non-PIC libc archives. Exercise the
        # shared-library link used by the engine's cdylib dependencies too,
        # with a real pthread reference so an accidental static fallback
        # cannot remain hidden behind an unused -lpthread.
        result = subprocess.run(
            [*compiler, "-fPIC", "-shared", "-Wl,--no-as-needed", "-Wl,-z,defs",
             "-x", "c", "-", "-o", "/tmp/yoodesk-engine-shared-link-probe.so",
             *[f"-l{library}" for library in LIBRARIES]],
            input=("#include <pthread.h>\n"
                   "int yoodesk_probe(pthread_t *thread, void *(*start)(void *)) {\n"
                   "return pthread_create(thread, 0, start, 0); }\n"),
            text=True, capture_output=True, timeout=60,
        )
        if result.returncode:
            report["link_compile_errors"].append(result.stderr)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=True, indent=2))
    return int(any(report[key] for key in report if key.startswith("missing_") or key in ("wrong_architecture", "header_compile_errors", "link_compile_errors", "incompatible_api_versions")))


if __name__ == "__main__":
    raise SystemExit(main())
