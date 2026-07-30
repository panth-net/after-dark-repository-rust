#!/usr/bin/env python3
"""Recursive-descent 68K disassembly of After Dark code resources using capstone.
Follows control flow from entry points so we count REAL instructions, not data."""
import sys, json, struct
from collections import Counter, defaultdict
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from rsrc import parse
import capstone

md = capstone.Cs(capstone.CS_ARCH_M68K, capstone.CS_MODE_BIG_ENDIAN | capstone.CS_MODE_M68K_000)
md.detail = True

CODE_TYPES = {'ADgm', 'CCOD', 'CODE', 'cdev'}

# Terminators: unconditional flow change
END = {'rts', 'rte', 'rtr', 'jmp', 'bra', 'bras', 'braw', 'illegal', 'trap'}
COND = {'bcc','bcs','beq','bge','bgt','bhi','ble','bls','blt','bmi','bne','bpl','bvc','bvs',
        'dbcc','dbcs','dbeq','dbf','dbra','dbge','dbgt','dbhi','dble','dbls','dblt','dbmi',
        'dbne','dbpl','dbt','dbvc','dbvs'}

def rd(data, entries, base=0):
    """Recursive descent. Returns (visited_offsets, aline_counter, insn_counter, bad)."""
    seen = set()
    work = list(entries)
    traps = Counter(); mnem = Counter(); bad = 0; jt = Counter()
    while work:
        pc = work.pop()
        while True:
            if pc in seen or pc < 0 or pc >= len(data) - 1:
                break
            # A-line trap: capstone may not decode; handle ourselves
            w = (data[pc] << 8) | data[pc+1]
            if 0xA000 <= w <= 0xAFFF:
                traps[w] += 1; seen.add(pc); seen.add(pc+1); pc += 2
                continue
            chunk = data[pc:pc+16]
            insns = list(md.disasm(bytes(chunk), base + pc, count=1))
            if not insns:
                bad += 1; seen.add(pc); break
            ins = insns[0]
            seen.update(range(pc, pc + ins.size))
            m = ins.mnemonic.lower().rstrip('.').split('.')[0]
            mnem[m] += 1
            op = ins.op_str
            # jump table call: jsr (d16,a5)
            if m in ('jsr', 'bsr'):
                # follow bsr targets (pc-relative, resolvable)
                if m == 'bsr' and '$' in op and 'a5' not in op:
                    try:
                        t = int(op.strip().lstrip('$').split()[0].replace('$',''), 16) - base
                        if 0 <= t < len(data): work.append(t)
                    except Exception: pass
                if 'a5' in op: jt[op] += 1
                pc += ins.size; continue
            if m in COND:
                if '$' in op:
                    try:
                        tgt = int(op.split('$')[-1].split(',')[0].split()[0], 16) - base
                        if 0 <= tgt < len(data): work.append(tgt)
                    except Exception: pass
                pc += ins.size; continue
            if m in ('bra',):
                if '$' in op:
                    try:
                        tgt = int(op.split('$')[-1].split()[0], 16) - base
                        if 0 <= tgt < len(data): work.append(tgt)
                    except Exception: pass
                break
            if m in ('rts', 'rte', 'rtr', 'jmp', 'illegal', 'trap', 'trapv', 'stop', 'reset'):
                break
            pc += ins.size
    return seen, traps, mnem, bad, jt

if __name__ == '__main__':
    forkdir = Path(sys.argv[1])
    targets = sys.argv[2:] or ['Lunatic Fringe', 'Flying Toasters', 'Bouncing Ball',
                               'Fish!', 'Clock', 'Rainstorm', 'After Dark']
    for name in targets:
        p = forkdir/f'{name}.rsrc'
        if not p.exists():
            print(f'!! missing {p}'); continue
        res = parse(p.read_bytes())
        print(f'\n############ {name} ############')
        allt = Counter(); allm = Counter(); cov_tot = 0; sz_tot = 0
        for r in res:
            if r['type'] not in CODE_TYPES: continue
            d = r['data']
            if r['type'] == 'ADgm':
                entries = [0, 16]
            else:
                # classic CODE header: word jt offset, word n entries, code follows at +4
                entries = [4]
            seen, traps, mnem, bad, jt = rd(d, entries)
            cov = 100.0*len(seen)/max(1, len(d))
            cov_tot += len(seen); sz_tot += len(d)
            allt += traps; allm += mnem
            print(f"  {r['type']!r} {r['id']:>6} size={len(d):>7} reached={len(seen):>7} ({cov:5.1f}%) "
                  f"traps={sum(traps.values()):>5} distinct={len(traps):>4} undecodable={bad} jt={sum(jt.values())}")
        print(f"  TOTAL code={sz_tot} reached={cov_tot} ({100.0*cov_tot/max(1,sz_tot):.1f}%)")
        print(f"  distinct traps reached = {len(allt)}  total trap sites = {sum(allt.values())}")
        # f-line check on reached code only
        fl = sum(c for w, c in allt.items() if w >= 0xF000)
        print(f"  A-line range check: min=${min(allt):04X} max=${max(allt):04X}" if allt else "  no traps")
        print(f"  top opcodes: {', '.join(f'{k}={v}' for k,v in allm.most_common(12))}")
        Path(f'rd_{name.replace(" ","_").replace("!","")}.json').write_text(json.dumps(
            {'traps': {f'{w:04X}': c for w, c in allt.most_common()},
             'opcodes': dict(allm.most_common())}, indent=2))
        print(f"  traps: {' '.join(f'${w:04X}x{c}' for w,c in allt.most_common(40))}")
