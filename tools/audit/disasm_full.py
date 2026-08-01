#!/usr/bin/env python3
"""Full linear disassembly of an After Dark module code resource, with A-line trap
annotation and MacsBug symbol labelling. For small modules this is exhaustive."""
import sys, json
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from rsrc import parse
from macsbug import extract
import capstone

sys.path.insert(0, '.')
from traptable import TRAPS, name as trap_name

# The list of "corrected Memory Manager entries" that used to be patched in here
# is gone: it existed to paper over the hand-written table, and the runtime's
# table is now the only one. Flag bits are still stripped below, which is what
# those overrides were really about — $A31E and $A01E are one trap, not two.


# Flag folding lives in `traptable.canonical`, with the table it belongs to.

md = capstone.Cs(capstone.CS_ARCH_M68K, capstone.CS_MODE_BIG_ENDIAN | capstone.CS_MODE_M68K_000)

def dis(data, base=0, symbols=None, limit=None):
    syms = dict(symbols or {})
    pc = 0
    out = []
    n = len(data) if limit is None else min(limit, len(data))
    while pc < n - 1:
        if pc in syms:
            out.append(f'\n; ======== {syms[pc]} ========')
        w = (data[pc] << 8) | data[pc+1]
        if 0xA000 <= w <= 0xAFFF:
            nm = trap_name(w)
            out.append(f'{base+pc:06x}: {w:04x}            _{nm}   ; TRAP ${w:04X}')
            pc += 2
            continue
        ins = list(md.disasm(bytes(data[pc:pc+16]), base+pc, count=1))
        if not ins:
            out.append(f'{base+pc:06x}: {w:04x}            DC.W    ${w:04X}')
            pc += 2
            continue
        i = ins[0]
        raw = ' '.join(f'{b:02x}' for b in data[pc:pc+i.size])
        out.append(f'{base+pc:06x}: {raw:<16}{i.mnemonic:<8}{i.op_str}')
        pc += i.size
    return out

if __name__ == '__main__':
    fork = Path(sys.argv[1]); rtype = sys.argv[2]; rid = int(sys.argv[3])
    limit = int(sys.argv[4]) if len(sys.argv) > 4 else None
    res = parse(fork.read_bytes())
    for r in res:
        if r['type'] == rtype and r['id'] == rid:
            d = r['data']
            syms = {off: s for off, s in extract(d)}
            print(f"; {fork.name}  {rtype} {rid} {r['name']!r}  size={len(d)}")
            print(f"; MacsBug symbols: {len(syms)}")
            print('\n'.join(dis(d, 0, syms, limit)))
            break
