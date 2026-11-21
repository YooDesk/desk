"""Install the checksummed Loongnix 20 development/runtime closure for ABI 1.0."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

lock_path = Path(__file__).with_name('oldworld-sysroot.lock.json')
lock = json.loads(lock_path.read_text(encoding='utf-8'))
root = Path(sys.argv[1]).absolute()
cache = Path(sys.argv[2]).absolute()
cache.mkdir(parents=True, exist_ok=True)
marker = root / ('.yoodesk-sysroot-' + hashlib.sha256(lock_path.read_bytes()).hexdigest())
if marker.is_file():
    print('Reusing pinned old-world sysroot')
    sys.exit(0)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def fetch(entry):
    path = cache / Path(entry['filename']).name
    if path.is_file() and digest(path) == entry['sha256']:
        return path
    for attempt in range(3):
        try:
            with urllib.request.urlopen(lock['base_url'] + entry['filename'], timeout=60) as response:
                temporary = path.with_suffix(path.suffix + '.partial')
                with temporary.open('wb') as output:
                    while block := response.read(1024 * 1024):
                        output.write(block)
            if digest(temporary) != entry['sha256']:
                raise ValueError('Checksum mismatch: ' + entry['name'])
            temporary.replace(path)
            return path
        except (OSError, ValueError):
            if attempt == 2:
                raise
            time.sleep(2 * (attempt + 1))


with ThreadPoolExecutor(max_workers=6) as pool:
    archives = list(pool.map(fetch, lock['files']))
for archive in archives:
    subprocess.run(['dpkg-deb', '-x', str(archive), str(root)], check=True)

# Keep every absolute target link within the foreign filesystem on the host.
for path in root.rglob('*'):
    if path.is_symlink():
        target = os.readlink(path)
        if target.startswith('/'):
            relative = os.path.relpath(root / target.lstrip('/'), path.parent)
            path.unlink()
            path.symlink_to(relative)
marker.touch()
print('Installed', len(archives), 'pinned Loongnix 20 packages')
