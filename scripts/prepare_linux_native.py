"""Add the software encoders used by YooDesk to the isolated RustDesk vcpkg manifest."""
from pathlib import Path
import json

ROOT=Path(__file__).resolve().parents[1]
tree=ROOT/"artifacts/rustdesk-engine-src"
manifest=tree/"vcpkg.json"
data=json.loads(manifest.read_text(encoding="utf-8"))
if not any((item if isinstance(item,str) else item['name'])=='libsodium' and (isinstance(item,str) or 'platform' not in item) for item in data['dependencies']):
    data['dependencies'].append('libsodium')
manifest.write_text(json.dumps(data,indent=2)+'\n',encoding="utf-8")
port=tree/"res/vcpkg/ffmpeg/portfile.cmake"
text=port.read_text(encoding="utf-8")
addition='if(VCPKG_TARGET_IS_LINUX)\n    string(APPEND OPTIONS " --enable-gpl --enable-libx264 --enable-libx265 --enable-encoder=libx264 --enable-encoder=libx265 ")'
text=text.replace(addition.replace('libx265 ")','libx265")'),addition)
if addition not in text:
    if text.count('if(VCPKG_TARGET_IS_LINUX)')!=1:raise ValueError('FFmpeg Linux port anchor changed')
    text=text.replace('if(VCPKG_TARGET_IS_LINUX)',addition,1).replace('--disable-swresample','--enable-swresample')
port.write_text(text,encoding="utf-8")
manifest=tree/"res/vcpkg/ffmpeg/vcpkg.json"
data=json.loads(manifest.read_text(encoding="utf-8"))
for dependency in data['dependencies']:
    if isinstance(dependency,dict) and dependency['name'] in ['x264','x265']:
        dependency['platform']='linux | (windows & !uwp)'
names={item if isinstance(item,str) else item['name'] for item in data['dependencies']}
for name in ['x264','x265']:
    if name not in names:
        data['dependencies'].append({'name':name,'platform':'linux | (windows & !uwp)'})
manifest.write_text(json.dumps(data,indent=2)+'\n',encoding="utf-8")
print('Linux software codecs enabled in isolated RustDesk source')
