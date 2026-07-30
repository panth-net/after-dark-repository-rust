#!/usr/bin/env python3
"""Scan 68K code resources across all After Dark modules for A-line traps,
jump-table usage, and 68020+ opcodes. Reports an UPPER BOUND (data words in
code resources can alias trap encodings)."""
import sys, json, struct
from collections import Counter, defaultdict
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from rsrc import parse

# The trap table comes from the runtime — see `traptable.py`. The hand-written
# copy that used to live here was wrong through $A93F..$A952 (CountMItems as
# "InitControls", PlotIcon three entries early, GetItem twelve low) and cost a
# debugging session before anyone checked it against a call site.
from traptable import TRAPS, name as trap_name

CODE_TYPES = {'ADgm','CCOD','CODE','cdev','ADgh','ADlb','adcp','ADrv','LDEF','PACK','WDEF','MDEF','CDEF','INIT','PDEF','DRVR','proc','shlb','scod'}

def scan(data):
    traps = Counter(); jt = 0; n020 = Counter()
    for i in range(0, len(data)-1, 2):
        w = (data[i] << 8) | data[i+1]
        if 0xA000 <= w <= 0xABFF:
            traps[w] += 1
        elif w == 0x4EAD:   # JSR d16(A5) - jump table call
            jt += 1
        elif 0xF000 <= w <= 0xFFFF:
            n020['fline'] += 1
        elif (w & 0xF1C0) == 0x4C00 or (w & 0xF1C0) == 0x4C40:
            n020['mulu/divu.l(68020)'] += 1
        elif (w & 0xFFC0) == 0x06C0 or (w & 0xFFC0) == 0x0EC0:
            n020['bitfield/68020'] += 1
    return traps, jt, n020

if __name__ == '__main__':
    forkdir = Path(sys.argv[1])
    idx = json.loads((forkdir/'_index.json').read_text())
    all_traps = Counter(); trap_modules = defaultdict(set)
    per_module = {}
    for entry in sorted(idx, key=lambda e: e['name']):
        if entry['type'] not in ('ADgm','cdev','MSTG'): continue
        try:
            res = parse((forkdir/entry['file']).read_bytes())
        except Exception as ex:
            print(f"!! {entry['name']}: {ex}"); continue
        traps = Counter(); jt = 0; n020 = Counter(); codebytes = 0; types=Counter()
        for r in res:
            types[r['type']] += 1
            if r['type'] in CODE_TYPES:
                codebytes += r['size']
                t, j, n = scan(r['data'])
                traps += t; jt += j; n020 += n
        all_traps += traps
        for t in traps: trap_modules[t].add(entry['name'])
        per_module[entry['name']] = {'type': entry['type'], 'resources': len(res),
            'code_bytes': codebytes, 'distinct_traps': len(traps),
            'trap_calls': sum(traps.values()), 'jumptable_calls': jt,
            'n020': dict(n020), 'restypes': dict(types)}

    print(f'{"module":<32}{"res":>5}{"codeB":>9}{"traps":>7}{"calls":>7}{"JT":>6}  68020+')
    for name, d in sorted(per_module.items(), key=lambda x: -x[1]['code_bytes']):
        n020s = ','.join(f'{k}={v}' for k, v in d['n020'].items()) or '-'
        print(f"{name:<32}{d['resources']:>5}{d['code_bytes']:>9}{d['distinct_traps']:>7}{d['trap_calls']:>7}{d['jumptable_calls']:>6}  {n020s}")

    print(f'\n=== AGGREGATE: {len(all_traps)} distinct A-line words across {len(per_module)} files ===')
    known = [(w,c) for w,c in all_traps.items() if w in TRAPS]
    unknown = [(w,c) for w,c in all_traps.items() if w not in TRAPS]
    print(f'named={len(known)} unnamed={len(unknown)}')
    print('\n--- top 60 by call count ---')
    for w, c in all_traps.most_common(60):
        print(f'  ${w:04X} {trap_name(w):<22} calls={c:<6} modules={len(trap_modules[w])}')
    print('\n--- traps used by >=30 modules (the core HLE surface) ---')
    core = sorted([(w,len(trap_modules[w])) for w in all_traps], key=lambda x:-x[1])
    for w,m in core:
        if m >= 30: print(f'  ${w:04X} {trap_name(w):<22} modules={m}')
    Path('trapscan.json').write_text(json.dumps({
      'per_module': per_module,
      'traps': {f'{w:04X}': {'name': TRAPS.get(w), 'calls': c, 'modules': sorted(trap_modules[w])}
                for w, c in all_traps.most_common()}}, indent=2))
