"""Link the pinned vcpkg tool's dlopen usage against libdl on glibc before 2.34."""
from pathlib import Path

root=Path(__file__).resolve().parents[1]
script=root/'artifacts/vcpkg/scripts/bootstrap.sh'
text=script.read_text(encoding='utf-8')
original='-DVCPKG_DEVELOPMENT_WARNINGS=OFF"'
patched='-DVCPKG_DEVELOPMENT_WARNINGS=OFF -DCMAKE_CXX_STANDARD_LIBRARIES=-ldl"'
if patched not in text:
    if text.count(original)!=1:raise RuntimeError('Pinned vcpkg bootstrap configuration changed')
    script.write_text(text.replace(original,patched,1),encoding='utf-8',newline='\n')
print('Prepared explicit libdl linkage for the Ubuntu 18.04 vcpkg tool')
