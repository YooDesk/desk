"""Fix LLVM 10.0.0's signed NEON lane macro while preserving strict vector checks."""
from pathlib import Path
import subprocess

resource=Path(subprocess.check_output(['clang-10','-print-resource-dir'],text=True).strip())
header=resource/'include/arm_neon.h'
text=header.read_text(encoding='utf-8')
lines=text.splitlines(keepends=True)
fixed=0
for index,line in enumerate(lines):
    if 'int32x4_t __reint1 = __builtin_shufflevector(' in line and '*(uint32x2_t *)' in line:
        lines[index]=line.replace('*(uint32x2_t *)','*(int32x2_t *)')
        fixed+=1
if fixed:
    header.write_text(''.join(lines),encoding='utf-8')
print(f'LLVM 10 signed NEON lane macro corrections: {fixed}',flush=True)
probe='''#include <arm_neon.h>
int32x4_t yoodesk_dotprod_probe(int32x4_t a, int8x16_t b, int8x8_t filter) {
    return vdotq_lane_s32(a, b, filter, 0);
}
'''
subprocess.run(['clang-10','-march=armv8.2-a+dotprod','-flax-vector-conversions=none',
                '-Werror','-x','c','-c','-o','/tmp/yoodesk-neon-probe.o','-'],
               input=probe,text=True,check=True)
