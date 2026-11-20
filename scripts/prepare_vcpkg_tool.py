"""Build the pinned vcpkg tool against Ubuntu 18 glibc and its system libcurl."""
from pathlib import Path
import subprocess
import sys

root=Path(__file__).resolve().parents[1]
subprocess.run([sys.executable, str(root/'scripts/prepare_vcpkg_downloads.py'), str(root/'artifacts/vcpkg')], check=True)
script=root/'artifacts/vcpkg/scripts/bootstrap.sh'
text=script.read_text(encoding='utf-8')
original='-DVCPKG_DEVELOPMENT_WARNINGS=OFF"'
patched='-DVCPKG_DEVELOPMENT_WARNINGS=OFF -DCMAKE_CXX_STANDARD_LIBRARIES=-ldl"'
if patched not in text:
    if text.count(original)!=1:raise RuntimeError('Pinned vcpkg bootstrap configuration changed')
    text=text.replace(original,patched,1)
# The prebuilt musl executable cannot dlopen Ubuntu's glibc libcurl. Build on both CPUs.
anchor='# Do the download or build.'
source='vcpkgDownloadTool="OFF"\nvcpkgToolReleaseSha=$VCPKG_TOOL_SOURCE_SHA\n\n'+anchor
if source not in text:
    if text.count(anchor)!=1:raise RuntimeError('Pinned vcpkg bootstrap selection changed')
    text=text.replace(anchor,source,1)
script.write_text(text,encoding='utf-8',newline='\n')
print('Prepared source-built vcpkg with explicit libdl linkage for Ubuntu 18.04')
