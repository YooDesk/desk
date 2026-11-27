"""Materialize the pinned arboard fork with opaque Windows bitmap reading."""
from pathlib import Path
import io
import os
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
REVISION = "c7d5781f563176df9efd8df6287e823fb1b9bed5"
URL = "https://github.com/rustdesk-org/arboard"


def prepare():
    cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    source = next((path for path in (cargo / "git/db").glob("arboard-*")
                   if subprocess.run(["git", "--git-dir", str(path), "cat-file", "-e", REVISION + "^{commit}"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0), None)
    if source is None:
        source = ROOT / ".ci-cache/arboard-source.git"
        if not source.exists():
            source.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["git", "init", "--bare", str(source)], check=True, capture_output=True)
        subprocess.run(["git", "--git-dir", str(source), "fetch", "--depth=1", URL, REVISION], check=True)
    archive = subprocess.check_output(["git", "--git-dir", str(source), "archive", "--format=tar", REVISION])
    destination = ROOT / "artifacts/arboard-patched"

    def write(path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists() or path.read_bytes() != data:
            path.write_bytes(data)

    with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
        for member in bundle.getmembers():
            if not member.isfile():
                continue
            assert not member.name.startswith("/") and ".." not in Path(member.name).parts
            data = bundle.extractfile(member).read()
            if member.name == "src/platform/windows.rs":
                text = data.decode().replace("\r\n", "\n")
                anchor = "\tpub(super) fn read_cf_dibv5("
                if text.count(anchor) != 1:
                    raise RuntimeError("Pinned arboard image module changed")
                bridge = '''
    mod yoodesk_native_bitmap { include!("windows_native_bitmap.rs"); }

    pub(super) fn read_clipboard_dibv5(dibv5: &[u8]) -> Result<ImageData<'static>, Error> {
        if dibv5.len() >= size_of::<BITMAPV5HEADER>() {
            let header = unsafe { std::ptr::read_unaligned(dibv5.as_ptr() as *const BITMAPV5HEADER) };
            // Windows can synthesize DIBV5 from CF_BITMAP with an extra RGB
            // mask table. Read opaque pixels through the OS bitmap instead
            // of guessing offsets. Explicit-alpha images keep the DIB path.
            if header.bV5AlphaMask == 0 && matches!(header.bV5Compression, BI_RGB | BI_BITFIELDS) {
                let bitmap = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardData(2) };
                if bitmap != 0 {
                    return yoodesk_native_bitmap::read(bitmap as _, header.bV5Width, header.bV5Height);
                }
            }
        }
        read_cf_dibv5(dibv5)
    }

'''
                text = text.replace(anchor, bridge + anchor, 1)
                call = "image_data::read_cf_dibv5(&data)"
                if text.count(call) != 1:
                    raise RuntimeError("Pinned arboard clipboard entry changed")
                text = text.replace(call, "image_data::read_clipboard_dibv5(&data)", 1)
                data = text.encode()
            write(destination / member.name, data)
    write(destination / "src/platform/windows_native_bitmap.rs", (ROOT / "engine-patches/windows-native-bitmap.rs").read_bytes())
    print("arboard", REVISION, "prepared at", destination)


if __name__ == "__main__":
    prepare()
