#!/usr/bin/env python3
"""
Oracle frame capture: boot the original System 7.5.2 + After Dark 2.0x disk in
QEMU's q800 machine and capture deterministic screenshots via QMP.

This is Track B / layer L7 of the verification stack. It is a
TEST INSTRUMENT ONLY. It never ships, and the ROM it needs stays test-local.

Usage:
    qemu_capture.py --at 30 60 90 --out captures/boot
    qemu_capture.py --interactive          # leaves a VNC display up on :1
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ROM = REPO / "reference/private/Quadra800.ROM"
IMG = REPO / "AfterDark-original.img"


# ---------------------------------------------------------------- PNG encoding


def write_png(path: Path, width: int, height: int, rgb: bytes) -> None:
    """Minimal PNG writer (no third-party deps, so the oracle has no supply chain)."""

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    raw = bytearray()
    stride = width * 3
    for y in range(height):
        raw.append(0)  # filter type 0
        raw += rgb[y * stride : (y + 1) * stride]

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def read_ppm(path: Path) -> tuple[int, int, bytes]:
    """Parse the binary PPM (P6) that QEMU's screendump emits."""
    data = path.read_bytes()
    fields: list[bytes] = []
    pos = 0
    while len(fields) < 4:
        while pos < len(data) and data[pos : pos + 1].isspace():
            pos += 1
        if data[pos : pos + 1] == b"#":  # comment
            while pos < len(data) and data[pos] != 0x0A:
                pos += 1
            continue
        start = pos
        while pos < len(data) and not data[pos : pos + 1].isspace():
            pos += 1
        fields.append(data[start:pos])
    if fields[0] != b"P6":
        raise ValueError(f"expected P6 PPM, got {fields[0]!r}")
    width, height, maxval = (int(f) for f in fields[1:4])
    if maxval != 255:
        raise ValueError(f"unsupported maxval {maxval}")
    pos += 1  # single whitespace byte after the header
    return width, height, data[pos : pos + width * height * 3]


# ---------------------------------------------------------------- QMP client


class Qmp:
    """Minimal QMP client. QEMU speaks newline-delimited JSON over a socket."""

    def __init__(self, path: Path, timeout: float = 60.0) -> None:
        deadline = time.monotonic() + timeout
        last: Exception | None = None
        while time.monotonic() < deadline:
            try:
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.settimeout(20.0)
                self.sock.connect(str(path))
                break
            except OSError as exc:
                last = exc
                time.sleep(0.2)
        else:
            raise RuntimeError(f"could not connect to QMP at {path}: {last}")
        self.buf = b""
        self._recv()                      # greeting
        self.command("qmp_capabilities")

    def _recv(self) -> dict:
        while b"\n" not in self.buf:
            more = self.sock.recv(65536)
            if not more:
                raise RuntimeError("QMP connection closed")
            self.buf += more
        line, _, self.buf = self.buf.partition(b"\n")
        return json.loads(line)

    def command(self, name: str, **args) -> dict:
        payload = {"execute": name}
        if args:
            payload["arguments"] = args
        self.sock.sendall(json.dumps(payload).encode() + b"\n")
        while True:
            msg = self._recv()
            if "error" in msg:
                raise RuntimeError(f"QMP {name} failed: {msg['error']}")
            if "return" in msg:
                return msg["return"]
            # otherwise an async event; keep reading

    def hmp(self, line: str) -> str:
        return self.command("human-monitor-command", **{"command-line": line})

    def screendump(self, dest: Path) -> None:
        self.hmp(f"screendump {dest}")

    def quit(self) -> None:
        try:
            self.command("quit")
        except Exception:
            pass


# ---------------------------------------------------------------- main


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--at", type=float, nargs="+", default=[20, 40, 60, 90],
                    help="seconds after boot at which to capture")
    ap.add_argument("--out", type=Path, default=REPO / "tests/oracle/captures/boot")
    ap.add_argument("--ram", type=int, default=128)
    ap.add_argument("--rom", type=Path, default=ROM)
    ap.add_argument("--img", type=Path, default=IMG)
    ap.add_argument("--cd", type=Path, default=None,
                    help="bootable CD image (.toast/.iso). A genuine ROM boots a "
                         "CD from the ROM's own driver, so this is the way in "
                         "when the hard disk has no driver partition of its own.")
    ap.add_argument("--no-hd", action="store_true",
                    help="omit the hard disk entirely (CD-only boot)")
    ap.add_argument("--writable", action="store_true",
                    help="allow the guest to write the disk (default read-only)")
    ap.add_argument("--interactive", action="store_true",
                    help="serve VNC on :1 and wait for Ctrl-C instead of capturing")
    args = ap.parse_args()

    needed = [(args.rom, "ROM")]
    if not args.no_hd:
        needed.append((args.img, "disk image"))
    if args.cd is not None:
        needed.append((args.cd, "CD image"))
    for p, what in needed:
        if not p.exists():
            print(f"error: {what} not found at {p}", file=sys.stderr)
            return 2

    args.out.mkdir(parents=True, exist_ok=True)
    # AF_UNIX paths are capped near 104 bytes on macOS, so the socket must live in
    # a short directory regardless of where captures go.
    sock_dir = Path(tempfile.mkdtemp(prefix="adq", dir="/tmp"))
    qmp_path = sock_dir / "q.sock"

    display = ["-vnc", ":1"] if args.interactive else ["-display", "none"]
    ro = "off" if args.writable else "on"
    cmd = [
        "qemu-system-m68k", "-M", "q800", "-m", str(args.ram),
        "-bios", str(args.rom),
    ]
    if not args.no_hd:
        cmd += [
            "-drive", f"file={args.img},format=raw,if=none,id=hd0,readonly={ro}",
            "-device", "scsi-hd,drive=hd0,bus=scsi.0,scsi-id=0",
        ]
    if args.cd is not None:
        # SCSI ID 3 is where Apple's ROM conventionally looks for a CD-ROM.
        cmd += [
            "-drive", f"file={args.cd},format=raw,if=none,id=cd0,readonly=on",
            "-device", "scsi-cd,drive=cd0,bus=scsi.0,scsi-id=3",
        ]
    cmd += [
        "-qmp", f"unix:{qmp_path},server,nowait",
        "-serial", "none",
        *display,
    ]
    print("$", " ".join(cmd), flush=True)
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)

    try:
        qmp = Qmp(qmp_path)
        started = time.monotonic()
        print(f"QMP connected; qemu version {qmp.command('query-version')['qemu']}", flush=True)

        if args.interactive:
            print("VNC on localhost:5901 — connect and drive it. Ctrl-C to stop.", flush=True)
            while proc.poll() is None:
                time.sleep(1)
            return 0

        manifest = []
        for t in sorted(args.at):
            wait = t - (time.monotonic() - started)
            if wait > 0:
                time.sleep(wait)
            if proc.poll() is not None:
                print(f"qemu exited early (code {proc.returncode})", file=sys.stderr)
                break
            ppm = args.out / f"t{int(t):04d}.ppm"
            png = args.out / f"t{int(t):04d}.png"
            qmp.screendump(ppm)
            for _ in range(50):                       # screendump is async-ish
                if ppm.exists() and ppm.stat().st_size > 0:
                    break
                time.sleep(0.1)
            w, h, rgb = read_ppm(ppm)
            write_png(png, w, h, rgb)
            ppm.unlink()
            digest = zlib.crc32(rgb) & 0xFFFFFFFF
            nonblack = sum(1 for i in range(0, len(rgb), 3) if rgb[i : i + 3] != b"\0\0\0")
            manifest.append({"t": t, "png": png.name, "w": w, "h": h,
                             "crc32": f"{digest:08x}", "nonblack_px": nonblack})
            print(f"  t={t:>5.0f}s  {w}x{h}  crc32={digest:08x}  "
                  f"non-black={nonblack}  -> {png.name}", flush=True)

        (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2))
        qmp.quit()
        return 0
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
        qmp_path.unlink(missing_ok=True)
        try:
            sock_dir.rmdir()
        except OSError:
            pass


if __name__ == "__main__":
    sys.exit(main())
