#!/usr/bin/env python3
"""Map-path warp cave.

The AC Test mission XML (``ch6000.xml``) is loaded with the layer that the AC
Test menu expects and points at ``m4000``.  For an arbitrary map we keep the AC
Test mission/layer and rewrite the *formatted path strings* instead:

    .../m4000/m4000_actest.msb  ->  .../m<id>/m<id>.msb
    .../m4000/m4000_map.msb     ->  .../m<id>/m<id>_map.msb
    .../ch_env/m4000_env.msb    ->  .../ch_env/m<id>_env.msb

The rewrite runs at the return edge of the shared string formatter
(``0x82B405A0`` / ``blr`` at ``0x82B40614``), where ``r3`` is the string object:
``[r3+4]`` is the character buffer and ``[r3+8]`` its capacity.  The strings are
NUL-terminated C strings; the ``_actest.msb`` replacement therefore shortens
and terminates the string.

The cave is gated by ``AXPR`` + ``WARP_MODE`` in the ring header.  Mode 1
replaces ``_actest.msb`` with ``.msb``; mode 2 replaces it with ``_map.msb``.
The source token ``m4000`` is replaced only when followed by ``/`` or by one of
``_a``/``_m``/``_e`` so the ``m4000_Mtd.bnd`` material path is left alone.
"""
from __future__ import annotations

from ppcgen import Cave

SOURCE = b"m4000"


def _check_target(target: str) -> bytes:
    data = target.encode("ascii")
    if len(data) != len(SOURCE):
        raise ValueError(f"map token must be 5 bytes, got {target!r}")
    if not data.startswith(b"m"):
        raise ValueError(f"map token must start with 'm': {target!r}")
    return data


def build_cave(base: int, ring_base: int, target: str, mode: int,
               offsets: dict | None = None) -> list[int]:
    """Emit the mapwarp cave; ``mode`` is 1 (.msb) or 2 (_map.msb).

    The hook replaces the formatter's final ``blr``, so the cave is a leaf:
    ``r3`` is the return string object and LR is already the caller's return
    address.  Volatile GPRs and CR belong to the caller and may be clobbered,
    so no frame or register saves are needed.  The header counters at
    ``warp_calls``/``warp_map``/``warp_msb`` are the live evidence that the
    cave ran and which rewrite fired.
    """
    if mode not in (1, 2):
        raise ValueError("mapwarp mode must be 1 or 2")
    if offsets is None:
        offsets = {"magic": 0x08, "warp_mode": 0x5C,
                   "warp_calls": 0x60, "warp_map": 0x64, "warp_msb": 0x68}
    target_bytes = _check_target(target)
    c = Cave(base)

    # Ring header: AXPR magic + enabled warp mode.
    c.lis(11, ring_base >> 16)
    c.ori(11, 11, ring_base & 0xFFFF)
    c.lwz(12, 11, int(offsets["magic"]))
    c.lis(0, 0x4158)
    c.ori(0, 0, 0x5052)
    c.cmpw(12, 0)
    c.bne("exit")
    c.lwz(8, 11, int(offsets["warp_mode"]))
    c.cmpwi(8, 1)
    c.blt("exit")
    c.cmpwi(8, 2)
    c.bgt("exit")

    c.lwz(12, 11, int(offsets["warp_calls"]))
    c.addi(12, 12, 1)
    c.stw(12, 11, int(offsets["warp_calls"]))

    # String buffer and end bound.  At the return edge r3 is the string
    # object; [r3+4] is the destination buffer and [r3+8] its capacity.  The
    # formatter's r5 no longer holds the source pointer (memcpy clobbered it),
    # so scan the object's own buffer.
    c.lwz(5, 3, 4)
    c.cmpwi(5, 0)
    c.beq("exit")
    c.lwz(12, 3, 8)
    c.add(7, 5, 12)
    c.addi(7, 7, -1)

    c.mark("loop")
    c.cmpw(5, 7)
    c.bge("exit")
    c.lbz(6, 5, 0)
    c.cmpwi(6, 0)
    c.beq("exit")

    # -- m4000 -> m<id> (equal length) --------------------------------
    c.lwz(4, 5, 0)
    c.lis(0, 0x6D34)
    c.ori(0, 0, 0x3030)        # 'm400'
    c.cmpw(4, 0)
    c.bne("try_actest")
    c.lbz(6, 5, 4)
    c.cmpwi(6, 0x30)           # '0'
    c.bne("try_actest")
    c.lbz(6, 5, 5)
    c.cmpwi(6, ord("/"))
    c.beq("replace_m4000")
    c.cmpwi(6, ord("_"))
    c.bne("try_actest")
    c.lbz(6, 5, 6)
    for ch in "ame":
        c.cmpwi(6, ord(ch))
        c.beq("replace_m4000")
    c.b("try_actest")

    c.mark("replace_m4000")
    for offset, value in enumerate(target_bytes):
        c.li(0, value)
        c.stb(0, 5, offset)
    c.lwz(12, 11, int(offsets["warp_map"]))
    c.addi(12, 12, 1)
    c.stw(12, 11, int(offsets["warp_map"]))
    # Fall through: the rewritten bytes cannot match _act, so try_actest
    # advances to the next byte without a branch.

    # -- _actest.msb -> .msb / _map.msb ------------------------------
    c.mark("try_actest")
    c.lwz(4, 5, 0)
    c.lis(0, 0x5F61)
    c.ori(0, 0, 0x6374)        # '_act'
    c.cmpw(4, 0)
    c.bne("next")
    c.lwz(4, 5, 4)
    c.lis(0, 0x6573)
    c.ori(0, 0, 0x742E)        # 'est.'
    c.cmpw(4, 0)
    c.bne("next")
    c.lwz(4, 5, 8)
    c.lis(0, 0x6D73)
    c.ori(0, 0, 0x6200)        # 'msb\0'
    c.cmpw(4, 0)
    c.bne("next")

    c.lwz(12, 11, int(offsets["warp_msb"]))
    c.addi(12, 12, 1)
    c.stw(12, 11, int(offsets["warp_msb"]))

    c.cmpwi(8, 1)
    c.bne("mode_map")
    c.li(0, ord("."))
    c.stb(0, 5, 0)
    c.li(0, ord("m"))
    c.stb(0, 5, 1)
    c.li(0, ord("s"))
    c.stb(0, 5, 2)
    c.li(0, ord("b"))
    c.stb(0, 5, 3)
    c.li(0, 0)
    c.stb(0, 5, 4)
    c.b("exit")

    c.mark("mode_map")
    c.li(0, ord("_"))
    c.stb(0, 5, 0)
    c.li(0, ord("m"))
    c.stb(0, 5, 1)
    c.li(0, ord("a"))
    c.stb(0, 5, 2)
    c.li(0, ord("p"))
    c.stb(0, 5, 3)
    c.li(0, ord("."))
    c.stb(0, 5, 4)
    c.li(0, ord("m"))
    c.stb(0, 5, 5)
    c.li(0, ord("s"))
    c.stb(0, 5, 6)
    c.li(0, ord("b"))
    c.stb(0, 5, 7)
    c.li(0, 0)
    c.stb(0, 5, 8)
    c.b("exit")

    c.mark("next")
    c.addi(5, 5, 1)
    c.b("loop")

    c.mark("exit")
    c.blr()
    return c.finish()


if __name__ == "__main__":
    words = build_cave(0x8260E700, 0x83803400, "m3120", 1)
    print(f"{len(words)} words / 0x{len(words) * 4:X} bytes")
