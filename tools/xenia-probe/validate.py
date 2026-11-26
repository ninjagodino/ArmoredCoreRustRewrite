#!/usr/bin/env python3
"""Static validation of the probe caves, hooks and FFX ring.

Phase 0 established that the cave ranges are *not* zero padding: they are the
bodies of uncalled functions ``0x8260E290..0x8260E700`` and
``0x8260E700..0x8260E97C``.  The verifier therefore checks what actually
matters: executable section, no indexed callers/references, hook words, cave
size and branch range, and for the ring a writable zero/no-ref range.

Index scans are optional (``scan_index=False``) so unit tests do not have to
read the 100 MB ``private/index`` CSVs.
"""
from __future__ import annotations

import re
import struct
from dataclasses import dataclass
from pathlib import Path

IMAGE_SCN_MEM_EXECUTE = 0x20000000
IMAGE_SCN_MEM_WRITE = 0x80000000
ADDR_RE = re.compile(r"0x[0-9A-Fa-f]{8}")


class ValidationError(RuntimeError):
    pass


@dataclass
class Section:
    name: str
    va: int          # guest virtual address
    vsize: int
    raw_size: int
    raw_ptr: int
    flags: int

    @property
    def end(self) -> int:
        return self.va + self.vsize

    @property
    def executable(self) -> bool:
        return bool(self.flags & IMAGE_SCN_MEM_EXECUTE)

    @property
    def writable(self) -> bool:
        return bool(self.flags & IMAGE_SCN_MEM_WRITE)


class PE:
    def __init__(self, path):
        self.path = Path(path)
        self.data = self.path.read_bytes()
        if self.data[:2] != b"MZ":
            raise ValidationError(f"{self.path} is not an MZ image")
        e_lfanew = struct.unpack_from("<I", self.data, 0x3C)[0]
        if self.data[e_lfanew:e_lfanew + 4] != b"PE\0\0":
            raise ValidationError(f"{self.path} has no PE signature")
        coff = e_lfanew + 4
        self.machine = struct.unpack_from("<H", self.data, coff)[0]
        nsections = struct.unpack_from("<H", self.data, coff + 2)[0]
        opt_size = struct.unpack_from("<H", self.data, coff + 16)[0]
        opt = coff + 20
        magic = struct.unpack_from("<H", self.data, opt)[0]
        if magic == 0x20B:
            self.image_base = struct.unpack_from("<Q", self.data, opt + 24)[0]
        else:
            self.image_base = struct.unpack_from("<I", self.data, opt + 28)[0]
        self.size_of_image = struct.unpack_from("<I", self.data, opt + 56)[0]
        self.file_size = len(self.data)
        section_table = opt + opt_size
        self.sections: list[Section] = []
        for i in range(nsections):
            off = section_table + i * 40
            name = self.data[off:off + 8].rstrip(b"\0").decode("ascii", "replace")
            vsize, va, raw_size, raw_ptr = struct.unpack_from("<IIII", self.data, off + 8)
            flags = struct.unpack_from("<I", self.data, off + 36)[0]
            self.sections.append(Section(name, self.image_base + va, vsize,
                                         raw_size, raw_ptr, flags))

    def section_for(self, va: int) -> Section | None:
        for section in self.sections:
            if section.va <= va < section.end:
                return section
        return None

    def contains(self, va: int, size: int = 4) -> bool:
        return any(s.va <= va and va + size <= s.end for s in self.sections)

    def read(self, va: int, size: int) -> bytes | None:
        # ACV2.pe is a flat memory image: file offset = guest VA - image base.
        # The section table's PointerToRawData has a header/section gap and
        # must not be used for byte reads; x360dis and Phase 0 use the same
        # flat mapping.
        section = self.section_for(va)
        if section is None or va + size > section.end:
            return None
        offset = va - self.image_base
        if offset < 0 or offset + size > len(self.data):
            return None
        return self.data[offset:offset + size]

    def zero_state(self, va: int, size: int) -> tuple[bool, str]:
        """Return (is_zero, reason) for a writable data range.

        The flat image carries the zero-filled memory tail, so inspect the
        actual bytes.  If a range runs past the end of the file but is still
        inside a section's virtual size, the loader zero-fills it.
        """
        section = self.section_for(va)
        if section is None or va + size > section.end:
            return False, "outside image sections"
        offset = va - self.image_base
        if offset < 0:
            return False, "before image base"
        if offset + size > len(self.data):
            return True, "virtual-zero tail"
        data = self.data[offset:offset + size]
        return all(b == 0 for b in data), "flat image bytes"

    def word(self, va: int) -> int | None:
        data = self.read(va, 4)
        return None if data is None else struct.unpack(">I", data)[0]


# Only semantic *reference* columns count.  The cave bases happen to be
# function starts, so scanning every address cell in calls.csv would count the
# uncalled function's own outgoing calls as "references to the cave".
REF_COLUMNS = {
    "calls.csv": ("callee",),
    "slots.csv": ("target",),
    "consts.csv": ("target",),
    "switch_cases.csv": ("target",),
}


def scan_index(index_dir, spans: dict[str, tuple[int, int]],
               ignore: set[str] | None = None) -> dict[str, list[str]]:
    """Find indexed references whose target falls inside a span.

    Only reference columns are inspected (``calls.callee``, ``slots.target``,
    ``consts.target``, ``switch_cases.target``).  ``functions.csv`` definitions,
    fields offsets, immediates and caller/site columns are not references.
    """
    ignore = {name.lower() for name in (ignore or set())}
    hits = {name: [] for name in spans}
    pending = set(spans)
    root = Path(index_dir)
    if not root.is_dir():
        raise ValidationError(f"missing index directory: {root}")
    for filename, columns in REF_COLUMNS.items():
        path = root / filename
        if not path.exists() or filename.lower() in ignore:
            continue
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            header = fh.readline().rstrip("\n").split(",")
            indices = [header.index(col) for col in columns if col in header]
            if not indices:
                continue
            for line in fh:
                cells = line.rstrip("\n").split(",")
                for index in indices:
                    if index >= len(cells):
                        continue
                    cell = cells[index]
                    if len(cell) != 10 or not cell.startswith("0x"):
                        continue
                    addr = int(cell, 16)
                    for name, (lo, hi) in spans.items():
                        if name in pending and lo <= addr < hi:
                            hits[name].append(f"{filename}: {line.strip()}")
                            pending.discard(name)
                if not pending:
                    return hits
    return hits


def _branch_range_ok(src: int, dst: int, kind: str = "b") -> bool:
    delta = dst - src
    if delta & 3:
        return False
    if kind == "b":
        return abs(delta) < 0x02000000
    return -0x8000 <= delta < 0x8000


def validate(profile: dict, root, *, scan_index_refs: bool = True) -> dict:
    root = Path(root)
    paths = profile["paths"]
    pe_path = root / paths["pe"]
    if not pe_path.exists():
        raise ValidationError(f"missing PE: {pe_path}")
    pe = PE(pe_path)
    errors: list[str] = []
    warnings: list[str] = []

    if pe.image_base != int(profile["image_base"]):
        errors.append(f"image base 0x{pe.image_base:X} != profile 0x{int(profile['image_base']):X}")
    if pe.file_size != int(profile["image_size"]):
        errors.append(f"file size 0x{pe.file_size:X} != profile 0x{int(profile['image_size']):X}")

    hooks = {}
    for name, hook in profile["hooks"].items():
        addr = int(hook["addr"])
        expected = int(hook["word"])
        word = pe.word(addr)
        section = pe.section_for(addr)
        hooks[name] = {
            "addr": f"0x{addr:08X}",
            "word": None if word is None else f"0x{word:08X}",
            "expected": f"0x{expected:08X}",
            "section": section.name if section else None,
            "executable": bool(section and section.executable),
        }
        if word is None:
            errors.append(f"{name}: hook 0x{addr:08X} is not readable")
        elif word != expected:
            errors.append(f"{name}: hook word 0x{word:08X} != 0x{expected:08X}")
        if section is None or not section.executable:
            errors.append(f"{name}: hook 0x{addr:08X} is not in an executable section")

    caves = {}
    spans: dict[str, tuple[int, int]] = {}
    for name, cave in profile["caves"].items():
        base = int(cave["base"])
        size = int(cave["size"])
        section = pe.section_for(base)
        info = {
            "base": f"0x{base:08X}",
            "size": f"0x{size:X}",
            "range": f"0x{base:08X}..0x{base + size:08X}",
            "section": section.name if section else None,
            "executable": bool(section and section.executable),
        }
        caves[name] = info
        spans[name] = (base, base + size)
        if section is None:
            errors.append(f"cave {name}: 0x{base:08X} is outside every section")
        else:
            if not section.executable:
                errors.append(f"cave {name}: section {section.name} is not executable")
            if base + size > section.end:
                errors.append(f"cave {name}: range crosses the end of {section.name}")
        hook_name = next((h for h, hook in profile["hooks"].items()
                          if hook.get("cave") == name), None)
        if hook_name:
            hook_addr = int(profile["hooks"][hook_name]["addr"])
            if not _branch_range_ok(hook_addr, base):
                errors.append(f"{name}: hook 0x{hook_addr:08X} cannot reach 0x{base:08X}")
            if hook_name != "mapwarp":
                next_addr = int(profile["hooks"][hook_name]["next"])
                if not _branch_range_ok(base, next_addr):
                    warnings.append(f"{name}: cave cannot branch back to 0x{next_addr:08X}")

    cave_names = list(profile["caves"])
    for i, a in enumerate(cave_names):
        a_base = int(profile["caves"][a]["base"])
        a_end = a_base + int(profile["caves"][a]["size"])
        for b in cave_names[i + 1:]:
            b_base = int(profile["caves"][b]["base"])
            b_end = b_base + int(profile["caves"][b]["size"])
            if a_base < b_end and b_base < a_end:
                errors.append(f"caves {a} and {b} overlap")

    ring = profile["ring"]
    ring_base = int(ring["base"])
    ring_cap = int(ring["cap"])
    ring_size = int(ring["header_size"]) + ring_cap * int(ring["recsz"])
    ring_section = pe.section_for(ring_base)
    zero_ok, zero_reason = pe.zero_state(ring_base, ring_size)
    ring_info = {
        "base": f"0x{ring_base:08X}",
        "cap": ring_cap,
        "size": f"0x{ring_size:X}",
        "range": f"0x{ring_base:08X}..0x{ring_base + ring_size:08X}",
        "section": ring_section.name if ring_section else None,
        "writable": bool(ring_section and ring_section.writable),
        "zero": zero_ok,
        "zero_reason": zero_reason,
    }
    spans["ring"] = (ring_base, ring_base + ring_size)
    if ring_section is None:
        errors.append(f"ring 0x{ring_base:08X} is outside every section")
    else:
        if not ring_section.writable:
            errors.append(f"ring section {ring_section.name} is not writable")
        if ring_base + ring_size > ring_section.end:
            errors.append(f"ring crosses the end of {ring_section.name}")
    if not zero_ok:
        errors.append(f"ring is not zero: {zero_reason}")

    if scan_index_refs:
        try:
            hits = scan_index(root / paths["index"], spans,
                              set(profile.get("validation", {}).get("index_ignore", [])))
        except ValidationError as exc:
            warnings.append(str(exc))
            hits = {}
        for name, rows in hits.items():
            if rows:
                errors.append(f"{name}: {len(rows)} indexed ref(s) inside span, "
                              f"e.g. {rows[0]}")
    else:
        hits = {}

    return {
        "ok": not errors,
        "errors": errors,
        "warnings": warnings,
        "pe": {
            "path": str(pe_path),
            "machine": f"0x{pe.machine:04X}",
            "image_base": f"0x{pe.image_base:X}",
            "file_size": f"0x{pe.file_size:X}",
            "size_of_image": f"0x{pe.size_of_image:X}",
            "sections": [
                {"name": s.name, "va": f"0x{s.va:08X}", "vsize": f"0x{s.vsize:X}",
                 "raw_size": f"0x{s.raw_size:X}", "flags": f"0x{s.flags:08X}"}
                for s in pe.sections
            ],
        },
        "hooks": hooks,
        "caves": caves,
        "ring": ring_info,
        "index_hits": {k: v[:3] for k, v in hits.items()},
    }


def require_ok(result: dict) -> None:
    if not result["ok"]:
        raise ValidationError("; ".join(result["errors"]))
