#!/usr/bin/env python3
"""FFX ring drain and camera handshake over the live Xenia guest memory."""
from __future__ import annotations

import ctypes
import math
import struct
import time
from dataclasses import dataclass

from ppcgen import Cave

PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_VM_READ = 0x0010
PROCESS_VM_WRITE = 0x0020
PROCESS_VM_OPERATION = 0x0008

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
kernel32.OpenProcess.restype = ctypes.c_void_p
kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
kernel32.ReadProcessMemory.argtypes = [ctypes.c_void_p, ctypes.c_ulonglong,
                                       ctypes.c_void_p, ctypes.c_size_t,
                                       ctypes.POINTER(ctypes.c_size_t)]
kernel32.ReadProcessMemory.restype = ctypes.c_int
kernel32.WriteProcessMemory.argtypes = [ctypes.c_void_p, ctypes.c_ulonglong,
                                        ctypes.c_void_p, ctypes.c_size_t,
                                        ctypes.POINTER(ctypes.c_size_t)]
kernel32.WriteProcessMemory.restype = ctypes.c_int


class ProcessMemory:
    def __init__(self, pid: int, host_base: int = 0, write: bool = False):
        access = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ
        if write:
            access |= PROCESS_VM_WRITE | PROCESS_VM_OPERATION
        self.handle = kernel32.OpenProcess(access, False, pid)
        if not self.handle:
            raise OSError(f"OpenProcess failed for pid {pid}: "
                          f"error {ctypes.get_last_error()}")
        self.host_base = host_base
        self.write_enabled = write

    def close(self) -> None:
        if self.handle:
            kernel32.CloseHandle(self.handle)
            self.handle = None

    def read(self, guest_addr: int, size: int) -> bytes | None:
        buf = ctypes.create_string_buffer(size)
        got = ctypes.c_size_t(0)
        ok = kernel32.ReadProcessMemory(self.handle, self.host_base + guest_addr,
                                        buf, size, ctypes.byref(got))
        if not ok or got.value != size:
            return None
        return buf.raw

    def write(self, guest_addr: int, data: bytes) -> bool:
        if not self.write_enabled:
            raise RuntimeError("process memory was opened read-only")
        buf = ctypes.create_string_buffer(data, len(data))
        wrote = ctypes.c_size_t(0)
        ok = kernel32.WriteProcessMemory(self.handle, self.host_base + guest_addr,
                                         buf, len(data), ctypes.byref(wrote))
        return bool(ok) and wrote.value == len(data)

    def u32(self, guest_addr: int) -> int | None:
        data = self.read(guest_addr, 4)
        return None if data is None else struct.unpack(">I", data)[0]

    def write_u32(self, guest_addr: int, value: int) -> bool:
        return self.write(guest_addr, struct.pack(">I", value & 0xFFFFFFFF))

    def floats(self, guest_addr: int, count: int = 3) -> tuple[float, ...] | None:
        data = self.read(guest_addr, 4 * count)
        if data is None:
            return None
        return struct.unpack(f">{count}f", data)


def parse_header(data: bytes, offsets: dict) -> dict:
    def u32(off):
        return struct.unpack_from(">I", data, off)[0]

    def vec(off):
        return struct.unpack_from(">3f", data, off)

    return {
        "write_index": u32(int(offsets["write_index"])),
        "dropped_cave": u32(int(offsets["dropped"])),
        "magic": u32(int(offsets["magic"])),
        "version": u32(int(offsets["version"])),
        "flags": u32(int(offsets["flags"])),
        "cam_enable": u32(int(offsets["cam_enable"])),
        "cam_ptr": u32(int(offsets["cam_ptr"])),
        "cam_seq": u32(int(offsets["cam_seq"])),
        "cam_eye": vec(int(offsets["cam_eye"])),
        "cam_target": vec(int(offsets["cam_target"])),
        "cam_read_eye": vec(int(offsets["cam_read_eye"])),
        "cam_read_target": vec(int(offsets["cam_read_target"])),
        "cam_frame": u32(int(offsets["cam_frame"])),
        "ring_cap": u32(int(offsets["ring_cap"])),
        "ring_recsz": u32(int(offsets["ring_recsz"])),
        "warp_mode": u32(int(offsets["warp_mode"])),
        "warp_calls": u32(int(offsets["warp_calls"])),
        "warp_map": u32(int(offsets["warp_map"])),
        "warp_msb": u32(int(offsets["warp_msb"])),
    }


@dataclass
class FfxRecord:
    n: int
    ms: int
    ffx: int
    owner: int
    lr: int
    x: float
    y: float
    z: float
    dropped: int
    write_index: int

    def to_dict(self) -> dict:
        return {
            "n": self.n, "ms": self.ms, "ffx": self.ffx, "owner": self.owner,
            "lr": self.lr, "x": self.x, "y": self.y, "z": self.z,
            "dropped": self.dropped, "write_index": self.write_index,
        }


class RingDrainer:
    """Read FFX records from the guest ring, lagging the live producer."""

    def __init__(self, mem: ProcessMemory, profile: dict, *, lag: int = 16,
                 text_lo: int | None = None, text_hi: int | None = None):
        self.mem = mem
        self.ring = profile["ring"]
        self.offsets = self.ring["offsets"]
        self.base = int(self.ring["base"])
        self.cap = int(self.ring["cap"])
        self.recsz = int(self.ring["recsz"])
        self.lag = lag
        self.text_lo = text_lo
        self.text_hi = text_hi
        self.cursor: int | None = None
        self.dropped = 0
        self.invalid = 0
        self.n = 0
        self.records: list[FfxRecord] = []
        self.last_write_index = 0

    def header(self) -> dict | None:
        data = self.mem.read(self.base, int(self.ring["header_size"]))
        if data is None:
            return None
        return parse_header(data, self.offsets)

    def drain(self, now_ms: int, *, final: bool = False) -> list[FfxRecord]:
        header = self.header()
        if header is None or header["magic"] != int(self.ring["magic"]):
            return []
        write_index = header["write_index"]
        self.last_write_index = write_index
        if self.cursor is None:
            # Ring history is intentionally retained across boot/map load.
            self.cursor = max(0, write_index - self.cap)
        if write_index < self.cursor:
            # u32 wrap: keep monotonic bookkeeping.
            self.cursor -= 1 << 32
        pending = write_index - self.cursor
        if pending > self.cap:
            self.dropped += pending - self.cap
            self.cursor = write_index - self.cap
            pending = self.cap
        upto = write_index if final else max(self.cursor, write_index - self.lag)
        out: list[FfxRecord] = []
        while self.cursor < upto:
            slot = self.cursor % self.cap
            addr = self.base + int(self.ring["data_offset"]) + slot * self.recsz
            data = self.mem.read(addr, self.recsz)
            self.cursor += 1
            if data is None:
                self.invalid += 1
                continue
            ffx, owner, lr, x, y, z = struct.unpack(">IIIfff", data[:24])
            if (ffx == 0 or not all(math.isfinite(v) for v in (x, y, z))
                    or (self.text_lo is not None
                        and not self.text_lo <= lr < self.text_hi)):
                self.invalid += 1
                continue
            rec = FfxRecord(self.n, now_ms, ffx, owner, lr, x, y, z,
                            self.dropped, write_index)
            self.n += 1
            self.records.append(rec)
            out.append(rec)
        return out

    def camera_status(self) -> dict | None:
        header = self.header()
        if header is None:
            return None
        return {
            "cam_ptr": header["cam_ptr"],
            "cam_frame": header["cam_frame"],
            "cam_seq": header["cam_seq"],
            "cam_enable": header["cam_enable"],
            "read_eye": header["cam_read_eye"],
            "read_target": header["cam_read_target"],
        }


def wait_camera_frame(drainer: RingDrainer, first_frame: int, count: int = 2,
                      timeout_s: float = 0.12) -> int | None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        status = drainer.camera_status()
        if status is not None:
            delta = (status["cam_frame"] - first_frame) & 0xFFFFFFFF
            if delta >= count:
                return status["cam_frame"]
        time.sleep(0.005)
    return None


def apply_camera(mem: ProcessMemory, drainer: RingDrainer, profile: dict,
                 eye: tuple[float, float, float],
                 target: tuple[float, float, float],
                 *, timeout_s: float = 0.12) -> dict:
    """Disable -> wait two cave ticks -> publish -> enable (no torn vectors)."""
    offsets = profile["ring"]["offsets"]
    base = int(profile["ring"]["base"])
    status = drainer.camera_status() or {}
    first_frame = int(status.get("cam_frame", 0))
    eye_addr = base + int(offsets["cam_eye"])
    target_addr = base + int(offsets["cam_target"])
    enable_addr = base + int(offsets["cam_enable"])
    seq_addr = base + int(offsets["cam_seq"])

    mem.write_u32(enable_addr, 0)
    waited = wait_camera_frame(drainer, first_frame, 2, timeout_s)
    mem.write(eye_addr, struct.pack(">3f", *eye))
    mem.write(target_addr, struct.pack(">3f", *target))
    seq = mem.u32(seq_addr) or 0
    mem.write_u32(seq_addr, (seq + 1) & 0xFFFFFFFF)
    mem.write_u32(enable_addr, 1)
    return {
        "frame_before": first_frame,
        "frame_after_wait": waited,
        "eye": eye,
        "target": target,
        "seq": (seq + 1) & 0xFFFFFFFF,
        "enabled": True,
    }


def build_camera_cave(base: int, ring_base: int, offsets: dict,
                      next_addr: int = 0x82BFB33C) -> list[int]:
    """Camera readback/override cave hooked at 0x82BFB338.

    ``r31`` is the camera-effects object (``CamFx``); ``r30`` is its camera
    block.  ``CamFx_stateFollow`` has just eased the shown eye/target to
    +0x20/+0x30, and the view matrix at 0x82BFB344 is built from those two
    vectors (360 evidence: 0x82BFB338 ``addi r6,r31,0x20``; 0x82BFB33C
    ``addi r5,r31,0x30``).  The cave publishes the object pointer, copies the
    shown vectors out, and only when ``CAM_ENABLE`` is set copies the
    host-published vectors in and clears the camera-clip flag at block+0x60 so
    the clip pass cannot pull the override back to the AC.  ``CAM_FRAME`` ticks
    every invocation for the disable -> two-frame wait -> publish -> enable
    handshake.
    """
    shown_eye = 0x20
    shown_target = 0x30
    c = Cave(base)
    c.stwu(1, 1, -0x30)
    c.std(0, 1, 0x10)
    c.std(11, 1, 0x18)
    c.std(12, 1, 0x20)
    c.mfcr(0)
    c.stw(0, 1, 0x28)

    c.lis(11, ring_base >> 16)
    c.ori(11, 11, ring_base & 0xFFFF)
    c.lwz(12, 11, int(offsets["magic"]))
    c.lis(0, 0x4158)
    c.ori(0, 0, 0x5052)
    c.cmpw(12, 0)
    c.bne("exit")

    c.lwz(12, 11, int(offsets["cam_frame"]))
    c.addi(12, 12, 1)
    c.stw(12, 11, int(offsets["cam_frame"]))
    c.stw(31, 11, int(offsets["cam_ptr"]))

    c.lwz(12, 11, int(offsets["cam_enable"]))
    c.cmpwi(12, 0)
    c.beq("readback")
    for hdr_off, cam_off in ((int(offsets["cam_eye"]), shown_eye),
                             (int(offsets["cam_eye"]) + 4, shown_eye + 4),
                             (int(offsets["cam_eye"]) + 8, shown_eye + 8),
                             (int(offsets["cam_target"]), shown_target),
                             (int(offsets["cam_target"]) + 4, shown_target + 4),
                             (int(offsets["cam_target"]) + 8, shown_target + 8)):
        c.lwz(12, 11, hdr_off)
        c.stw(12, 31, cam_off)
    c.li(0, 0)
    c.stw(0, 30, 0x60)         # block+0x60: skip CamFx camera clip this frame
    c.sync()

    c.mark("readback")
    for cam_off, hdr_off in ((shown_eye, int(offsets["cam_read_eye"])),
                             (shown_eye + 4, int(offsets["cam_read_eye"]) + 4),
                             (shown_eye + 8, int(offsets["cam_read_eye"]) + 8),
                             (shown_target, int(offsets["cam_read_target"])),
                             (shown_target + 4, int(offsets["cam_read_target"]) + 4),
                             (shown_target + 8, int(offsets["cam_read_target"]) + 8)):
        c.lwz(12, 31, cam_off)
        c.stw(12, 11, hdr_off)
    c.sync()

    c.mark("exit")
    c.lwz(0, 1, 0x28)
    c.mtcrf(0xFF, 0)
    c.ld(0, 1, 0x10)
    c.ld(11, 1, 0x18)
    c.ld(12, 1, 0x20)
    c.addi(1, 1, 0x30)
    c.addi(6, 31, shown_eye)   # original hooked instruction
    c.b(next_addr)
    return c.finish()


def vec_sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def vec_norm(v):
    length = math.sqrt(sum(c * c for c in v))
    if length < 1e-6:
        return None
    return (v[0] / length, v[1] / length, v[2] / length)


def vec_dist(a, b) -> float:
    return math.sqrt(sum((a[i] - b[i]) ** 2 for i in range(3)))
