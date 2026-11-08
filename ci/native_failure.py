"""Print the last native build logs so configure errors are visible in Actions."""
from pathlib import Path
import sys

root=Path(sys.argv[1])
logs=sorted(root.rglob('*.log'),key=lambda path:path.stat().st_mtime)[-8:]
for path in logs:
    print(f'Native build log: {path}',flush=True)
    print('\n'.join(path.read_text(encoding='utf-8',errors='replace').splitlines()[-100:]),flush=True)
