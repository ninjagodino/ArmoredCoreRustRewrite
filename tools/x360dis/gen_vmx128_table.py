#!/usr/bin/env python3
"""Generate the Xbox 360 (Xenon) VMX128 decode table.

Input: external/xenia-canary/tools/ppc-instructions.xml (Xenia, BSD-2-Clause).
Output: tools/x360dis/vmx128.txt, a line-oriented table shared by the Python
disassembler and the Ghidra annotator (tools/ghidra/X360Vmx128.java).

The table reproduces Xenia's LookupOpcode() ordering exactly: instructions are
grouped per primary opcode by the bit ranges that identify the extended opcode,
groups are visited in sorted-expression order, and the first group whose
extracted key matches a case wins. That matters because primary opcodes 4/5/6
mix Altivec (VX/VA), VMX128 and a few scalar forms, and the shorter-span VMX128
groups must not pre-empt the full-width Altivec groups.

Encoding reference: http://biallas.net/doc/vmx128/vmx128.txt
Register-extension layout: Xenia src/xenia/cpu/ppc/ppc_decode_data.h
FormatVX128* structs (VMX128 addresses v0-v127 by stealing bits from the
Altivec XO/UIM field).

Usage:
  py tools/x360dis/gen_vmx128_table.py            # regenerate vmx128.txt
  py tools/x360dis/gen_vmx128_table.py --check    # verify table matches XML
"""
import argparse
import os
import sys
from xml.etree import ElementTree

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_XML = os.path.join(HERE, "..", "..", "external", "xenia-canary", "tools",
                           "ppc-instructions.xml")
OUT = os.path.join(HERE, "vmx128.txt")

# Mirrors extended_opcode_bits in external/xenia-canary/tools/ppc-table-gen.py.
# Ranges are MSB-first bit numbers (PPC numbering, bit 0 = MSB).
EXT_BITS = {
    "VX128": [(22, 25), (27, 27)],
    "VX128_1": [(21, 27), (30, 31)],
    "VX128_2": [(22, 22), (27, 27)],
    "VX128_3": [(21, 27)],
    "VX128_4": [(21, 23), (26, 27)],
    "VX128_5": [(27, 27)],
    "VX128_R": [(22, 24), (27, 27)],
    "VX128_P": [(21, 22), (26, 27)],
    "VX": [(21, 31)],
    "VA": [(26, 31)],
    "VC": [(22, 31)],
}
PRIMARIES = (4, 5, 6)

# Xenia's XML marks vpkd3d128 TODO (no <disasm> text). Operand order follows
# InstrEmit_vpkd3d128 (ppc_emit_altivec.cc): vD, vB, then the 5-bit IMM field
# (type >> 2, pack & 3) and the 2-bit z field.
TEMPLATE_OVERRIDE = {
    "vpkd3d128": "vpkd3d128 [VD], [VB], [IMM], [z]",
    "vupkd3d128": "vupkd3d128 [VD], [VB], [IMM]",
}


def bit_extract(word, left, right):
    """PPC/MSB-first extract, same as Xenia's ExtractBits."""
    return (word >> (32 - 1 - right)) & ((1 << (right - left + 1)) - 1)


def ext_key(word, spans):
    shift = max(b for _, b in spans)
    value = 0
    for a, b in spans:
        value |= bit_extract(word, a, b) << (shift - b)
    return value


def span_mask(spans):
    mask = 0x3F << 26  # primary opcode field
    for a, b in spans:
        for bit in range(a, b + 1):
            mask |= 1 << (31 - bit)
    return mask


def span_text(spans):
    return ",".join("%d-%d" % (a, b) for a, b in spans)


def load(xml_path):
    root = ElementTree.parse(xml_path).getroot()
    insns = []
    for e in root.findall(".//insn"):
        form = e.get("form")
        primary = int(e.get("opcode"), 16) >> 26
        if primary not in PRIMARIES:
            continue
        disasm = None
        for d in e.findall(".//disasm"):
            disasm = (d.text or "").strip()
        insns.append({
            "mnem": e.get("mnem"),
            "opcode": int(e.get("opcode"), 16),
            "form": form,
            "primary": primary,
            "spans": EXT_BITS[form],
            "template": disasm,
        })
    return insns


def group_order(insns):
    """Return instructions ordered exactly like generate_lookup() emits them."""
    by_primary = {}
    for i in insns:
        by_primary.setdefault(i["primary"], []).append(i)
    ordered = []
    for primary in sorted(by_primary):
        groups = {}
        for i in by_primary[primary]:
            key = span_text(i["spans"])
            groups.setdefault(key, []).append(i)
        for key in sorted(groups):  # string sort, as in ppc-table-gen.py
            for i in sorted(groups[key], key=lambda x: x["key"]):
                ordered.append(i)
    return ordered


def render(insns):
    for i in insns:
        i["key"] = ext_key(i["opcode"], i["spans"])
        i["mask"] = span_mask(i["spans"])
        assert i["opcode"] & ~i["mask"] == 0, (
            "don't-care bits set outside mask: %s %08x mask %08x"
            % (i["mnem"], i["opcode"], i["mask"]))
        i["is_vmx128"] = i["form"].startswith("VX128")
        if not i["template"]:
            # Xenia prints a few Altivec ops through custom code (custom_disasm);
            # they only matter here as ordering barriers, so keep the mnemonic.
            i["template"] = TEMPLATE_OVERRIDE.get(i["mnem"], i["mnem"])
            assert not i["is_vmx128"] or i["mnem"] in TEMPLATE_OVERRIDE, (
                "missing disasm text for " + i["mnem"])
    insns = group_order(insns)
    lines = [
        "# Xbox 360 (Xenon) PPC decode table, primary opcodes 4/5/6.",
        "# Contains VMX128 plus the Altivec forms that share those primaries;",
        "# order is Xenia's LookupOpcode() group order, so the first matching",
        "# row is the correct decode. Lines starting with 'v' are VMX128.",
        "# columns: kind|primary|spans|key|mnemonic|form|template",
        "# Generated by tools/x360dis/gen_vmx128_table.py from",
        "# external/xenia-canary/tools/ppc-instructions.xml (Xenia, BSD-2-Clause).",
        "# Encoding reference: http://biallas.net/doc/vmx128/vmx128.txt",
    ]
    last = None
    for i in insns:
        spans = span_text(i["spans"])
        if spans != last:
            lines.append("# spans %s" % spans)
            last = spans
        kind = "v" if i["is_vmx128"] else "i"
        lines.append("%s|%d|%s|%s|%s|%s|%s" % (
            kind, i["primary"], spans, bin(i["key"]),
            i["mnem"], i["form"], i["template"]))
    return "\n".join(lines) + "\n"


def parse_table(path):
    rows = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            kind, primary, spans, key, mnem, form, template = line.split("|", 6)
            rows.append({
                "kind": kind,
                "primary": int(primary),
                "spans": [tuple(int(x) for x in s.split("-"))
                          for s in spans.split(",")],
                "key": int(key, 2),
                "mnem": mnem,
                "form": form,
                "template": template,
            })
    return rows


def check(rows, xml_path):
    """Every XML instruction must decode to itself through the committed table."""
    insns = load(xml_path)
    groups = {}
    for i in insns:
        i["key"] = ext_key(i["opcode"], i["spans"])
    for i in insns:
        groups.setdefault((i["primary"], span_text(i["spans"])), {})[i["key"]] = i

    by_primary = {}
    for r in rows:
        by_primary.setdefault(r["primary"], []).append(r)

    errors = 0
    for i in insns:
        primary = i["primary"]
        word = i["opcode"]
        hit = None
        seen = set()
        for r in by_primary.get(primary, []):
            spans_id = span_text(r["spans"])
            if spans_id in seen:
                continue
            seen.add(spans_id)
            if ext_key(word, r["spans"]) in groups[(primary, spans_id)]:
                # first group that matches decides
                key = ext_key(word, r["spans"])
                hit = groups[(primary, spans_id)][key]
                break
        if hit is None or hit["mnem"] != i["mnem"]:
            errors += 1
            print("MISMATCH %-14s %08x -> %s" % (
                i["mnem"], i["opcode"], hit["mnem"] if hit else "invalid"))
    return errors


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--xml", default=DEFAULT_XML)
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--out", default=OUT)
    args = ap.parse_args()
    if not os.path.exists(args.xml):
        sys.exit("missing %s (gitignored Xenia checkout needed to regenerate)" % args.xml)
    if args.check:
        rows = parse_table(args.out)
        errors = check(rows, args.xml)
        total = sum(1 for r in rows if r["kind"] == "v")
        print("checked %d rows (%d VMX128), %d error(s)" % (len(rows), total, errors))
        sys.exit(1 if errors else 0)
    insns = load(args.xml)
    text = render(insns)
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    vmx = sum(1 for i in insns if i["form"].startswith("VX128"))
    print("wrote %s: %d rows, %d VMX128" % (args.out, len(insns), vmx))


if __name__ == "__main__":
    main()
