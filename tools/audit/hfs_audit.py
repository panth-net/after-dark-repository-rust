#!/usr/bin/env python3
"""Independent HFS audit: enumerate every file, fork sizes, type/creator."""
import struct, json, hashlib, sys
from pathlib import Path

IMG = Path(sys.argv[1])
OUT = Path(sys.argv[2]); OUT.mkdir(parents=True, exist_ok=True)

def pad_up(n, m): return (n + m - 1) // m * m

def unpack_extent_record(record):
    vals = struct.unpack('>6H', record)
    return [(vals[i], vals[i+1]) for i in range(0, 6, 2) if vals[i+1]]

def unpack_btree_node(buf, start):
    ndFLink, ndBLink, ndType, ndNHeight, ndNRecs = struct.unpack_from('>LLBBH', buf, start)
    offsets = list(reversed(struct.unpack_from('>%dH' % (ndNRecs+1), buf, start+512-2*(ndNRecs+1))))
    records = [bytes(buf[start+a:start+b]) for a, b in zip(offsets[:-1], offsets[1:])]
    return ndFLink, ndBLink, ndType, ndNHeight, records

def dump_btree(buf):
    _, _, _, _, recs = unpack_btree_node(buf, 0)
    header_rec = recs[0]
    (bthDepth, bthRoot, bthNRecs, bthFNode, bthLNode,
     bthNodeSize, bthKeyLen, bthNNodes, bthFree) = struct.unpack_from('>HLLLLHHLL', header_rec)
    this = bthFNode
    visited = set()
    while True:
        if this in visited: raise RuntimeError('btree loop')
        visited.add(this)
        ndFLink, _, _, _, recs = unpack_btree_node(buf, bthNodeSize*this)
        yield from recs
        if this == bthLNode: break
        this = ndFLink

flat = IMG.read_bytes()
img_sha = hashlib.sha256(flat).hexdigest()
for i in range(0, len(flat), 512):
    if flat[i+1024:i+1026] == b'BD':
        flat = flat[i:]; break
else:
    raise SystemExit('HFS signature not found')

fmt = '>2sLLHHHHHLLHLH28pLHLLLHLL32sHHHL12sL12s'
v = struct.unpack_from(fmt, flat, 1024)
(drSigWord, drCrDate, drLsMod, drAtrb, drNmFls, drVBMSt, drAllocPtr, drNmAlBlks,
 drAlBlkSiz, drClpSiz, drAlBlSt, drNxtCNID, drFreeBks, drVN, drVolBkUp, drVSeqNum,
 drWrCnt, drXTClpSiz, drCTClpSiz, drNmRtDirs, drFilCnt, drDirCnt, drFndrInfo,
 drVCSize, drVBMCSize, drCtlCSize, drXTFlSize, drXTExtRec, drCTFlSize, drCTExtRec) = v
volume_name = drVN.decode('mac_roman')

def block2offset(b): return 512*drAlBlSt + drAlBlkSiz*b
def getextents(exts): return b''.join(flat[block2offset(a):block2offset(a+b)] for a, b in exts)

def get_every_extent(nblocks, firstrecord, cnid, fextoflow, fork):
    accum = 0; extlist = []
    for a, b in unpack_extent_record(firstrecord):
        accum += b; extlist.append((a, b))
    while accum < nblocks:
        nxt = fextoflow[(cnid, fork, accum)]
        for a, b in unpack_extent_record(nxt):
            accum += b; extlist.append((a, b))
    return extlist

extoflow = {}
extbuf = getextents(unpack_extent_record(drXTExtRec))[:drXTFlSize]
for rec in dump_btree(extbuf):
    if not rec or rec[0] != 7: continue
    xkrFkType, xkrFNum, xkrFABN, extrec = struct.unpack_from('>xBLH12s', rec)
    extoflow[(xkrFNum, 'rsrc' if xkrFkType == 0xFF else 'data', xkrFABN)] = extrec

def getfork(size, extrec, cnid, fork):
    if not size: return b''
    nblocks = (size + drAlBlkSiz - 1)//drAlBlkSiz
    return getextents(get_every_extent(nblocks, extrec, cnid, extoflow, fork))[:size]

catbuf = getfork(drCTFlSize, drCTExtRec, 4, 'data')
cnid_info = {1: {'name': '<root parent>', 'parent': None, 'kind': 'dir'},
             2: {'name': volume_name, 'parent': 1, 'kind': 'dir'}}
forks = {}
for rec in dump_btree(catbuf):
    if not rec: continue
    rl = rec[0]
    if rl == 0: continue
    key = rec[2:1+rl]; val = rec[pad_up(1+rl, 2):]
    if len(key) < 5 or len(val) < 2: continue
    par, namelen = struct.unpack_from('>LB', key)
    name = key[5:5+namelen].decode('mac_roman', errors='replace')
    dtype = {1: 'dir', 2: 'file', 3: 'dthread', 4: 'fthread'}.get(val[0])
    datarec = val[2:]
    if dtype == 'dir':
        dirFlags, dirVal, dirID, cr, md, bk, usr, fndr = struct.unpack_from('>HHLLLL16s16s', datarec)
        cnid_info[dirID] = {'name': name, 'parent': par, 'kind': 'dir'}
    elif dtype == 'file':
        (filFlags, filTyp, filUsrWds, filFlNum, filStBlk, filLgLen, filPyLen,
         filRStBlk, filRLgLen, filRPyLen, filCrDat, filMdDat, filBkDat,
         filFndrInfo, filClpSize, filExtRec, filRExtRec) = struct.unpack_from(
            '>BB16sLHLLHLLLLL16sH12s12sxxxx', datarec)
        ftype, creator = struct.unpack_from('>4s4s', filUsrWds)
        cnid_info[filFlNum] = {
            'name': name, 'parent': par, 'kind': 'file',
            'type': ftype.decode('mac_roman', errors='replace'),
            'creator': creator.decode('mac_roman', errors='replace'),
            'data_len': filLgLen, 'rsrc_len': filRLgLen}
        forks[filFlNum] = (getfork(filLgLen, filExtRec, filFlNum, 'data'),
                           getfork(filRLgLen, filRExtRec, filFlNum, 'rsrc'))

def full_path(cnid):
    parts = []; cur = cnid; guard = set()
    while cur in cnid_info and cur not in guard and cur not in (1, 2):
        guard.add(cur); parts.append(cnid_info[cur]['name']); cur = cnid_info[cur]['parent']
    return '/'.join(reversed(parts))

manifest = []
for cnid, info in sorted(cnid_info.items()):
    if info['kind'] != 'file': continue
    data, rsrc = forks[cnid]
    path = full_path(cnid)
    manifest.append({'cnid': cnid, 'path': path, **info,
                     'data_sha256': hashlib.sha256(data).hexdigest(),
                     'rsrc_sha256': hashlib.sha256(rsrc).hexdigest()})
    # dump every resource fork for modules (type ADgm/ADrk-created things)
    if info['rsrc_len'] and info['type'] in ('ADgm', 'ADrk', 'adgm'):
        d = OUT/'forks'; d.mkdir(exist_ok=True)
        safe = info['name'].replace('/', '_')
        (d/f'{safe}.rsrc').write_bytes(rsrc)

(OUT/'hfs_full_manifest.json').write_text(json.dumps(
    {'volume': volume_name, 'image_sha256': img_sha,
     'alloc_block_size': drAlBlkSiz, 'file_count': drFilCnt, 'dir_count': drDirCnt,
     'files': manifest}, indent=2), encoding='utf-8')

print(f'volume={volume_name} img_sha={img_sha}')
print(f'drFilCnt={drFilCnt} drDirCnt={drDirCnt} parsed_files={len(manifest)}')
print()
from collections import Counter
tc = Counter(m['type'] for m in manifest)
print('--- Finder type histogram ---')
for t, n in tc.most_common(): print(f'  {t!r:10} {n}')
print()
print('--- All ADgm (After Dark module) files ---')
adgm = [m for m in manifest if m['type'] == 'ADgm']
for m in sorted(adgm, key=lambda x: x['path']):
    print(f"  {m['name']:<28} data={m['data_len']:>8} rsrc={m['rsrc_len']:>8}  creator={m['creator']!r}  {m['path']}")
print(f'  TOTAL ADgm = {len(adgm)}')
print()
print('--- All files (non-ADgm) ---')
for m in sorted(manifest, key=lambda x: x['path']):
    if m['type'] == 'ADgm': continue
    print(f"  {m['type']!r:8} {m['creator']!r:8} data={m['data_len']:>9} rsrc={m['rsrc_len']:>9}  {m['path']}")
