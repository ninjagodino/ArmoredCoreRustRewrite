"""Disassemble Xbox 360 PowerPC + VMX128 straight from the disc image.

The Xbox 360 build uses Xenon VMX128 (128 vector registers) which capstone and
Ghidra's PowerPC module do not decode: their register fields reuse bits of the
Altivec XO field. tools/x360dis/vmx128.txt carries that encoding (generated
from Xenia's ppc-instructions.xml, see gen_vmx128_table.py); every other
instruction is decoded by capstone.

Function bounds come from the image's .pdata table, so sweeps stay inside real
code and data tables are not mistaken for instructions.

Usage:
  py tools/x360dis/x360dis.py --func 0x8211cd40
  py tools/x360dis/x360dis.py --func 0x8211cd40 --refs
  py tools/x360dis/x360dis.py --range 0x8211cd40 0x8211ce00
  py tools/x360dis/x360dis.py --trace 0x8211cd40 --depth 2
  py tools/x360dis/x360dis.py --stats --func 0x8211cd40

--refs resolves lis/addi/ori constants within the block and prints the vector
constant a lv*/st* touches, as floats, which is usually the fastest way to see
what a matrix/shader-constant block is doing without a runtime probe.
"""
import argparse
import bisect
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ppc_vmx128  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
DEFAULT_DISC = os.path.join(ROOT, "private", "x360", "vd", "ACV2.pe")
BASE = 0x82000000
PDATA = 0x8225C200
PDATA_SIZE = 0x99A38


class Disc:
    def __init__(self, path):
        with open(path, "rb") as f:
            self.image = f.read()
        self.end = BASE + len(self.image)

    def contains(self, addr):
        return BASE <= addr < self.end

    def read(self, addr, size=4):
        if not self.contains(addr) or addr + size > self.end:
            return None
        off = addr - BASE
        return self.image[off:off + size]

    def word(self, addr):
        data = self.read(addr, 4)
        return None if data is None else struct.unpack(">I", data)[0]

    def floats(self, addr):
        data = self.read(addr, 16)
        return None if data is None else struct.unpack(">4f", data)


class PData:
    """Function boundaries from the 360 image's .pdata (see X360Pdata.java)."""

    def __init__(self, disc):
        self.bodies = []
        self.starts = []
        for off in range(0, PDATA_SIZE, 8):
            begin = disc.word(PDATA + off)
            words = (disc.word(PDATA + off + 4) or 0) >> 8 & 0x3FFFFF
            if not begin or not words:
                continue
            self.starts.append(begin)
            self.bodies.append((begin, begin + words * 4))
        order = sorted(range(len(self.starts)), key=lambda i: self.starts[i])
        self.starts = [self.starts[i] for i in order]
        self.bodies = [self.bodies[i] for i in order]

    def find(self, addr):
        i = bisect.bisect_right(self.starts, addr) - 1
        if i >= 0 and self.bodies[i][0] <= addr < self.bodies[i][1]:
            return self.bodies[i]
        return None

    def is_start(self, addr):
        i = bisect.bisect_left(self.starts, addr)
        return i < len(self.starts) and self.starts[i] == addr


def make_capstone():
    import capstone
    md = capstone.Cs(capstone.CS_ARCH_PPC,
                     capstone.CS_MODE_32 | capstone.CS_MODE_BIG_ENDIAN)
    md.detail = True
    return md


LOADSTORE = ("lv", "st")


def branch_target(insn):
    for op in insn.operands:
        if op.type == 2:  # PPC_OP_IMM
            return op.imm & 0xFFFFFFFF
    return None


def is_transfer(insn):
    return insn.mnemonic in ("b", "bl", "bc", "bcl", "blr", "bctr", "bctrl",
                             "bcctr", "bcctrl", "bclr", "bdnz", "bdz")


def track_constants(insn, known):
    """Update known register constants for one instruction.

    known maps register names ("r11") to (value, from_lis). from_lis gates
    vector-constant references: only addresses built from a lis/addi chain are
    trusted, not stack/linkage offsets materialised with li.
    """
    mnem = insn.mnemonic
    ops = insn.operands

    def rname(op):
        return insn.reg_name(op.reg)

    if mnem in ("lis", "addis") and len(ops) == 2:
        known[rname(ops[0])] = ((ops[1].imm & 0xFFFF) << 16, True)
        return
    if mnem in ("addi", "li") and len(ops) == 2:
        known[rname(ops[0])] = (ops[1].imm & 0xFFFFFFFF, False)
        return
    if mnem in ("addi", "addis", "ori", "oris") and len(ops) == 3:
        src = known.get(rname(ops[1]))
        if src is None:
            known.pop(rname(ops[0]), None)
            return
        imm = ops[2].imm
        if mnem in ("addis", "oris"):
            imm = (imm & 0xFFFF) << 16
        if mnem.startswith("add"):
            value = (src[0] + imm) & 0xFFFFFFFF
        else:
            value = (src[0] | (imm & 0xFFFF)) & 0xFFFFFFFF
        known[rname(ops[0])] = (value, src[1])
        return
    if mnem == "mr" and len(ops) == 2:
        src = known.get(rname(ops[1]))
        if src is not None:
            known[rname(ops[0])] = src
        return
    if mnem in ("bl", "bctrl"):
        for r in range(13):  # caller-saved
            known.pop("r%d" % r, None)
        return
    try:
        _, written = insn.regs_access()
    except Exception:
        written = []
    for reg in written:
        known.pop(insn.reg_name(reg), None)


def resolve_ea(fields, known):
    """EA for an indexed vector load/store: RA0 + RB (RA/RB 0 means zero)."""
    ra = fields.get("RA0", 0)
    rb = fields.get("RB", 0)
    base = (0, True) if ra == 0 else known.get("r%d" % ra)
    index = (0, True) if rb == 0 else known.get("r%d" % rb)
    if base is None or index is None or not (base[1] and index[1]):
        return None
    return (base[0] + index[0]) & 0xFFFFFFFF


def line(addr, word, text, comment=None, raw=False):
    prefix = "%08x  %08x  " % (addr, word) if raw else "%08x  " % addr
    if comment:
        return "%s%-44s ; %s" % (prefix, text, comment)
    return prefix + text


def disasm_words(words, base=0, raw=True, out=sys.stdout):
    """Decode a list of raw 32-bit words at `base` (cave self-test, no PE)."""
    md = make_capstone()
    for i, word in enumerate(words):
        addr = (base + i * 4) & 0xFFFFFFFF
        hit = ppc_vmx128.disasm(word)
        if hit:
            text = hit[1]
        else:
            insns = list(md.disasm(struct.pack(">I", word & 0xFFFFFFFF), addr))
            if insns:
                text = ("%-8s %s" % (insns[0].mnemonic, insns[0].op_str)).strip()
            else:
                text = ".word 0x%08x" % (word & 0xFFFFFFFF)
        out.write(line(addr, word & 0xFFFFFFFF, text, None, raw) + "\n")


def disasm_body(disc, md, lo, hi, refs=False, raw=False, out=sys.stdout):
    known = {}
    addr = lo
    while addr < hi:
        word = disc.word(addr)
        if word is None:
            break
        hit = ppc_vmx128.disasm(word)
        comment = None
        if hit:
            _, text, row = hit
            if refs and row["mnem"].startswith(LOADSTORE):
                ea = resolve_ea(ppc_vmx128.fields(word, row["form"]), known)
                comment = vector_comment(disc, ea) if ea is not None else None
            out.write(line(addr, word, text, comment, raw) + "\n")
            addr += 4
            continue
        insns = list(md.disasm(disc.read(addr, 4), addr))
        if not insns:
            out.write(line(addr, word, ".word 0x%08x" % word, None, raw) + "\n")
            addr += 4
            continue
        insn = insns[0]
        if refs:
            track_constants(insn, known)
        text = ("%-8s %s" % (insn.mnemonic, insn.op_str)).strip()
        if refs and is_transfer(insn):
            target = branch_target(insn)
            if target:
                comment = "-> %08x" % target
        out.write(line(addr, word, text, comment, raw) + "\n")
        addr += 4


def vector_comment(disc, ea):
    data = disc.read(ea, 16)
    if data is None:
        return "ea %08x (outside image)" % ea
    f = struct.unpack(">4f", data)
    return "%08x f32[%s]" % (ea, ", ".join("%.6g" % x for x in f))


def trace(disc, pdata, md, entry, depth, max_insns, refs, raw, out):
    seen = set()
    count = 0
    work = [(entry, 0)]
    while work and count < max_insns:
        addr, level = work.pop(0)
        body = pdata.find(addr)
        if body is None or addr in seen:
            continue
        seen.add(addr)
        out.write("%s; === %08x-%08x depth %d%s\n" % (
            "    " * level, body[0], body[1], level,
            "" if body[0] == addr else " (from %08x)" % addr))
        lo, hi = body
        known = {}
        a = lo
        while a < hi and count < max_insns:
            word = disc.word(a)
            if word is None:
                break
            count += 1
            hit = ppc_vmx128.disasm(word)
            if hit:
                _, text, row = hit
                out.write("%s%s\n" % ("    " * level, line(a, word, text, None, raw)))
                a += 4
                continue
            insns = list(md.disasm(disc.read(a, 4), a))
            if not insns:
                out.write("%s%s\n" % ("    " * level, line(a, word, ".word 0x%08x" % word, None, raw)))
                a += 4
                continue
            insn = insns[0]
            text = ("%-8s %s" % (insn.mnemonic, insn.op_str)).strip()
            out.write("%s%s\n" % ("    " * level, line(a, word, text, None, raw)))
            if insn.mnemonic in ("bl", "bctrl"):
                target = branch_target(insn)
                if target and level + 1 <= depth and target not in seen:
                    work.append((target, level + 1))
            a += 4
        out.write("\n")


def stats(disc, pdata, md, body):
    import collections
    lo, hi = body
    counts = collections.Counter()
    decode_fail = 0
    reg_max = 0
    for a in range(lo, hi, 4):
        word = disc.word(a)
        if word is None:
            continue
        hit = ppc_vmx128.disasm(word)
        if hit:
            counts[hit[0]] += 1
            f = ppc_vmx128.fields(word, hit[2]["form"])
            for k in ("VD", "VA", "VB", "VC"):
                if k in f:
                    reg_max = max(reg_max, f[k])
            continue
        if not list(md.disasm(disc.read(a, 4), a)):
            decode_fail += 1
            continue
        insns = list(md.disasm(disc.read(a, 4), a))
        counts[insns[0].mnemonic] += 1
    total = sum(counts.values())
    print("%08x-%08x: %d instructions, %d undecoded, max vector reg v%d"
          % (lo, hi, total, decode_fail, reg_max))
    for mnem, n in counts.most_common(40):
        print("  %-18s %5d" % (mnem, n))


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--disc", default=DEFAULT_DISC)
    ap.add_argument("--func", type=lambda x: int(x, 16),
                    help="function containing this address")
    ap.add_argument("--range", nargs=2, type=lambda x: int(x, 16),
                    metavar=("LO", "HI"))
    ap.add_argument("--trace", type=lambda x: int(x, 16),
                    help="entry address; linear sweep + recurse into bl targets")
    ap.add_argument("--depth", type=int, default=1)
    ap.add_argument("--max", type=int, default=20000)
    ap.add_argument("--refs", action="store_true",
                    help="resolve vector constant addresses (lis/addi tracking)")
    ap.add_argument("--raw", action="store_true", help="show the raw word")
    ap.add_argument("--stats", action="store_true")
    ap.add_argument("--words", nargs="+", type=lambda x: int(x, 0),
                    help="decode raw 32-bit words (hex, e.g. 0x7d8802a6)")
    ap.add_argument("--base", type=lambda x: int(x, 0), default=0,
                    help="address of the first --words word (default 0)")
    args = ap.parse_args()

    if args.words:
        disasm_words(args.words, args.base, True)
        return

    disc = Disc(args.disc)
    md = make_capstone()
    pdata = None

    if args.stats or args.func or args.trace:
        pdata = PData(disc)

    if args.trace:
        trace(disc, pdata, md, args.trace, args.depth, args.max, args.refs,
              args.raw, sys.stdout)
        return
    if args.stats:
        if not args.func:
            sys.exit("--stats needs --func")
        body = pdata.find(args.func)
        if body is None:
            sys.exit("%08x is not inside a .pdata function" % args.func)
        stats(disc, pdata, md, body)
        return
    if args.func:
        body = pdata.find(args.func)
        if body is None:
            sys.exit("%08x is not inside a .pdata function" % args.func)
        disasm_body(disc, md, body[0], body[1], args.refs, args.raw)
        return
    if args.range:
        lo, hi = args.range
        disasm_body(disc, md, lo, hi, args.refs, args.raw)
        return
    ap.print_help()


if __name__ == "__main__":
    main()
