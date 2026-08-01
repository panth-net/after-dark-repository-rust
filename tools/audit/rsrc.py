#!/usr/bin/env python3
"""Minimal, bounds-checked resource fork reader usable as a library + CLI."""
import struct, sys, json
from pathlib import Path

def parse(b):
    if len(b) < 16: raise ValueError('too short')
    data_off, map_off, data_len, map_len = struct.unpack_from('>LLLL', b, 0)
    if data_off+data_len > len(b) or map_off+map_len > len(b):
        raise ValueError(f'bad offsets d={data_off}+{data_len} m={map_off}+{map_len} len={len(b)}')
    tlo, nlo = struct.unpack_from('>HH', b, map_off+24)
    tl = map_off+tlo; nl = map_off+nlo
    ntypes = struct.unpack_from('>H', b, tl)[0]+1
    res = []
    for ti in range(ntypes):
        e = tl+2+ti*8
        typ = b[e:e+4].decode('mac_roman', 'replace')
        _c = struct.unpack_from('>H', b, e+4)[0]
        cnt = 0 if _c == 0xFFFF else _c + 1
        rb = tl+struct.unpack_from('>H', b, e+6)[0]
        for ri in range(cnt):
            r = rb+ri*12
            rid = struct.unpack_from('>h', b, r)[0]
            nrel = struct.unpack_from('>h', b, r+2)[0]
            attrs = b[r+4]
            doff = int.from_bytes(b[r+5:r+8], 'big')
            ad = data_off+doff
            size = struct.unpack_from('>L', b, ad)[0]
            payload = b[ad+4:ad+4+size]
            name = None
            if nrel != -1:
                np_ = nl+nrel; n = b[np_]
                name = b[np_+1:np_+1+n].decode('mac_roman', 'replace')
            res.append({'type': typ, 'id': rid, 'name': name, 'attrs': attrs,
                        'size': size, 'data': payload})
    return res

def summary(res):
    from collections import Counter, defaultdict
    c = Counter(r['type'] for r in res)
    byt = defaultdict(int)
    for r in res: byt[r['type']] += r['size']
    return c, byt

if __name__ == '__main__':
    b = Path(sys.argv[1]).read_bytes()
    res = parse(b)
    c, byt = summary(res)
    print(f'total resources: {len(res)}  types: {len(c)}')
    for t, n in sorted(c.items(), key=lambda x: -byt[x[0]]):
        print(f'  {t!r:8} n={n:<4} bytes={byt[t]:>8}')
    if len(sys.argv) > 2:
        want = sys.argv[2]
        for r in res:
            if r['type'] == want:
                print(f"\n--- {r['type']} {r['id']} {r['name']!r} size={r['size']}")
                print(r['data'][:96].hex(' '))
