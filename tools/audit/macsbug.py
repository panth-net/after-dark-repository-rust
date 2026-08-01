#!/usr/bin/env python3
"""Extract MacsBug procedure-name symbols from 68K code resources.
Format (Think C / MPW): after an RTS/JMP, a name record:
  - byte 0x80|len (len 1..31) followed by len chars, OR
  - byte with value 0x20..0x7F starting an 8-char fixed name (older), OR
  - 0x80 followed by len byte (Think C variant)
We use the common variant: high-bit-set length byte then MacRoman chars, padded to even.
"""
import sys, re, json
from collections import Counter
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from rsrc import parse

CODE_TYPES = {'ADgm', 'CCOD', 'CODE', 'cdev'}
NAME_RE = re.compile(rb'[\x80-\x9f]')

def extract(data):
    out = []
    i = 0
    n = len(data)
    while i < n:
        b = data[i]
        if 0x81 <= b <= 0x9f:  # 0x80 | length, length 1..31
            ln = b & 0x7f
            if i + 1 + ln <= n:
                cand = data[i+1:i+1+ln]
                if all(32 <= c < 127 for c in cand):
                    s = cand.decode('ascii')
                    # must look like an identifier
                    if re.fullmatch(r'[A-Za-z_][A-Za-z0-9_.%]*', s) and len(s) >= 3:
                        out.append((i, s))
                        i += 1 + ln
                        continue
        i += 1
    return out

if __name__ == '__main__':
    forkdir = Path(sys.argv[1])
    targets = sys.argv[2:]
    idx = json.loads((forkdir/'_index.json').read_text())
    if targets:
        entries = [e for e in idx if e['name'] in targets]
    else:
        entries = [e for e in idx if e['type'] in ('ADgm', 'cdev')]
    grand = 0
    for e in sorted(entries, key=lambda x: x['name']):
        res = parse((forkdir/e['file']).read_bytes())
        syms = []
        for r in res:
            if r['type'] in CODE_TYPES:
                syms += [(r['type'], r['id'], off, s) for off, s in extract(r['data'])]
        grand += len(syms)
        if targets:
            print(f"\n===== {e['name']}  ({len(syms)} symbols) =====")
            for t, rid, off, s in syms:
                print(f'  {t} {rid:>6} +0x{off:05x}  {s}')
        else:
            print(f"{e['name']:<32} symbols={len(syms):>5}")
    print(f'\nTOTAL symbols across {len(entries)} files = {grand}')
