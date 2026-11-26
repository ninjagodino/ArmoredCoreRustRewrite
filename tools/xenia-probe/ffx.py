#!/usr/bin/env python3
"""FFX record JSONL, annotation, and near-target summary."""
from __future__ import annotations

import json
import math
from collections import Counter, defaultdict
from pathlib import Path

from ppcgen import Cave


def build_ffx_cave(base: int, ring_base: int, cap: int,
                   *, data_offset: int = 0x100, recsz: int = 24) -> list[int]:
    """Emit the FFX spawn ring writer hooked at 0x82B88A78.

    At the hook r3=output, r4=request, r5=ffx id, r6=0 (Phase 0).  The record is
    ``ffx_id, owner([r4+0x3C]), caller_lr, x/y/z([r4+0x10..18])``; floats are
    copied as raw words.  The original ``mflr r12`` is re-emitted at the exit
    before branching to 0x82B88A7C.
    """
    if cap & (cap - 1):
        raise ValueError("ring capacity must be a power of two")
    c = Cave(base)
    c.stwu(1, 1, -0x160)
    for reg, off in ((0, 0x10), (10, 0x18), (11, 0x20), (12, 0x28)):
        c.std(reg, 1, off)
    c.mfcr(0)
    c.stw(0, 1, 0x30)
    c.mflr(0)
    c.std(0, 1, 0x38)          # caller LR; low word at 0x3C

    c.lis(11, ring_base >> 16)
    c.ori(11, 11, ring_base & 0xFFFF)
    c.lwz(12, 11, 0x08)
    c.lis(0, 0x4158)
    c.ori(0, 0, 0x5052)        # AXPR
    c.cmpw(12, 0)
    c.bne("exit")
    c.lwz(12, 11, 0x10)
    c.andi_dot(12, 12, 1)      # flags bit0 = ring enabled
    c.beq("exit")

    c.addi(10, 11, 0)          # atomic claim of write_index
    c.mark("retry")
    c.lwarx(12, 0, 10)
    c.addi(0, 12, 1)
    c.stwcx_dot(0, 0, 10)
    c.bne_minus("retry")

    c.andi_dot(12, 12, cap - 1)
    c.mulli(12, 12, recsz)
    c.addi(12, 12, data_offset)
    c.add(10, 11, 12)

    c.stw(5, 10, 0)            # ffx_id
    c.lwz(0, 4, 0x3C)          # owner/dummy object pointer
    c.stw(0, 10, 4)
    c.lwz(0, 1, 0x3C)          # caller LR (low word of std slot)
    c.stw(0, 10, 8)
    for off, rec_off in ((0x10, 12), (0x14, 16), (0x18, 20)):
        c.lwz(0, 4, off)
        c.stw(0, 10, rec_off)
    c.sync()

    c.mark("exit")
    c.lwz(0, 1, 0x30)
    c.mtcrf(0xFF, 0)
    c.ld(0, 1, 0x38)
    c.mtlr(0)
    for reg, off in ((0, 0x10), (10, 0x18), (11, 0x20), (12, 0x28)):
        c.ld(reg, 1, off)
    c.addi(1, 1, 0x160)
    c.mflr(12)                 # re-emit the hooked instruction verbatim
    c.b(0x82B88A7C)
    return c.finish()


def load_jsonl(path) -> list[dict]:
    records = []
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return records


def write_jsonl(path, records) -> None:
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for record in records:
            fh.write(json.dumps(record, separators=(",", ":")) + "\n")


def _distance(record, target) -> float:
    return math.sqrt((record["x"] - target[0]) ** 2
                     + (record["y"] - target[1]) ** 2
                     + (record["z"] - target[2]) ** 2)


def summarize(records: list[dict], *, target=None, map_name: str | None = None) -> dict:
    by_id = Counter(int(r.get("ffx", 0)) for r in records)
    by_lr = Counter(int(r.get("lr", 0)) for r in records)
    summary = {
        "map": map_name,
        "count": len(records),
        "unique_ffx": len(by_id),
        "unique_lr": len(by_lr),
        "ffx_top": [{"id": f"0x{k:X}", "count": v} for k, v in by_id.most_common(20)],
        "lr_top": [{"lr": f"0x{k:08X}", "count": v} for k, v in by_lr.most_common(20)],
        "first_ms": records[0]["ms"] if records else None,
        "last_ms": records[-1]["ms"] if records else None,
        "dropped": records[-1].get("dropped", 0) if records else 0,
    }
    if target is not None:
        radii = (10.0, 50.0, 200.0)
        near = {f"within_{int(r)}m": 0 for r in radii}
        nearest = None
        near_ids = Counter()
        for record in records:
            dist = _distance(record, target)
            if nearest is None or dist < nearest["distance"]:
                nearest = {"distance": dist, "n": record.get("n"),
                           "ffx": f"0x{int(record.get('ffx', 0)):X}",
                           "lr": f"0x{int(record.get('lr', 0)):08X}",
                           "pos": [record.get("x"), record.get("y"), record.get("z")]}
            for radius in radii:
                if dist <= radius:
                    near[f"within_{int(radius)}m"] += 1
            if dist <= 50.0:
                near_ids[int(record.get("ffx", 0))] += 1
        summary["target"] = list(target)
        summary["near"] = near
        summary["nearest"] = nearest
        summary["near_ffx_top"] = [{"id": f"0x{k:X}", "count": v}
                                   for k, v in near_ids.most_common(20)]
    return summary


def annotate(records: list[dict], symbols, out_dir, *,
             map_name: str | None = None, target=None,
             max_unique: int = 128) -> dict:
    """Write ffx_annotated.txt and ffx_summary.json; never fail the run."""
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    summary = summarize(records, target=target, map_name=map_name)
    by_function = Counter()
    unresolved = []
    blocks = []
    lrs = Counter(int(r.get("lr", 0)) for r in records)
    for lr, count in lrs.most_common(max_unique):
        info = symbols.label(lr)
        if info["function"] is None:
            unresolved.append(f"0x{lr:08X}")
            blocks.append(f"LR 0x{lr:08X} count={count}  (unresolved)")
            blocks.append("")
            continue
        by_function[f"0x{info['function']:08X}"] += count
        lo = max(info["function"], info["site"] - 0x30)
        hi = min(info["function_end"], info["site"] + 0x10)
        decoded = symbols.decode_slice(lo, hi)
        blocks.append(
            f"LR 0x{lr:08X} site 0x{info['site']:08X} count={count}  "
            f"func 0x{info['function']:08X}..0x{info['function_end']:08X} "
            f"(+0x{info['offset']:X}) {info['name']}")
        if decoded:
            blocks.extend("  " + line for line in decoded.splitlines())
        blocks.append("")
    if len(lrs) > max_unique:
        blocks.append(f"... {len(lrs) - max_unique} more unique LR(s) not decoded")
    (out / "ffx_annotated.txt").write_text("\n".join(blocks).rstrip() + "\n",
                                           encoding="utf-8")
    summary["by_function"] = [{"function": k, "count": v}
                              for k, v in by_function.most_common()]
    summary["unresolved_lr"] = unresolved
    summary["annotated_unique_lr"] = min(len(lrs), max_unique)
    (out / "ffx_summary.json").write_text(json.dumps(summary, indent=2),
                                           encoding="utf-8")
    return summary
