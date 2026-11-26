#!/usr/bin/env python3
"""Unlock-free map / FFX / camera probes for the Xbox 360 Verdict Day build.

Typical use:

    py tools\\xenia-probe\\xenia_probe.py --map m3120 --look-at part:o9341 \
        --dump-ffx --annotate --shot private\\xenia\\shots\\m3120.png

The tool verifies the static caves, generates a ``4E4D0864`` patch, launches
Xenia with the patch, drives the AC Test menu with the existing
``tools/xenia/recipes/ac_test.txt`` (state-gated on ``press_dialog`` and
``env_bgm``), drains the FFX ring, applies the camera override, captures a
guest shot, and annotates every visible FFX callsite.

Target is Xbox 360 only.  No PS3 address or artifact is used.
"""
from __future__ import annotations

import argparse
import ctypes
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

import ffx  # noqa: E402
import mapwarp  # noqa: E402
import parts  # noqa: E402
import patchgen  # noqa: E402
import ring  # noqa: E402
import state_nav  # noqa: E402
import symbols  # noqa: E402
import validate  # noqa: E402

DEFAULT_GAME = "vd"


class ProbeError(RuntimeError):
    def __init__(self, message: str, code: int = 1):
        super().__init__(message)
        self.code = code


def load_profile(game: str) -> dict:
    path = HERE / "profiles" / f"{game}.toml"
    if not path.exists():
        raise ProbeError(f"missing profile: {path}", 2)
    with open(path, "rb") as fh:
        return tomllib.load(fh)


def parse_map(value: str) -> int:
    text = value.strip().lower()
    if text.startswith("ch"):
        text = text[2:]
    if text.startswith("m"):
        text = text[1:]
    if not text.isdigit():
        raise ProbeError(f"bad --map value: {value}", 2)
    map_id = int(text)
    if not 0 <= map_id <= 9999:
        raise ProbeError(f"map id out of range: {value}", 2)
    return map_id


def parse_xyz(value: str) -> tuple[float, float, float]:
    pieces = value.split(",")
    if len(pieces) != 3:
        raise ProbeError(f"expected X,Y,Z, got {value!r}", 2)
    return tuple(float(p) for p in pieces)  # type: ignore[return-value]


def choose_mission(root, profile, map_id: int, args) -> tuple[int, int, str]:
    """Return (mission_id, warp_mode, mission_source)."""
    if args.mission is not None:
        return args.mission, 0, "explicit"
    default_map = int(profile["mission"]["default_map"])
    ac_test_id = int(profile["mission"]["ac_test_id"])
    if map_id == default_map:
        return ac_test_id, 0, "ac_test"
    # A disc mission XML is the clean path: it supplies the spawn/layers that
    # an AC Test map warp cannot invent.  The old actest-layer gate made m3120
    # fall back to mission 6000 + mapwarp, whose sortie loaded assets but never
    # reached a playable mission.
    if parts.mission_exists(root, profile, map_id):
        return map_id, 0, "map_mission"
    assets = parts.map_assets(root, profile, map_id)
    if assets["base"]["exists"]:
        mode = int(profile["map"]["warp_mode_base_msb"])
    elif assets["map"]["exists"]:
        mode = int(profile["map"]["warp_mode_map_msb"])
    else:
        raise ProbeError(f"map {parts.map_name(map_id)} has no MSB on disc", 2)
    return ac_test_id, mode, "ac_test_warp"


def make_patch(root, profile, *, mission_id: int, map_id: int, warp_mode: int,
               dump_ffx: bool = False) -> tuple[patchgen.PatchSpec, dict]:
    hooks = profile["hooks"]
    caves = profile["caves"]
    mission = profile["mission"]
    spec = patchgen.PatchSpec(
        title_name=profile["title_name"],
        title_id=int(profile["title_id"]),
        module_hash=profile["module_hash"],
        name=profile["patch"]["name"],
        desc=profile["patch"]["desc"],
        author=profile["patch"]["author"],
    )

    patchgen.mission_patch(spec, int(mission["getter"]),
                           int(mission["getter_word"]), mission_id)

    ffx_words = ffx.build_ffx_cave(
        int(caves["ffx"]["base"]), int(profile["ring"]["base"]),
        int(profile["ring"]["cap"]), data_offset=int(profile["ring"]["data_offset"]),
        recsz=int(profile["ring"]["recsz"]))
    if len(ffx_words) * 4 > int(caves["ffx"]["size"]):
        raise ProbeError("FFX cave exceeds its slot", 2)
    patchgen.cave_patch(spec, int(hooks["ffx"]["addr"]),
                        int(hooks["ffx"]["word"]), int(caves["ffx"]["base"]),
                        ffx_words, int(hooks["ffx"]["next"]))

    camera_words = ring.build_camera_cave(
        int(caves["camera"]["base"]), int(profile["ring"]["base"]),
        profile["ring"]["offsets"], int(hooks["camera"]["next"]))
    if len(camera_words) * 4 > int(caves["camera"]["size"]):
        raise ProbeError("camera cave exceeds its slot", 2)
    patchgen.cave_patch(spec, int(hooks["camera"]["addr"]),
                        int(hooks["camera"]["word"]), int(caves["camera"]["base"]),
                        camera_words, int(hooks["camera"]["next"]))

    warp_info = {"mode": warp_mode, "target": None}
    warp_words = []
    if warp_mode:
        target = parts.map_name(map_id)
        warp_words = mapwarp.build_cave(int(caves["mapwarp"]["base"]),
                                        int(profile["ring"]["base"]),
                                        target, warp_mode,
                                        offsets=profile["ring"]["offsets"])
        if len(warp_words) * 4 > int(caves["mapwarp"]["size"]):
            raise ProbeError("mapwarp cave exceeds its slot", 2)
        patchgen.cave_patch(spec, int(hooks["mapwarp"]["addr"]),
                            int(hooks["mapwarp"]["word"]),
                            int(caves["mapwarp"]["base"]), warp_words,
                            int(hooks["mapwarp"]["next"]), return_mode="blr")
        warp_info.update({"target": target,
                          "size": len(warp_words) * 4})

    patchgen.ring_prearm(spec, profile["ring"], profile["ring"]["offsets"],
                         dump_ffx=dump_ffx, warp_mode=warp_mode)
    sizes = {
        "ffx": len(ffx_words) * 4,
        "camera": len(camera_words) * 4,
        "mapwarp": warp_info.get("size", 0),
    }
    exits = {
        "ffx": int(caves["ffx"]["base"]) + (len(ffx_words) - 1) * 4,
        "camera": int(caves["camera"]["base"]) + (len(camera_words) - 1) * 4,
    }
    if warp_words:
        exits["mapwarp"] = (int(caves["mapwarp"]["base"])
                            + (len(warp_words) - 1) * 4)
    return spec, {"warp": warp_info, "cave_sizes": sizes, "cave_exits": exits}


def patch_summary(spec: patchgen.PatchSpec) -> dict:
    return {
        "be32": len(spec.be32),
        "arrays": len(spec.arrays),
        "bytes": sum(len(data) for _, data in spec.arrays),
        "text_bytes": len(patchgen.render_patch(spec)),
    }


def preflight(root, profile, args, mission_id: int, warp_mode: int,
              map_id: int) -> dict:
    paths = profile["paths"]
    checks = {}
    for key, rel in (("xenia", paths["xenia_bin"]), ("iso", paths["iso"]),
                     ("drive", paths["drive"]), ("recipe", args.recipe or paths["recipe"]),
                     ("pe", paths["pe"]), ("index", paths["index"])):
        path = root / rel
        checks[key] = {"path": str(path), "exists": path.exists()}
        if not checks[key]["exists"]:
            raise ProbeError(f"missing {key}: {path}", 2)
    exe = root / paths["xenia_bin"] / paths["xenia_exe"]
    if not exe.exists():
        raise ProbeError(f"missing xenia executable: {exe}", 2)
    index_exe = root / paths["index_exe"]
    if not index_exe.exists():
        raise ProbeError(f"missing acvd-index: {index_exe} (build it first)", 2)
    x360dis = root / paths["x360dis"]
    if not x360dis.exists():
        raise ProbeError(f"missing x360dis: {x360dis}", 2)
    if not parts.mission_exists(root, profile, mission_id):
        raise ProbeError(f"mission/ch{mission_id:04d}.xml not found on disc/cache", 2)
    if warp_mode:
        assets = parts.map_assets(root, profile, map_id)
        checks["map_assets"] = assets
        if not (assets["base"]["exists"] or assets["map"]["exists"]):
            raise ProbeError(f"no MSB for {parts.map_name(map_id)}", 2)
        if warp_mode == 1 and not assets["base"]["exists"]:
            raise ProbeError(f"{parts.map_name(map_id)}.msb missing for mode 1", 2)
        if warp_mode == 2 and not assets["map"]["exists"]:
            raise ProbeError(f"{parts.map_name(map_id)}_map.msb missing for mode 2", 2)
    return checks


def patch_path(root, profile) -> Path:
    return root / profile["paths"]["xenia_bin"] / "patches" / profile["patch"]["file"]


def check_patch_dir(root, profile) -> Path:
    patches = patch_path(root, profile).parent
    patches.mkdir(parents=True, exist_ok=True)
    own = patcher_own_name(profile)
    for path in patches.glob("4E4D0864*.patch.toml"):
        if path.name == own:
            path.unlink()
            continue
        raise ProbeError(f"refusing to run with another probe patch present: {path}", 2)
    return patches


def patcher_own_name(profile) -> str:
    return profile["patch"]["file"]


def xenia_running() -> bool:
    try:
        out = subprocess.run(["tasklist", "/FI", "IMAGENAME eq xenia_canary.exe"],
                             capture_output=True, text=True, timeout=10,
                             errors="replace")
    except (OSError, subprocess.TimeoutExpired):
        return False
    return "xenia_canary.exe" in (out.stdout or "").lower()


def probe_lines(profile, build_info: dict) -> list[str]:
    lines = list(profile["probes"]["lines"])
    exits = build_info.get("cave_exits", {})
    if "ffx" in exits:
        lines.append(f"0x{exits['ffx']:08X} ffx_cave_exit max=4 "
                     f"after=press_dialog r5")
    if "camera" in exits:
        lines.append(f"0x{exits['camera']:08X} cam_cave_exit max=4 "
                     f"after=actest_sortie_load r31")
    return lines


def write_probes(path: Path, profile, build_info: dict) -> None:
    lines = probe_lines(profile, build_info)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def send_input(input_path: Path, line: str) -> None:
    with open(input_path, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")
        fh.flush()


def wait_file(path: Path, timeout: float = 10.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists() and path.stat().st_size > 0:
            return True
        time.sleep(0.1)
    return False


def take_shot(input_path: Path, path: Path, *, timeout: float = 10.0) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        try:
            path.unlink()
        except OSError:
            pass
    send_input(input_path, f"shot {path}")
    return wait_file(path, timeout)


def event_label(event: dict) -> str:
    return str(event.get("label", ""))


def wait_event(log: state_nav.ProbeLog, predicate, timeout: float, proc,
               description: str, *, drainer=None, records=None,
               input_path: Path | None = None, debug_shot: Path | None = None,
               driver=None, interval: float = 0.015):
    deadline = time.monotonic() + timeout
    while True:
        log.poll()
        while log.cursor < len(log.events):
            event = log.events[log.cursor]
            log.cursor += 1
            if predicate(event):
                print(f"[state] {description} at {event.get('ms')} ms: "
                      f"{event.get('label')} {event.get('r5', '')}", flush=True)
                return event
        if proc.poll() is not None:
            raise ProbeError(f"Xenia exited while waiting for {description}", 7)
        if driver is not None and driver.poll() is not None and driver.returncode:
            raise ProbeError(f"recipe failed with exit code {driver.returncode} "
                             f"while waiting for {description}", 7)
        if time.monotonic() >= deadline:
            if debug_shot is not None and input_path is not None:
                take_shot(input_path, debug_shot, timeout=5.0)
            raise ProbeError(f"timeout ({timeout:.0f}s) waiting for {description}", 7)
        if drainer is not None:
            now_ms = int((time.monotonic() - wait_event.start) * 1000)
            new = drainer.drain(now_ms)
            if records is not None:
                records.extend(new)
        time.sleep(interval)


wait_event.start = time.monotonic()


def wait_camera_ready(drainer: ring.RingDrainer, records: list, proc,
                      driver, timeout_s: float) -> dict:
    """Wait for the camera cave to publish a pointer and tick frames."""
    start = time.monotonic()
    first_frame = None
    last = None
    while time.monotonic() - start < timeout_s:
        last = drainer.camera_status() or {}
        now_ms = int((time.monotonic() - wait_event.start) * 1000)
        records.extend(drainer.drain(now_ms))
        if last.get("cam_ptr"):
            frame = int(last.get("cam_frame", 0))
            if first_frame is None:
                first_frame = frame
            elif (frame - first_frame) & 0xFFFFFFFF >= 2:
                return {
                    "ready": True,
                    "waited_ms": int((time.monotonic() - start) * 1000),
                    "cam_ptr": last.get("cam_ptr"),
                    "first_frame": first_frame,
                    "cam_frame": frame,
                }
        if proc.poll() is not None:
            break
        if driver is not None and driver.poll() is not None and driver.returncode:
            break
        time.sleep(0.05)
    return {
        "ready": False,
        "waited_ms": int((time.monotonic() - start) * 1000),
        "cam_ptr": (last or {}).get("cam_ptr"),
        "first_frame": first_frame,
        "cam_frame": (last or {}).get("cam_frame"),
    }


def settle_after_env(drainer: ring.RingDrainer, records: list, log,
                     proc, driver, seconds: float) -> None:
    """Keep draining while the mission fades in; the ring keeps history."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        log.poll()
        now_ms = int((time.monotonic() - wait_event.start) * 1000)
        records.extend(drainer.drain(now_ms))
        if proc.poll() is not None:
            break
        if driver is not None and driver.poll() is not None and driver.returncode:
            break
        time.sleep(0.05)
    records.extend(drainer.drain(int((time.monotonic() - wait_event.start) * 1000)))


def probe_hit_counts(log: state_nav.ProbeLog) -> dict:
    counts: dict[str, int] = {}
    for event in log.events:
        label = event_label(event)
        counts[label] = counts.get(label, 0) + 1
    return counts


def warp_counters(drainer: ring.RingDrainer) -> dict:
    header = drainer.header() or {}
    return {
        "mode": header.get("warp_mode"),
        "calls": header.get("warp_calls"),
        "map": header.get("warp_map"),
        "msb": header.get("warp_msb"),
    }


def read_xenia_log(path: Path) -> dict:
    info = {"module_hash": None, "patch_applied": False, "lines": []}
    if not path.exists():
        return info
    text = path.read_text(encoding="utf-8", errors="replace")
    for line in text.splitlines():
        if "Module Hash:" in line:
            info["module_hash"] = line.split("Module Hash:", 1)[1].strip()
        lower = line.lower()
        if "applying patch" in lower or "patcher: applying" in lower:
            info["patch_applied"] = True
            info["lines"].append(line.strip())
    return info


def fmt_vec(value) -> list[float] | None:
    if value is None:
        return None
    return [float(v) for v in value]


def apply_look_at(root, profile, map_id: int, spec: str, args,
                  drainer: ring.RingDrainer, mem: ring.ProcessMemory,
                  input_path: Path) -> dict:
    result = {"spec": spec, "ok": False, "error": None}
    kind, target, part = parts.resolve_look_at(spec, root, profile, map_id)
    if part is not None:
        result["part"] = part
    status = drainer.camera_status() or {}
    read_eye = status.get("read_eye")
    read_target = status.get("read_target")
    if kind == "ac":
        target = read_target
        if target is None:
            raise ProbeError("camera readback target unavailable for --look-at ac", 6)
    result["target"] = fmt_vec(target)
    if args.eye:
        eye = parse_xyz(args.eye)
        result["eye_source"] = "explicit"
    else:
        direction = None
        if read_eye is not None and read_target is not None:
            direction = ring.vec_norm(ring.vec_sub(read_eye, read_target))
        if direction is None:
            direction = tuple(float(v) for v in profile["camera"]["default_dir"])
            direction = ring.vec_norm(direction) or (0.0, 0.0, 1.0)
        dist = float(args.dist)
        eye = (target[0] + direction[0] * dist,
               target[1] + direction[1] * dist,
               target[2] + direction[2] * dist)
        result["eye_source"] = "readback"
    result["eye"] = list(eye)
    apply = ring.apply_camera(mem, drainer, profile, eye, target,
                              timeout_s=int(profile["camera"]["frame_timeout_ms"]) / 1000.0)
    result["handshake"] = apply
    time.sleep(int(profile["camera"]["settle_ms"]) / 1000.0)
    drainer.drain(int((time.monotonic() - wait_event.start) * 1000))
    after = drainer.camera_status() or {}
    result["after"] = {
        "cam_ptr": after.get("cam_ptr"),
        "cam_frame": after.get("cam_frame"),
        "read_eye": fmt_vec(after.get("read_eye")),
        "read_target": fmt_vec(after.get("read_target")),
        "cam_enable": after.get("cam_enable"),
    }
    readback_eye = after.get("read_eye")
    readback_target = after.get("read_target")
    result["eye_error_m"] = (ring.vec_dist(readback_eye, eye)
                             if readback_eye is not None else None)
    result["target_error_m"] = (ring.vec_dist(readback_target, target)
                                if readback_target is not None else None)
    max_error = float(profile["camera"].get("max_error_m", 1.0))
    result["ok"] = bool(
        after.get("cam_ptr")
        and result["eye_error_m"] is not None
        and result["eye_error_m"] <= max_error
        and result["target_error_m"] is not None
        and result["target_error_m"] <= max_error
    )
    return result


def summarize_camera(result: dict) -> str:
    if not result:
        return "camera: not requested"
    er = result.get("eye_error_m")
    tr = result.get("target_error_m")
    return (f"camera: ptr=0x{(result.get('after') or {}).get('cam_ptr') or 0:08X} "
            f"eye_err={er if er is None else round(er, 3)} "
            f"target_err={tr if tr is None else round(tr, 3)}")


def annotate_records(root, profile, records: list[dict], out_dir: Path,
                     map_id: int, target) -> dict:
    index = symbols.SymbolIndex(root, profile)
    return ffx.annotate(records, index, out_dir, map_name=parts.map_name(map_id),
                        target=target)


def run_probe(root, profile, args) -> int:
    map_id = parse_map(args.map or "m4000")
    mission_id, warp_mode, mission_source = choose_mission(root, profile, map_id, args)
    spec, build_info = make_patch(root, profile, mission_id=mission_id,
                                  map_id=map_id, warp_mode=warp_mode,
                                  dump_ffx=args.dump_ffx)
    validation = validate.validate(profile, root, scan_index_refs=True)
    if not validation["ok"]:
        raise ProbeError("static validation failed: " + "; ".join(validation["errors"]), 2)

    checks = preflight(root, profile, args, mission_id, warp_mode, map_id)
    look_kind = None
    target = None
    if args.look_at:
        look_kind, target, part = parts.resolve_look_at(args.look_at, root,
                                                        profile, map_id)
    else:
        part = None

    run_name = args.out_dir or f"{time.strftime('%Y%m%d_%H%M%S')}_{parts.map_name(map_id)}"
    out_dir = (root / profile["paths"]["runs"] / run_name).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    shots = root / profile["paths"]["shots"]
    shot_path = (Path(args.shot) if args.shot else
                 shots / f"{run_name}_{parts.map_name(map_id)}.png").resolve()
    debug_shot = shots / f"{run_name}_debug.png"
    probes_path = out_dir / "probes.txt"
    jsonl_path = out_dir / "probes.jsonl"
    xlog_path = out_dir / "xenia.log"
    stdout_path = out_dir / "xenia.stdout.log"
    input_path = out_dir / "commands.input"
    driver_log_path = out_dir / "driver.log"
    ffx_jsonl = out_dir / "ffx.jsonl"
    summary_path = out_dir / "summary.json"
    for path in (probes_path, jsonl_path, xlog_path, stdout_path,
                 input_path, driver_log_path, ffx_jsonl, summary_path):
        if path.exists():
            path.unlink()
    write_probes(probes_path, profile, build_info)
    input_path.write_text("", encoding="utf-8")

    if args.verify or args.dry_run:
        print(json.dumps({
            "ok": True,
            "map": parts.map_name(map_id),
            "mission": mission_id,
            "mission_source": mission_source,
            "warp": build_info["warp"],
            "cave_sizes": build_info["cave_sizes"],
            "patch": patch_summary(spec),
            "validation": validation,
            "preflight": checks,
        }, indent=2))
        if args.print_patch or args.verify:
            print(patchgen.render_patch(spec))
        return 0

    if xenia_running():
        raise ProbeError("xenia_canary.exe is already running; one probe at a time", 2)
    patches_dir = check_patch_dir(root, profile)
    patch_file = patches_dir / profile["patch"]["file"]
    patchgen.write_patch(patch_file, spec)

    xenia_bin = root / profile["paths"]["xenia_bin"]
    xenia_exe = xenia_bin / profile["paths"]["xenia_exe"]
    iso = root / profile["paths"]["iso"]
    drive = root / profile["paths"]["drive"]
    recipe = Path(args.recipe) if args.recipe else root / profile["paths"]["recipe"]
    if not recipe.exists():
        raise ProbeError(f"missing recipe: {recipe}", 2)

    cmd = [
        str(xenia_exe),
        f"--acvd_probes={probes_path}",
        f"--acvd_probe_log={jsonl_path}",
        f"--acvd_input={input_path}",
        "--acvd_input_port=1",
        "--protect_zero=false",
        "--readback_resolve=full",
        profile["patch"]["apply_arg"],
        f"--log_file={xlog_path}",
        str(iso),
    ]
    proc = subprocess.Popen(cmd, cwd=str(xenia_bin),
                            stdout=open(stdout_path, "wb"), stderr=subprocess.STDOUT)
    print(f"[launch] pid={proc.pid} patch={patch_file.name} map={parts.map_name(map_id)} "
          f"mission={mission_id} warp={warp_mode}", flush=True)
    driver = subprocess.Popen([sys.executable, str(drive), str(input_path), str(recipe)],
                              cwd=str(root), stdout=open(driver_log_path, "wb"),
                              stderr=subprocess.STDOUT)

    log = state_nav.ProbeLog(jsonl_path)
    handle = None
    mem = None
    drainer = None
    records = []
    wait_event.start = time.monotonic()
    summary = {
        "run": run_name,
        "map": parts.map_name(map_id),
        "mission": mission_id,
        "mission_source": mission_source,
        "recipe": str(recipe),
        "warp": build_info["warp"],
        "look_at": args.look_at,
        "look_at_kind": look_kind,
        "target": list(target) if target is not None else None,
        "cave_sizes": build_info["cave_sizes"],
        "patch": {"file": patch_file.name, **patch_summary(spec)},
        "validation": validation,
        "preflight": checks,
        "ffx": {},
        "camera": {},
        "shot": None,
        "errors": [],
    }
    try:
        # Attach before the patch's boot writes and start draining immediately.
        deadline = time.monotonic() + 12.0
        while time.monotonic() < deadline:
            read_handle, host_base = state_nav.find_guest_base(proc.pid)
            if read_handle:
                ring.kernel32.CloseHandle(read_handle)
                mem = ring.ProcessMemory(proc.pid, host_base, write=True)
                break
            time.sleep(0.1)
        if mem is None:
            raise ProbeError("guest mapping not found", 3)
        text_section = next(s for s in validation["pe"]["sections"]
                            if s["name"] == ".text")
        text_lo = int(text_section["va"], 16)
        text_hi = text_lo + int(text_section["vsize"], 16)
        drainer = ring.RingDrainer(mem, profile,
                                   lag=int(profile["nav"]["drain_lag"]),
                                   text_lo=text_lo, text_hi=text_hi)
        magic_deadline = time.monotonic() + 10.0
        magic_seen = False
        while time.monotonic() < magic_deadline:
            header = drainer.header()
            if header and header["magic"] == int(profile["ring"]["magic"]):
                magic_seen = True
                summary["ring_header"] = header
                break
            if proc.poll() is not None:
                break
            time.sleep(0.05)
        if not magic_seen:
            xlog = read_xenia_log(xlog_path)
            raise ProbeError(f"AXPR ring magic not observed "
                             f"(patch_applied={xlog['patch_applied']})", 3)

        title = wait_event(
            log,
            lambda e: event_label(e) == profile["nav"]["title_label"],
            float(args.title_timeout), proc, "title (press_dialog)",
            drainer=drainer, records=records, input_path=input_path,
            debug_shot=debug_shot, driver=driver,
            interval=float(profile["nav"]["drain_interval"]))
        mission = wait_event(
            log,
            lambda e: event_label(e) == profile["nav"]["mission_label"],
            float(args.mission_timeout), proc, "mission (env_bgm)",
            drainer=drainer, records=records, input_path=input_path,
            debug_shot=debug_shot, driver=driver,
            interval=float(profile["nav"]["drain_interval"]))
        summary["nav"] = {
            "title": title,
            "mission": mission,
            "elapsed_ms": int((time.monotonic() - wait_event.start) * 1000),
        }

        # The env event fires before the fade-in and before the first camera
        # tick; the previous shot caught that loading frame.  Wait for the
        # camera cave to publish/tick, then let the map and its FFX settle.
        ready = wait_camera_ready(
            drainer, records, proc, driver,
            float(profile["nav"]["camera_ready_timeout"]))
        summary["camera_ready"] = ready
        settle_s = int(profile["nav"]["post_env_settle_ms"]) / 1000.0
        settle_after_env(drainer, records, log, proc, driver, settle_s)
        log.poll()
        summary["post_env"] = {
            "settle_ms": int(settle_s * 1000),
            "camera": drainer.camera_status(),
        }
        mission_hits = [e for e in log.events if event_label(e) == "mission_id"]
        summary["mission_id_hits"] = mission_hits[-8:]
        seen_mission = any(int(str(e.get("r3", "0")), 0) == mission_id
                           for e in mission_hits)
        if not seen_mission:
            summary["errors"].append(f"mission id {mission_id} not observed in probes")

        if args.look_at:
            try:
                summary["camera"] = apply_look_at(root, profile, map_id,
                                                  args.look_at, args, drainer,
                                                  mem, input_path)
            except Exception as exc:  # camera failures are reported, not fatal
                summary["camera"] = {"ok": False, "error": str(exc)}
                summary["errors"].append(f"camera: {exc}")
        else:
            summary["camera"] = {"ok": False, "error": "not requested"}
        if args.look_at and not summary["camera"].get("ok"):
            err = summary["camera"].get("error") or "camera handshake not confirmed"
            if not any(str(e).startswith("camera:") for e in summary["errors"]):
                summary["errors"].append(f"camera: {err}")

        shot_ok = take_shot(input_path, shot_path, timeout=12.0)
        summary["shot"] = str(shot_path) if shot_ok else None
        if not shot_ok:
            summary["errors"].append(f"shot not written: {shot_path}")

        time.sleep(0.1)
        final = drainer.drain(int((time.monotonic() - wait_event.start) * 1000),
                              final=True)
        records.extend(final)
        summary["ffx"] = {
            "count": drainer.n,
            "invalid": drainer.invalid,
            "dropped": drainer.dropped,
            "write_index": drainer.last_write_index,
        }
        summary["probe_hits"] = probe_hit_counts(log)
        summary["warp_counters"] = warp_counters(drainer)
        xlog = read_xenia_log(xlog_path)
        summary["xenia"] = xlog
        if xlog["module_hash"] and xlog["module_hash"] != profile["module_hash"]:
            summary["errors"].append(
                f"module hash {xlog['module_hash']} != {profile['module_hash']}")
        if not xlog["patch_applied"]:
            summary["errors"].append("xenia log did not report Applying patch")
    except Exception as exc:
        summary["errors"].append(str(exc))
        if isinstance(exc, ProbeError):
            summary["exit_code"] = exc.code
        else:
            summary["exit_code"] = 1
        if drainer is not None:
            try:
                summary["warp_counters"] = warp_counters(drainer)
                summary["probe_hits"] = probe_hit_counts(log)
                summary["post_env"] = {
                    "settle_ms": None,
                    "camera": drainer.camera_status(),
                }
            except Exception:
                pass
        if input_path.exists():
            take_shot(input_path, debug_shot, timeout=5.0)
    finally:
        if driver.poll() is None:
            driver.terminate()
            try:
                driver.wait(timeout=5)
            except subprocess.TimeoutExpired:
                driver.kill()
        state_nav.terminate(proc)
        if mem is not None:
            mem.close()
        if not args.keep_patches:
            try:
                patch_file.unlink()
            except OSError:
                pass
        if input_path.exists():
            pass

    record_dicts = [r.to_dict() for r in records]
    ffx.write_jsonl(ffx_jsonl, record_dicts)
    final_target = summary.get("target")
    if final_target is None and summary.get("camera", {}).get("target"):
        final_target = summary["camera"]["target"]
    try:
        ffx_summary = annotate_records(root, profile, record_dicts, out_dir,
                                       map_id, final_target)
    except Exception as exc:  # annotation is best effort
        ffx_summary = {"error": str(exc)}
    summary["ffx_summary"] = ffx_summary
    summary["ffx"]["jsonl"] = str(ffx_jsonl)
    summary["ffx"]["annotated"] = str(out_dir / "ffx_annotated.txt")
    if summary.get("camera", {}).get("ok"):
        pass
    summary_path.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({k: summary[k] for k in
                      ("run", "map", "mission", "warp", "look_at", "ffx",
                       "camera_ready", "warp_counters", "probe_hits",
                       "camera", "shot", "errors") if k in summary}, indent=2))
    print(f"[out] {out_dir}", flush=True)
    if summary.get("exit_code"):
        return int(summary["exit_code"])
    if summary["errors"]:
        if any("mission id" in e for e in summary["errors"]):
            return 4
        if args.dump_ffx and summary["ffx"]["count"] == 0:
            return 5
        if args.look_at and not summary["camera"].get("ok"):
            return 6
        return 1
    if args.dump_ffx and summary["ffx"]["count"] == 0:
        return 5
    return 0


def annotate_only(root, profile, args) -> int:
    records = ffx.load_jsonl(args.input)
    out_dir = (Path(args.out_dir) if args.out_dir
               else root / profile["paths"]["runs"] / "annotate").resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    map_id = parse_map(args.map or "m4000")
    target = None
    if args.look_at:
        _, target, _ = parts.resolve_look_at(args.look_at, root, profile, map_id)
    summary = annotate_records(root, profile, records, out_dir, map_id, target)
    print(json.dumps(summary, indent=2))
    print(f"[out] {out_dir}", flush=True)
    return 0


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--game", default=DEFAULT_GAME)
    ap.add_argument("--map", help="m####, #### or ch####")
    ap.add_argument("--mission", type=int,
                    help="explicit mission XML id (disables map warp)")
    ap.add_argument("--look-at", help="X,Y,Z | ac | part:<model>[:<instance>]")
    ap.add_argument("--eye", help="explicit camera eye X,Y,Z")
    ap.add_argument("--dist", type=float, default=None,
                    help="camera distance from target (default profile 20)")
    ap.add_argument("--dump-ffx", action="store_true")
    ap.add_argument("--annotate", action="store_true")
    ap.add_argument("--input", help="existing ffx.jsonl for --annotate")
    ap.add_argument("--shot", help="guest PNG path")
    ap.add_argument("--test", default="actest1", help="AC Test layer name")
    ap.add_argument("--recipe", help="drive.py recipe override (demo only)")
    ap.add_argument("--title-timeout", type=float, default=None)
    ap.add_argument("--mission-timeout", type=float, default=None)
    ap.add_argument("--out-dir", help="run directory name")
    ap.add_argument("--hash", default=None, help="expected module hash")
    ap.add_argument("--keep-patches", action="store_true")
    ap.add_argument("--verify", "--validate-only", action="store_true",
                    dest="verify")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--print-patch", action="store_true")
    return ap


def main() -> int:
    args = build_parser().parse_args()
    profile = load_profile(args.game)
    if args.dist is None:
        args.dist = float(profile["camera"]["default_dist"])
    if args.title_timeout is None:
        args.title_timeout = float(profile["nav"]["title_timeout"])
    if args.mission_timeout is None:
        args.mission_timeout = float(profile["nav"]["mission_timeout"])
    if args.hash and args.hash.lower() != profile["module_hash"].lower():
        print(f"error: --hash {args.hash} does not match profile "
              f"{profile['module_hash']}", file=sys.stderr)
        return 2
    if args.annotate and args.input:
        try:
            return annotate_only(ROOT, profile, args)
        except Exception as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 1
    try:
        return run_probe(ROOT, profile, args)
    except ProbeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return exc.code
    except Exception as exc:  # pragma: no cover - CLI guard
        print(f"error: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
