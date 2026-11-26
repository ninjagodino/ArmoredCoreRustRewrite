#!/usr/bin/env python3
"""Function bounds, best-effort names, and decoded callsite slices."""
from __future__ import annotations

import csv
import re
import subprocess
import sys
from pathlib import Path

FUNC_RE = re.compile(r"is in (0x[0-9A-Fa-f]+)\.\.(0x[0-9A-Fa-f]+)")
ADDR_RE = re.compile(r"0x[0-9A-Fa-f]{8}")


class SymbolIndex:
    def __init__(self, root, profile, *, timeout: float = 20.0):
        self.root = Path(root)
        self.profile = profile
        paths = profile["paths"]
        self.index_exe = self.root / paths["index_exe"]
        self.x360dis = self.root / paths["x360dis"]
        self.named_path = self.root / paths["named_funcs"]
        self.timeout = timeout
        self._bounds: dict[int, tuple[int, int] | None] = {}
        self._names: dict[int, str] | None = None

    def _named(self) -> dict[int, str]:
        if self._names is not None:
            return self._names
        names: dict[int, str] = {}
        if self.named_path.exists():
            try:
                with open(self.named_path, newline="", encoding="utf-8",
                          errors="replace") as fh:
                    for row in csv.reader(fh):
                        if len(row) < 2:
                            continue
                        raw = row[0].strip()
                        try:
                            addr = int(raw, 16)
                        except ValueError:
                            continue
                        names[addr] = row[1].strip()
            except OSError:
                pass
        self._names = names
        return names

    def func_bounds(self, addr: int) -> tuple[int, int] | None:
        addr &= 0xFFFFFFFF
        if addr in self._bounds:
            return self._bounds[addr]
        if not self.index_exe.exists():
            self._bounds[addr] = None
            return None
        try:
            out = subprocess.run(
                [str(self.index_exe), "q", "func", f"0x{addr:08X}"],
                cwd=str(self.root), capture_output=True, text=True,
                timeout=self.timeout, errors="replace")
        except (OSError, subprocess.TimeoutExpired):
            self._bounds[addr] = None
            return None
        match = FUNC_RE.search(out.stdout or "")
        result = None
        if match:
            result = (int(match.group(1), 16), int(match.group(2), 16))
        self._bounds[addr] = result
        return result

    def name_for(self, func_start: int) -> str:
        return self._named().get(func_start & 0xFFFFFFFF,
                                 f"FUN_{func_start & 0xFFFFFFFF:08X}")

    def label(self, lr: int) -> dict:
        """Resolve a return address to its containing function and callsite."""
        site = (lr - 4) & 0xFFFFFFFF
        bounds = self.func_bounds(site)
        if bounds is None:
            return {"lr": lr, "site": site, "function": None,
                    "function_end": None, "name": f"FUN_{site:08X}",
                    "offset": None}
        start, end = bounds
        return {"lr": lr, "site": site, "function": start,
                "function_end": end, "name": self.name_for(start),
                "offset": site - start}

    def decode_slice(self, lo: int, hi: int, *, max_bytes: int = 0xC0) -> str:
        lo &= 0xFFFFFFFF
        hi &= 0xFFFFFFFF
        if hi <= lo:
            return ""
        if hi - lo > max_bytes:
            hi = lo + max_bytes
        if not self.x360dis.exists():
            return ""
        try:
            out = subprocess.run(
                [sys.executable, str(self.x360dis), "--range",
                 f"0x{lo:08X}", f"0x{hi:08X}", "--raw"],
                cwd=str(self.root), capture_output=True, text=True,
                timeout=self.timeout, errors="replace")
        except (OSError, subprocess.TimeoutExpired):
            return ""
        return out.stdout.strip()
