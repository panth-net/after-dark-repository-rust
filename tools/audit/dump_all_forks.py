#!/usr/bin/env python3
"""Dump every resource fork from the HFS image, then summarize resource types per module."""
import struct, json, hashlib, sys, re
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))

IMG = Path(sys.argv[1]); OUT = Path(sys.argv[2]); OUT.mkdir(parents=True, exist_ok=True)

def pad_up(n, m): return (n + m - 1) // m * m
def uxr(r):
    v = struct.unpack('>6H', r)
    return [(v[i], v[i+1]) for i in range(0, 6, 2) if v[i+1]]
def ubn(buf, s):
    fl, bl, ty, h, nr = struct.unpack_from('>LLBBH', buf, s)
    offs = list(reversed(struct.unpack_from('>%dH' % (nr+1), buf, s+512-2*(nr+1))))
    return fl, [bytes(buf[s+a:s+b]) for a, b in zip(offs[:-1], offs[1:])]
def dump_btree(buf):
    _, recs = ubn(buf, 0)
    (d, root, nrec, fnode, lnode, nsz, klen, nn, fr) = struct.unpack_from('>HLLLLHHLL', recs[0])
    this, seen = fnode, set()
    while True:
        if this in seen: raise RuntimeError('loop')
        seen.add(this)
        fl, recs = ubn(buf, nsz*this)
        yield from recs
        if this == lnode: break
        this = fl

flat = IMG.read_bytes()
for i in range(0, len(flat), 512):
    if flat[i+1024:i+1026] == b'BD': flat = flat[i:]; break
fmt = '>2sLLHHHHHLLHLH28pLHLLLHLL32sHHHL12sL12s'
v = struct.unpack_from(fmt, flat, 1024)
drAlBlkSiz, drAlBlSt = v[8], v[10]
drXTFlSize, drXTExtRec, drCTFlSize, drCTExtRec = v[26], v[27], v[28], v[29]

def b2o(b): return 512*drAlBlSt + drAlBlkSiz*b
def gx(e): return b''.join(flat[b2o(a):b2o(a+n)] for a, n in e)
extoflow = {}
for rec in dump_btree(gx(uxr(drXTExtRec))[:drXTFlSize]):
    if not rec or rec[0] != 7: continue
    ft, fn, ab, er = struct.unpack_from('>xBLH12s', rec)
    extoflow[(fn, 'rsrc' if ft == 0xFF else 'data', ab)] = er
def gev(nb, first, cnid, fork):
    acc = 0; el = []
    for a, n in uxr(first): acc += n; el.append((a, n))
    while acc < nb:
        for a, n in uxr(extoflow[(cnid, fork, acc)]): acc += n; el.append((a, n))
    return el
def getfork(size, er, cnid, fork):
    if not size: return b''
    return gx(gev((size+drAlBlkSiz-1)//drAlBlkSiz, er, cnid, fork))[:size]

names = {}
out = []
for rec in dump_btree(getfork(drCTFlSize, drCTExtRec, 4, 'data')):
    if not rec or rec[0] == 0: continue
    rl = rec[0]; key = rec[2:1+rl]; val = rec[pad_up(1+rl, 2):]
    if len(key) < 5 or len(val) < 2: continue
    par, nl = struct.unpack_from('>LB', key)
    name = key[5:5+nl].decode('mac_roman', errors='replace')
    if val[0] != 2: continue
    dr = val[2:]
    (fF, fT, uw, cnid, sb, dl, dp, rsb, rl_, rp, cd, md, bd, fi, cs, der, rer) = \
        struct.unpack_from('>BB16sLHLLHLLLLL16sH12s12sxxxx', dr)
    ftype, creator = struct.unpack_from('>4s4s', uw)
    rsrc = getfork(rl_, rer, cnid, 'rsrc')
    if not rsrc: continue
    safe = re.sub(r'[^A-Za-z0-9 ._!\'-]+', '_', name)
    p = OUT/f'{safe}.rsrc'
    p.write_bytes(rsrc)
    out.append({'name': name, 'type': ftype.decode('mac_roman', 'replace'),
                'creator': creator.decode('mac_roman', 'replace'),
                'rsrc_len': rl_, 'file': p.name})
(OUT/'_index.json').write_text(json.dumps(out, indent=2))
print(f'dumped {len(out)} resource forks to {OUT}')
