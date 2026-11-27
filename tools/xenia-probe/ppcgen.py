#!/usr/bin/env python3
"""PowerPC (Xenon) big-endian instruction encoder for probe code caves.

Only the scalar forms the probe caves need are implemented.  Caves must stay
ABI-safe: 64-bit ``std``/``ld`` GPR save slots, CR save/restore, and a 0x160
frame that clears the PPC64 288-byte red zone.  Branches are label based and
resolved in :meth:`Cave.finish`; a branch whose displacement is outside the
architectural range raises :class:`AsmError` instead of emitting a bad word.

The generated words are meant to be decoded by ``tools/x360dis/x360dis.py
--words`` (scalar PPC through capstone, VMX128 through ppc_vmx128); this module
does not try to decode anything itself.
"""
from __future__ import annotations

import struct

BRANCH_RANGE = 0x02000000
COND_BRANCH_RANGE = 0x8000


class AsmError(ValueError):
    pass


def _u32(value: int) -> int:
    return value & 0xFFFFFFFF


def _signed(value: int, bits: int) -> int:
    value &= (1 << bits) - 1
    sign = 1 << (bits - 1)
    return value - (1 << bits) if value & sign else value


def _check_s16(value: int, what: str) -> int:
    if not -0x8000 <= value <= 0x7FFF:
        raise AsmError(f"{what} displacement out of range: {value}")
    return value & 0xFFFF


class Cave:
    """A single contiguous block of generated PPC words with branch labels."""

    def __init__(self, base: int):
        self.base = _u32(base)
        self.words: list[int] = []
        self.labels: dict[str, int] = {}
        self.fixups: list[tuple[int, object, str]] = []

    # -- layout ---------------------------------------------------------
    @property
    def addr(self) -> int:
        return _u32(self.base + 4 * len(self.words))

    @property
    def size(self) -> int:
        return 4 * len(self.words)

    def emit(self, word: int) -> int:
        self.words.append(_u32(word))
        return len(self.words) - 1

    def mark(self, name: str) -> int:
        if name in self.labels:
            raise AsmError(f"duplicate label {name}")
        self.labels[name] = self.addr
        return self.addr

    def label_addr(self, target) -> int:
        if isinstance(target, str):
            if target not in self.labels:
                raise AsmError(f"unknown label {target}")
            return self.labels[target]
        return _u32(int(target))

    # -- branches -------------------------------------------------------
    def b(self, target, link: bool = False, absolute: bool = False) -> int:
        idx = self.emit(0x48000000 | (0x2 if absolute else 0) | (1 if link else 0))
        self.fixups.append((idx, target, "b"))
        return idx

    def bl(self, target) -> int:
        return self.b(target, link=True)

    def bc(self, bo: int, bi: int, target, link: bool = False) -> int:
        idx = self.emit(0x40000000 | ((bo & 0x1F) << 21) | ((bi & 0x1F) << 16)
                        | (1 if link else 0))
        self.fixups.append((idx, target, "bc"))
        return idx

    def beq(self, target) -> int:
        return self.bc(12, 2, target)          # CR0[EQ]

    def bne(self, target) -> int:
        return self.bc(4, 2, target)

    def bne_minus(self, target) -> int:
        return self.bc(5, 2, target)           # predicted-not-taken hint

    def blt(self, target) -> int:
        return self.bc(12, 0, target)

    def bge(self, target) -> int:
        return self.bc(4, 0, target)

    def bgt(self, target) -> int:
        return self.bc(12, 1, target)

    def ble(self, target) -> int:
        return self.bc(4, 1, target)

    def bdnz(self, target) -> int:
        return self.bc(16, 0, target)

    def blr(self) -> int:
        return self.emit(0x4E800020)

    def bctrl(self) -> int:
        return self.emit(0x4E800421)

    def sync(self) -> int:
        return self.emit(0x7C0004AC)

    def isync(self) -> int:
        return self.emit(0x4C00012C)

    def nop(self) -> int:
        return self.emit(0x60000000)

    # -- integer / CR ---------------------------------------------------
    def li(self, rd: int, imm: int) -> int:
        return self.addi(rd, 0, imm)

    def lis(self, rd: int, imm: int) -> int:
        # The 16-bit field is encoded raw (it sign-extends at runtime); 0x8380
        # is a normal high half, not an out-of-range signed immediate.
        return self.emit(0x3C000000 | ((rd & 0x1F) << 21) | (imm & 0xFFFF))

    def addi(self, rd: int, ra: int, imm: int) -> int:
        return self.emit(0x38000000 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(imm, "addi"))

    def addis(self, rd: int, ra: int, imm: int) -> int:
        return self.emit(0x3C000000 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(imm, "addis"))

    def add(self, rd: int, ra: int, rb: int) -> int:
        return self.emit(0x7C000214 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | ((rb & 0x1F) << 11))

    def ori(self, ra: int, rs: int, imm: int) -> int:
        return self.emit(0x60000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | (imm & 0xFFFF))

    def andi_dot(self, ra: int, rs: int, imm: int) -> int:
        return self.emit(0x70000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | (imm & 0xFFFF))

    def mulli(self, rd: int, ra: int, imm: int) -> int:
        return self.emit(0x1C000000 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(imm, "mulli"))

    def cmpw(self, ra: int, rb: int, crf: int = 0) -> int:
        return self.emit(0x7C000000 | ((crf & 7) << 23) | ((ra & 0x1F) << 16)
                         | ((rb & 0x1F) << 11))

    def cmplw(self, ra: int, rb: int, crf: int = 0) -> int:
        return self.emit(0x7C000040 | ((crf & 7) << 23) | ((ra & 0x1F) << 16)
                         | ((rb & 0x1F) << 11))

    def cmpwi(self, ra: int, imm: int, crf: int = 0) -> int:
        return self.emit(0x2C000000 | ((crf & 7) << 23) | ((ra & 0x1F) << 16)
                         | _check_s16(imm, "cmpwi"))

    def cmplwi(self, ra: int, imm: int, crf: int = 0) -> int:
        return self.emit(0x28000000 | ((crf & 7) << 23) | ((ra & 0x1F) << 16)
                         | (imm & 0xFFFF))

    def mfcr(self, rd: int) -> int:
        return self.emit(0x7C000026 | ((rd & 0x1F) << 21))

    def mtcrf(self, crm: int, rs: int) -> int:
        return self.emit(0x7C000120 | ((rs & 0x1F) << 21) | ((crm & 0xFF) << 12))

    def mflr(self, rd: int) -> int:
        return self.emit(0x7C0002A6 | ((rd & 0x1F) << 21) | 0x00080000)

    def mtlr(self, rs: int) -> int:
        return self.emit(0x7C0003A6 | ((rs & 0x1F) << 21) | 0x00080000)

    def mfctr(self, rd: int) -> int:
        return self.emit(0x7C0002A6 | ((rd & 0x1F) << 21) | 0x00090000)

    def mtctr(self, rs: int) -> int:
        return self.emit(0x7C0003A6 | ((rs & 0x1F) << 21) | 0x00090000)

    def rlwinm(self, ra: int, rs: int, sh: int, mb: int, me: int) -> int:
        return self.emit(0x54000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | ((sh & 0x1F) << 11) | ((mb & 0x1F) << 6)
                         | ((me & 0x1F) << 1))

    # -- memory ---------------------------------------------------------
    def extsb(self, ra: int, rs: int) -> int:
        return self.emit(0x7C000774 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16))

    def stwu(self, rs: int, ra: int, d: int) -> int:
        return self.emit(0x94000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "stwu"))

    def lhz(self, rd: int, ra: int, d: int) -> int:
        return self.emit(0xA0000000 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "lhz"))

    def sth(self, rs: int, ra: int, d: int) -> int:
        return self.emit(0xB0000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "sth"))

    def lwz(self, rd: int, ra: int, d: int) -> int:
        return self.emit(0x80000000 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "lwz"))

    def stw(self, rs: int, ra: int, d: int) -> int:
        return self.emit(0x90000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "stw"))

    def stfs(self, frs: int, ra: int, d: int) -> int:
        return self.emit(0xD0000000 | ((frs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "stfs"))

    def lbz(self, rd: int, ra: int, d: int) -> int:
        return self.emit(0x88000000 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "lbz"))

    def stb(self, rs: int, ra: int, d: int) -> int:
        return self.emit(0x98000000 | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, "stb"))

    def _ds(self, op: int, reg: int, ra: int, d: int, what: str) -> int:
        if d & 3:
            raise AsmError(f"{what} displacement must be 4-byte aligned: {d}")
        return self.emit(op | ((reg & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | _check_s16(d, what) & 0xFFFC)

    def ld(self, rd: int, ra: int, d: int) -> int:
        return self._ds(0xE8000000, rd, ra, d, "ld")

    def std(self, rs: int, ra: int, d: int) -> int:
        return self._ds(0xF8000000, rs, ra, d, "std")

    def lwarx(self, rd: int, ra: int, rb: int) -> int:
        return self.emit(0x7C000028 | ((rd & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | ((rb & 0x1F) << 11))

    def stwcx_dot(self, rs: int, ra: int, rb: int) -> int:
        return self.emit(0x7C00012D | ((rs & 0x1F) << 21) | ((ra & 0x1F) << 16)
                         | ((rb & 0x1F) << 11))

    # -- resolution -----------------------------------------------------
    def finish(self) -> list[int]:
        words = list(self.words)
        for index, target, kind in self.fixups:
            pc = _u32(self.base + index * 4)
            dest = self.label_addr(target)
            delta = _signed(dest - pc, 32)
            if kind == "b":
                if delta & 3:
                    raise AsmError(f"branch to 0x{dest:08X} from 0x{pc:08X} not aligned")
                if abs(delta) >= BRANCH_RANGE:
                    raise AsmError(f"branch 0x{pc:08X}->0x{dest:08X} "
                                   f"({delta:+#x}) outside {BRANCH_RANGE:#x}")
                words[index] = (words[index] & 0xFC000003) | (delta & 0x03FFFFFC)
            elif kind == "bc":
                if delta & 3:
                    raise AsmError(f"cond branch to 0x{dest:08X} from 0x{pc:08X} not aligned")
                if not -COND_BRANCH_RANGE <= delta < COND_BRANCH_RANGE:
                    raise AsmError(f"cond branch 0x{pc:08X}->0x{dest:08X} "
                                   f"({delta:+#x}) outside +/-{COND_BRANCH_RANGE:#x}")
                words[index] = (words[index] & 0xFFFF0003) | (delta & 0xFFFC)
            else:  # pragma: no cover
                raise AsmError(f"unknown fixup kind {kind}")
        return words

    def to_bytes(self, byteorder: str = "big") -> bytes:
        return b"".join(struct.pack(">I", w) for w in self.finish())


def words_to_hex(words: list[int]) -> str:
    """Raw hex string used by Xenia's ``array`` patch data type."""
    return "".join(f"{w & 0xFFFFFFFF:08x}" for w in words)


def words_to_bytes(words: list[int]) -> bytes:
    return b"".join(struct.pack(">I", w & 0xFFFFFFFF) for w in words)


def branch_word(src: int, dst: int, link: bool = False) -> int:
    """Encode a single ``b``/``bl`` and assert the architectural range."""
    delta = _signed(_u32(dst) - _u32(src), 32)
    if delta & 3:
        raise AsmError(f"branch 0x{src:08X}->0x{dst:08X} not aligned")
    if abs(delta) >= BRANCH_RANGE:
        raise AsmError(f"branch 0x{src:08X}->0x{dst:08X} "
                       f"({delta:+#x}) outside {BRANCH_RANGE:#x}")
    return _u32(0x48000000 | (1 if link else 0) | (delta & 0x03FFFFFC))


if __name__ == "__main__":  # tiny smoke check
    c = Cave(0x8260E290)
    c.stwu(1, 1, -0x160)
    c.mark("loop")
    c.addi(10, 10, -1)
    c.bne("loop")
    c.blr()
    for i, w in enumerate(c.finish()):
        print(f"{c.base + i * 4:08x}  {w:08x}")
