"""Patch the pinned evdev fork to wait for a new virtual device's seat ACL."""
from pathlib import Path
import io
import os
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
REVISION = "cec616e37790293d2cd2aa54a96601ed6b1b35a9"
URL = "https://github.com/rustdesk-org/evdev"


def git_source():
    cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    for source in (cargo / "git/db").glob("evdev-*"):
        if subprocess.run(["git", "--git-dir", str(source), "cat-file", "-e", REVISION + "^{commit}"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            return source
    source = ROOT / ".ci-cache/evdev-source.git"
    if not source.exists():
        source.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "init", "--bare", str(source)], check=True, capture_output=True)
    subprocess.run(["git", "--git-dir", str(source), "fetch", "--depth=1", URL, REVISION], check=True)
    return source


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists() or path.read_bytes() != data:
        path.write_bytes(data)


def prepare():
    source = git_source()
    archive = subprocess.check_output(["git", "--git-dir", str(source), "archive", "--format=tar", REVISION])
    destination = ROOT / "artifacts/evdev-patched"
    with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
        for member in bundle.getmembers():
            if not member.isfile():
                continue
            assert not member.name.startswith("/") and ".." not in Path(member.name).parts
            data = bundle.extractfile(member).read()
            if member.name == "src/uinput.rs":
                text = data.decode().replace("\r\n", "\n")
                old = '''return OpenOptions::new()
                                            .read(true)
                                            // .write(true)
                                            .custom_flags(O_NONBLOCK)
                                            .open(event_file);'''
                if old not in text:
                    raise RuntimeError("pinned evdev event-open source changed")
                text = text.replace(old, 'return yoodesk_event_open::open_created_event(std::path::Path::new(&event_file), O_NONBLOCK);', 1)
                text += '\n#[path = "uinput_event_open.rs"]\nmod yoodesk_event_open;\n'
                data = text.encode()
            write(destination / member.name, data)
    write(destination / "src/uinput_event_open.rs", (ROOT / "engine-patches/uinput-event-open.rs").read_bytes())
    print("evdev", REVISION, "prepared at", destination)


if __name__ == "__main__":
    prepare()
