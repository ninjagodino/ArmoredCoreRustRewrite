"""Xbox 360 (Xenon) VMX128 decoding, shared by tools/x360dis/x360dis.py.

The table (vmx128.txt, next to this file) is generated from Xenia's
ppc-instructions.xml by gen_vmx128_table.py. It also carries the Altivec forms
that share primary opcodes 4/5/6, because Xenia's LookupOpcode() resolves those
primaries group by group: the first group whose extracted key matches wins.

VMX128 reaches v0-v127 by stealing bits from the Altivec XO field; the layouts
below mirror the FormatVX128* structs in Xenia's ppc_decode_data.h. Bit numbers
are LSB-based (bit 0 = least significant bit of the big-endian word).

Usage:
  from ppc_vmx128 import decode, format_insn
  i = decode(0x14000010)   # {'mnem': 'vaddfp128', ...}
  print(format_insn(i))    # 'vaddfp128 vr0, vr0, vr0'
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "vmx128.txt")

_regs = None
_groups = None


def _load(path=TABLE):
    global _regs, _groups
    rows = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            kind, primary, spans, key, mnem, form, template = line.split("|", 6)
            rows.append({
                "vmx128": kind == "v",
                "primary": int(primary),
                "spans": [tuple(int(x) for x in s.split("-"))
                          for s in spans.split(",")],
                "key": int(key, 2),
                "mnem": mnem,
                "form": form,
                "template": template,
            })
    # Preserve file order; a new span set starts a new resolution group.
    groups = {}
    seen = {}
    for r in rows:
        spans = tuple(r["spans"])
        gid = (r["primary"], spans)
        if gid not in seen:
            seen[gid] = len(groups.setdefault(r["primary"], []))
            groups[r["primary"]].append({"spans": spans, "by_key": {}})
        groups[r["primary"]][seen[gid]]["by_key"][r["key"]] = r
    _groups = groups


def _bits(word, lo, hi):
    return (word >> lo) & ((1 << (hi - lo + 1)) - 1)


def _key(word, spans):
    top = max(b for _, b in spans)
    value = 0
    for a, b in spans:
        # table spans are MSB-first (PPC numbering); convert to LSB shift
        shift = top - b
        value |= _bits(word, 31 - b, 31 - a) << shift
    return value


def _sx(value, bits):
    sign = 1 << (bits - 1)
    return (value ^ sign) - sign


def decode(word):
    """Decode one big-endian instruction word. Returns None when no row matches."""
    if _groups is None:
        _load()
    primary = word >> 26
    for group in _groups.get(primary, ()):
        row = group["by_key"].get(_key(word, group["spans"]))
        if row is not None:
            return row
    return None


def fields(word, form):
    """Operand field values for a VMX128 form (see ppc_decode_data.h)."""
    vd = _bits(word, 21, 25) | (_bits(word, 2, 3) << 5)
    va = (_bits(word, 16, 20) | (_bits(word, 5, 5) << 5)
          | (_bits(word, 10, 10) << 6))
    vb = _bits(word, 11, 15) | (_bits(word, 0, 1) << 5)
    ra = _bits(word, 16, 20)
    rb = _bits(word, 11, 15)
    if form == "VX128":
        return {"VD": vd, "VA": va, "VB": vb, "VC": _bits(word, 6, 8)}
    if form == "VX128_1":
        # stores print [VS], which is the same field as VD
        return {"VD": vd, "VS": vd, "RA0": ra, "RB": rb}
    if form == "VX128_2":
        return {"VD": vd, "VA": va, "VB": vb, "VC": _bits(word, 6, 8)}
    if form == "VX128_3":
        return {"VD": vd, "VB": vb, "UIMM": _bits(word, 16, 20),
                "IMM": _bits(word, 16, 20),
                # vspltisw128's 5-bit immediate is signed in the ISA; Xenia's
                # XEEXTS16() does not sign-extend it, so we do it here.
                "SIMM": _sx(_bits(word, 16, 20), 5)}
    if form == "VX128_4":
        return {"VD": vd, "VB": vb, "IMM": _bits(word, 16, 20),
                "z": _bits(word, 6, 7)}
    if form == "VX128_5":
        return {"VD": vd, "VA": va, "VB": vb, "SHB": _bits(word, 6, 9)}
    if form == "VX128_R":
        return {"VD": vd, "VA": va, "VB": vb, "Rc": _bits(word, 6, 6)}
    if form == "VX128_P":
        return {"VD": vd, "VB": vb,
                "UIMM": _bits(word, 16, 20) | (_bits(word, 6, 8) << 5)}
    return {}


def _operand(name, value):
    if name in ("VD", "VS", "VA", "VB", "VC"):
        return "vr%d" % value
    if name in ("RA0", "RB"):
        return "r%d" % value
    if name == "UIMM":
        return "0x%X" % value
    return str(value)


def format_insn(row, word):
    """Render the instruction text from its template and field values."""
    values = fields(word, row["form"])
    out = []
    token = None
    for ch in row["template"]:
        if ch == "[":
            token = ""
        elif ch == "]" and token is not None:
            if token == "Rc":
                out.append("." if values.get("Rc") else "")
            else:
                out.append(_operand(token, values[token]))
            token = None
        elif token is not None:
            token += ch
        else:
            out.append(ch)
    return "".join(out)


def disasm(word):
    """Return (mnemonic, text, row) for a VMX128 word, or None."""
    row = decode(word)
    if row is None or not row["vmx128"]:
        return None
    return row["mnem"], format_insn(row, word), row


def _word_for(row):
    """Synthesise a word that carries this row's key (free bits zero)."""
    spans = row["spans"]
    top = max(b for _, b in spans)
    word = row["primary"] << 26
    for a, b in spans:
        width = b - a + 1
        part = (row["key"] >> (top - b)) & ((1 << width) - 1)
        word |= part << (31 - b)
    return word


def selftest():
    """Every row must round-trip: synthesised word decodes back to that row and
    every operand in its template must be extractable for its form."""
    _load()
    seen = set()
    for groups in _groups.values():
        for group in groups:
            for row in group["by_key"].values():
                if not row["vmx128"] or row["mnem"] in seen:
                    continue
                seen.add(row["mnem"])
                word = _word_for(row)
                got = decode(word)
                if got is None or got["mnem"] != row["mnem"]:
                    raise SystemExit("round-trip failed for %s (%08x -> %s)"
                                     % (row["mnem"], word, got and got["mnem"]))
                try:
                    format_insn(row, word)
                except KeyError as e:
                    raise SystemExit("template token %s missing for %s (%s): %s"
                                     % (e, row["mnem"], row["form"], row["template"]))
    print("selftest: %d instructions round-trip" % len(seen))


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        selftest()
        raise SystemExit(0)
    for arg in sys.argv[1:]:
        w = int(arg, 16)
        r = decode(w)
        print("%08X  %s" % (w, (format_insn(r, w) if r else "<invalid>")))

