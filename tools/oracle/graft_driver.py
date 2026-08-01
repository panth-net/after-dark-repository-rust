#!/usr/bin/env python3
"""Wrap a bare HFS volume in an Apple Partition Map *with a real SCSI driver*,
so a genuine 68K ROM will mount and boot it.

Why this exists
---------------
Classic Mac OS cannot see a SCSI disk until a driver for it has been loaded,
and the ROM loads that driver from the disk itself — from a partition of type
`Apple_Driver43`, located through the driver descriptor map in block 0. A bare
HFS volume (what emulators like Basilisk II happily mount, because they fake
the driver) is therefore invisible to a real ROM. That was the single thing
blocking the layer-L7 oracle.

The driver is copied from a donor image that already has one — Apple's own
System 7.5.3 CD. We are moving Apple's driver between two Apple-formatted
volumes, which is what `Apple HD SC Setup` does when it says "update driver".

    graft_driver.py --donor System753.toast --hfs System7_5_3.img --out boot.img

Block sizes differ between donor and target (a CD addresses 2048-byte blocks, a
hard disk 512), so the driver's *location* is recomputed while its bytes and
its boot descriptor fields are copied verbatim.
"""
from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

BLK = 512
"""Target block size. Hard disks of this era are 512 bytes per block."""

MAP_START = 1
MAP_BLOCKS = 63
"""Apple's convention: the partition map occupies blocks 1..63."""

DRIVER_START = 64
DRIVER_BLOCKS = 64
"""32 KB for the driver, comfortably more than the ~8 KB it needs."""

HFS_START = 128


def find_driver(donor: bytes) -> tuple[bytes, bytes, int]:
    """Return (driver bytes, donor's Apple_Driver43 map entry, donor block size).

    The driver's extent comes from the driver descriptor map in block 0, which
    is authoritative about how much of the partition is actually loadable.
    """
    sig, blk_size, _count, _dt, _di, _data, drv_count = struct.unpack_from(
        ">HHIHHIH", donor, 0
    )
    if sig != 0x4552:
        raise SystemExit("donor block 0 is not an Apple driver descriptor map (ER)")
    if drv_count < 1:
        raise SystemExit("donor declares no drivers")
    # First driver descriptor: block, size in blocks, type.
    d_block, d_size, d_type = struct.unpack_from(">IHH", donor, 0x12)
    start = d_block * blk_size
    length = d_size * blk_size
    driver = donor[start : start + length]
    if len(driver) != length:
        raise SystemExit("donor truncated before the end of its driver")

    # Find the matching Apple_Driver43 partition entry, to copy its boot fields.
    entry = b""
    for i in range(16):
        off = blk_size * (1 + i)
        if donor[off : off + 2] != b"PM":
            break
        p_type = donor[off + 48 : off + 80].split(b"\0")[0]
        if p_type == b"Apple_Driver43":
            entry = donor[off : off + 512]
            break
    if not entry:
        raise SystemExit("donor has no Apple_Driver43 partition entry")
    print(
        f"donor: block size {blk_size}, driver at block {d_block} "
        f"({length} bytes), type {d_type:#06x}"
    )
    return driver, entry, blk_size


def pm_entry(
    map_blocks: int,
    start: int,
    blocks: int,
    name: bytes,
    p_type: bytes,
    status: int,
    boot: bytes = b"",
) -> bytes:
    """Build one 512-byte partition map entry.

    `boot` supplies the boot descriptor tail copied from the donor's driver
    entry: those fields describe the driver *binary* (its load address, entry
    point and checksum), so they must not be invented.
    """
    e = bytearray(512)
    struct.pack_into(">HHIII", e, 0, 0x504D, 0, map_blocks, start, blocks)
    e[16 : 16 + len(name)] = name
    e[48 : 48 + len(p_type)] = p_type
    # pmLgDataStart, pmDataCnt, pmPartStatus
    struct.pack_into(">III", e, 80, 0, blocks, status)
    if boot:
        # pmLgBootStart(92) .. pmProcessor(120..136) copied verbatim.
        e[92:136] = boot[92:136]
    return bytes(e)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--donor", type=Path, required=True,
                    help="Apple-formatted image that already has a driver")
    ap.add_argument("--hfs", type=Path, required=True,
                    help="bare HFS volume to wrap")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--name", default="MacOS", help="volume name in the map")
    args = ap.parse_args()

    donor = args.donor.read_bytes()
    driver, donor_entry, _ = find_driver(donor)
    hfs = args.hfs.read_bytes()

    if hfs[0x400:0x402] != b"BD":
        raise SystemExit(
            f"{args.hfs} does not start with an HFS volume "
            f"(no 'BD' master directory block at 0x400)"
        )
    bootable = hfs[:2] == b"LK"
    print(f"hfs: {len(hfs)} bytes, boot blocks {'present' if bootable else 'absent'}")

    hfs_blocks = (len(hfs) + BLK - 1) // BLK
    total = HFS_START + hfs_blocks

    out = bytearray(total * BLK)

    # ---- block 0: driver descriptor map ----
    struct.pack_into(">HHI", out, 0, 0x4552, BLK, total)
    struct.pack_into(">HH", out, 8, 1, 1)          # sbDevType, sbDevID
    struct.pack_into(">I", out, 12, 0)             # sbData
    struct.pack_into(">H", out, 16, 1)             # sbDrvrCount
    # One driver descriptor: where it is, how big, and that it is a Mac driver.
    struct.pack_into(">IHH", out, 0x12, DRIVER_START, len(driver) // BLK, 1)

    # ---- partition map ----
    entries = [
        pm_entry(3, MAP_START, MAP_BLOCKS, b"Apple", b"Apple_partition_map", 0x33),
        pm_entry(3, DRIVER_START, DRIVER_BLOCKS, b"Macintosh", b"Apple_Driver43",
                 0x7F, donor_entry),
        pm_entry(3, HFS_START, hfs_blocks, args.name.encode("mac_roman"),
                 b"Apple_HFS", 0x3F),
    ]
    for i, e in enumerate(entries):
        out[BLK * (MAP_START + i) : BLK * (MAP_START + i) + 512] = e

    # ---- driver bytes and the volume itself ----
    out[DRIVER_START * BLK : DRIVER_START * BLK + len(driver)] = driver
    out[HFS_START * BLK : HFS_START * BLK + len(hfs)] = hfs

    args.out.write_bytes(out)
    print(
        f"wrote {args.out} ({len(out)} bytes): map at {MAP_START}, "
        f"driver at {DRIVER_START} ({len(driver)} bytes), "
        f"HFS at {HFS_START} ({hfs_blocks} blocks)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
