#!/usr/bin/env python3
"""
Wrap a bare HFS volume in an Apple Partition Map so a real Macintosh ROM can
boot it from emulated SCSI.

`AfterDark-original.img` is a bare HFS volume: its MDB sits at offset 1024 with
no partition map ahead of it. That is fine for Basilisk II (which fakes the disk
driver) but a genuine Mac ROM driving emulated SCSI hardware looks for a Driver
Descriptor Map in block 0 and partition entries after it, finds neither, and
shows the flashing-question-mark floppy.

The source image is never modified; a new image is written.

Usage:
    wrap_apm.py AfterDark-original.img reference/private/AfterDark-apm.img
"""
from __future__ import annotations

import struct
import sys
from pathlib import Path

BLK = 512
VOLUME_START_BLK = 64          # conventional first data block on Apple disks
MAP_ENTRIES = 2

# pmPartStatus bits
ST_VALID, ST_ALLOCATED, ST_INUSE, ST_BOOTABLE = 0x01, 0x02, 0x04, 0x08
ST_READABLE, ST_WRITABLE = 0x10, 0x20
HFS_STATUS = ST_VALID | ST_ALLOCATED | ST_INUSE | ST_BOOTABLE | ST_READABLE | ST_WRITABLE
MAP_STATUS = ST_VALID | ST_ALLOCATED | ST_READABLE | ST_WRITABLE


def fixed(s: str, n: int) -> bytes:
    b = s.encode("ascii")
    if len(b) >= n:
        raise ValueError(f"{s!r} too long for {n}-byte field")
    return b + b"\0" * (n - len(b))


def ddm(total_blocks: int) -> bytes:
    """Block 0: Driver Descriptor Map."""
    b = bytearray(BLK)
    struct.pack_into(
        ">2sHIHHIH", b, 0,
        b"ER",           # sbSig
        BLK,             # sbBlkSize
        total_blocks,    # sbBlkCount
        1,               # sbDevType
        1,               # sbDevId
        0,               # sbData
        0,               # sbDrvrCount — no on-disk SCSI driver
    )
    return bytes(b)


def pm_entry(*, map_blocks: int, start: int, count: int, name: str,
             ptype: str, status: int, boot_size: int = 0) -> bytes:
    """One 512-byte partition map entry."""
    b = bytearray(BLK)
    struct.pack_into(">2sHIII", b, 0,
                     b"PM", 0, map_blocks, start, count)
    b[16:48] = fixed(name, 32)          # pmPartName
    b[48:80] = fixed(ptype, 32)         # pmParType
    struct.pack_into(">III", b, 80, 0, count, status)  # pmLgDataStart, pmDataCnt, pmPartStatus
    # pmLgBootStart, pmBootSize, pmBootAddr, pmBootAddr2, pmBootEntry,
    # pmBootEntry2, pmBootCksum all zero for an HFS partition; the ROM reads the
    # HFS boot blocks instead.
    struct.pack_into(">IIIIIII", b, 92, 0, boot_size, 0, 0, 0, 0, 0)
    b[120:136] = fixed("68000", 16)     # pmProcessor
    return bytes(b)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__)
        return 2
    src, dst = Path(argv[1]), Path(argv[2])
    data = src.read_bytes()

    if data[1024:1026] != b"BD":
        print(f"error: {src} does not look like a bare HFS volume "
              f"(no 'BD' at offset 1024)", file=sys.stderr)
        return 1
    if data[0:2] != b"LK":
        print("warning: boot blocks lack the 'LK' signature; volume may not be bootable",
              file=sys.stderr)
    blessed = struct.unpack_from(">I", data, 1024 + 92)[0]
    if blessed == 0:
        print("warning: drFndrInfo[0] is 0 — no blessed System Folder", file=sys.stderr)

    if len(data) % BLK:
        pad = BLK - (len(data) % BLK)
        data += b"\0" * pad
        print(f"note: padded volume by {pad} bytes to a block boundary")

    vol_blocks = len(data) // BLK
    total_blocks = VOLUME_START_BLK + vol_blocks

    out = bytearray()
    out += ddm(total_blocks)
    out += pm_entry(map_blocks=MAP_ENTRIES, start=1, count=MAP_ENTRIES,
                    name="Apple", ptype="Apple_partition_map", status=MAP_STATUS)
    out += pm_entry(map_blocks=MAP_ENTRIES, start=VOLUME_START_BLK, count=vol_blocks,
                    name="MacOS", ptype="Apple_HFS", status=HFS_STATUS)
    out += b"\0" * (BLK * (VOLUME_START_BLK - 1 - MAP_ENTRIES))
    assert len(out) == VOLUME_START_BLK * BLK, len(out)
    out += data

    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_bytes(bytes(out))
    print(f"wrote {dst}")
    print(f"  block size        {BLK}")
    print(f"  total blocks      {total_blocks}")
    print(f"  HFS partition     block {VOLUME_START_BLK}..{VOLUME_START_BLK + vol_blocks - 1} "
          f"({vol_blocks} blocks, {vol_blocks * BLK} bytes)")
    print(f"  blessed folder ID {blessed}")
    print(f"  size              {len(out)} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
