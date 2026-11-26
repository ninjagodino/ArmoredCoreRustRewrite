#!/usr/bin/env python3
"""Mission/asset preflight and authored MSB part transforms.

The heavy lifting is ``crates/acvd-formats/examples/probe_helper.rs`` (disc
``vfs`` + ``msb``).  The extracted caches under ``private/tmp`` are used only as
a fallback when the helper binary has not been built; neither path is a source
of placement truth for rendering, this is lookup for camera targeting.
"""
from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path

PART_LINE_RE = re.compile(
    r"kind=(?P<kind>\S+) layer=(?P<layer>\S+) name=(?P<name>\S+) "
    r"model=(?P<model>\S+) t=\((?P<tx>[-\d.]+),(?P<ty>[-\d.]+),(?P<tz>[-\d.]+)\) "
    r"rot=\((?P<rx>[-\d.]+),(?P<ry>[-\d.]+),(?P<rz>[-\d.]+)\)")


def map_name(map_id: int) -> str:
    return f"m{map_id:04d}"


def _helper_exe(root: Path) -> Path | None:
    for candidate in (root / "target" / "release" / "examples" / "probe_helper.exe",
                      root / "target" / "release" / "examples" / "probe_helper",
                      root / "target" / "release" / "probe_helper.exe",
                      root / "target" / "release" / "probe_helper"):
        if candidate.exists():
            return candidate
    return None


def run_helper(root, args, *, timeout: float = 30.0) -> str | None:
    exe = _helper_exe(Path(root))
    if exe is None:
        return None
    try:
        out = subprocess.run([str(exe), *map(str, args)], cwd=str(root),
                             capture_output=True, text=True, timeout=timeout,
                             errors="replace")
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    return out.stdout


def mission_xml_path(profile: dict, mission_id: int) -> str:
    return profile["map"]["mission_xml"].format(id=mission_id)


def mission_exists(root, profile: dict, mission_id: int) -> bool:
    rel = mission_xml_path(profile, mission_id)
    cache = Path(root) / profile["paths"]["missions_cache"] / f"ch{mission_id:04d}.xml"
    if cache.exists():
        return True
    out = run_helper(root, ["mission", mission_id])
    if out:
        for line in out.splitlines():
            try:
                data = json.loads(line)
            except json.JSONDecodeError:
                continue
            if data.get("exists") is not None:
                return bool(data["exists"])
    return False


def mission_has_layer(root, profile: dict, mission_id: int, layer: str) -> bool:
    cache = Path(root) / profile["paths"]["missions_cache"] / f"ch{mission_id:04d}.xml"
    if cache.exists():
        text = cache.read_text(encoding="utf-8", errors="replace")
        return f"<{layer}>" in text
    return False


def map_assets(root, profile: dict, map_id: int) -> dict:
    name = map_name(map_id)
    cache = Path(root) / profile["paths"]["allmaps_cache"]
    result = {}
    for key, rel in (
        ("base", profile["map"]["map_msb"].format(map=name)),
        ("map", profile["map"]["map_map_msb"].format(map=name)),
        ("env", profile["map"]["map_env_msb"].format(map=name)),
    ):
        cached = cache / rel
        result[key] = {"path": rel, "exists": cached.exists(), "bytes": cached.stat().st_size if cached.exists() else 0}
    out = run_helper(root, ["mapfiles", name])
    if out:
        for line in out.splitlines():
            try:
                data = json.loads(line)
            except json.JSONDecodeError:
                continue
            for key in ("base", "map", "env"):
                if key in data and isinstance(data[key], dict):
                    result[key] = data[key]
    return result


def _parts_from_helper(root, profile, map_id: int, model: str,
                       instance: str | None) -> list[dict]:
    args = ["msb", map_name(map_id), model]
    if instance is not None:
        args.append(instance)
    out = run_helper(root, args)
    if not out:
        return []
    parts = []
    for line in out.splitlines():
        try:
            parts.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return parts


def _parts_from_cache(root, profile, map_id: int, model: str) -> list[dict]:
    """Parse extracted MSB text via partsdump only when the helper is absent."""
    name = map_name(map_id)
    cache = Path(root) / profile["paths"]["allmaps_cache"] / "model" / "map" / name / f"{name}_map.msb"
    if not cache.exists():
        return []
    # partsdump needs a disc asset path; use it through cargo only as a last
    # resort.  Tests and normal runs use probe_helper.
    rel = f"model/map/{name}/{name}_map.msb"
    env = dict(**__import__("os").environ)
    env["CARGO_TARGET_DIR"] = str(Path(root) / "target")
    env["PATH"] = str(Path.home() / ".cargo" / "bin") + ";" + env.get("PATH", "")
    try:
        out = subprocess.run(
            ["cargo", "run", "--release", "-q", "-p", "acvd-formats",
             "--example", "partsdump", "--", rel],
            cwd=str(root), capture_output=True, text=True, timeout=120,
            errors="replace", env=env)
    except (OSError, subprocess.TimeoutExpired):
        return []
    parts = []
    if out.returncode == 0:
        for line in out.stdout.splitlines():
            match = PART_LINE_RE.search(line)
            if match and match.group("model") == model:
                parts.append({
                    "name": match.group("name"),
                    "model": match.group("model"),
                    "t": [float(match.group("tx")), float(match.group("ty")),
                          float(match.group("tz"))],
                    "rot": [float(match.group("rx")), float(match.group("ry")),
                            float(match.group("rz"))],
                    "source": rel,
                })
    return parts


def find_part(root, profile, map_id: int, model: str,
              instance: str | None = None) -> dict | None:
    parts = _parts_from_helper(root, profile, map_id, model, instance)
    if not parts and instance is None:
        parts = _parts_from_cache(root, profile, map_id, model)
    if not parts:
        return None
    if instance is not None:
        if instance.isdigit():
            index = int(instance)
            if 0 <= index < len(parts):
                return parts[index]
        for part in parts:
            if part.get("name") == instance or str(part.get("name", "")).endswith(instance):
                return part
        return None
    return parts[0]


def parse_look_at(spec: str):
    """Return ('xyz', tuple) | ('ac', None) | ('part', (model, instance))."""
    value = spec.strip()
    if value.lower() == "ac":
        return "ac", None
    if value.lower().startswith("part:"):
        rest = value[5:]
        model, _, instance = rest.partition(":")
        if not model:
            raise ValueError(f"bad --look-at part spec: {spec}")
        return "part", (model, instance or None)
    # Bare model IDs (o9341, e6010, ...) are accepted as part look-at specs.
    if re.fullmatch(r"[a-z]{1,2}\d{3,5}", value.lower()):
        return "part", (value, None)
    pieces = value.replace("%2C", ",").split(",")
    if len(pieces) == 3:
        return "xyz", tuple(float(p) for p in pieces)
    raise ValueError(f"bad --look-at value '{spec}' "
                     "(expected X,Y,Z, ac, or part:<model>[:<instance>])")


def resolve_look_at(spec: str, root, profile, map_id: int):
    kind, value = parse_look_at(spec)
    if kind == "xyz":
        return kind, value, None
    if kind == "ac":
        return kind, None, None
    model, instance = value
    part = find_part(root, profile, map_id, model, instance)
    if part is None:
        raise ValueError(f"part {model!r} not found in {map_name(map_id)} MSB")
    return kind, tuple(part["t"]), part
