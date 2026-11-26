#!/usr/bin/env python3
"""Phase 0 live pass: scripted AC Test boot + probe/memory evidence."""

import argparse
import csv
import ctypes
import hashlib
import json
import struct
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
XENIA_BIN = ROOT / "external" / "xenia-canary" / "build" / "bin" / "Windows" / "Release"
XENIA_EXE = XENIA_BIN / "xenia_canary.exe"
ISO = ROOT / "armoredcoredumps" / "Armored Core - Verdict Day (USA)" / "Armored Core - Verdict Day (USA).iso"
PE = ROOT / "private" / "x360" / "vd" / "ACV2.pe"
INDEX = ROOT / "private" / "index"
DRIVE = ROOT / "tools" / "xenia" / "drive.py"
RECIPE = ROOT / "tools" / "xenia" / "recipes" / "ac_test.txt"
IMAGE_BASE = 0x82000000
DATA_BASE = 0x836F0000
DATA_SIZE = 0x4A263C
RING_BASE = 0x83803400
RING_CAP = 0x4000
RING_SIZE = 0x100 + RING_CAP * 24
GUEST_MAX = 0x7FFFFFFEFFFF

PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_VM_READ = 0x0010
MEM_COMMIT = 0x1000


class MEMORY_BASIC_INFORMATION64(ctypes.Structure):
    _fields_ = [
        ("BaseAddress", ctypes.c_ulonglong),
        ("AllocationBase", ctypes.c_ulonglong),
        ("AllocationProtect", ctypes.c_ulong),
        ("__alignment1", ctypes.c_ulong),
        ("RegionSize", ctypes.c_ulonglong),
        ("State", ctypes.c_ulong),
        ("Protect", ctypes.c_ulong),
        ("Type", ctypes.c_ulong),
        ("__alignment2", ctypes.c_ulong),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
kernel32.OpenProcess.restype = ctypes.c_void_p
kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
kernel32.VirtualQueryEx.argtypes = [ctypes.c_void_p, ctypes.c_ulonglong,
                                   ctypes.POINTER(MEMORY_BASIC_INFORMATION64),
                                   ctypes.c_size_t]
kernel32.VirtualQueryEx.restype = ctypes.c_size_t
kernel32.ReadProcessMemory.argtypes = [ctypes.c_void_p, ctypes.c_ulonglong,
                                       ctypes.c_void_p, ctypes.c_size_t,
                                       ctypes.POINTER(ctypes.c_size_t)]
kernel32.ReadProcessMemory.restype = ctypes.c_int


def read_mem(handle, address, size):
    buf = ctypes.create_string_buffer(size)
    got = ctypes.c_size_t(0)
    ok = kernel32.ReadProcessMemory(handle, address, buf, size, ctypes.byref(got))
    if not ok or got.value != size:
        return None
    return buf.raw


def find_guest_base(pid):
    handle = kernel32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, pid)
    if not handle:
        return None, None
    try:
        rdata0 = PE.read_bytes()[0x400:0x420]
    except OSError:
        rdata0 = b""
    address = 0
    while address < GUEST_MAX:
        mbi = MEMORY_BASIC_INFORMATION64()
        if not kernel32.VirtualQueryEx(handle, address, ctypes.byref(mbi), ctypes.sizeof(mbi)):
            break
        candidate = mbi.BaseAddress
        if mbi.State == MEM_COMMIT and mbi.RegionSize:
            head_addr = candidate + IMAGE_BASE
            if head_addr + 0x40 < GUEST_MAX:
                head = read_mem(handle, head_addr, 0x40)
                if head and head[:2] == b"MZ":
                    e_lfanew = struct.unpack_from("<I", head, 0x3C)[0]
                    if 0 < e_lfanew < 0x1000:
                        if read_mem(handle, head_addr + e_lfanew, 4) == b"PE\0\0":
                            return handle, candidate
            if rdata0:
                if read_mem(handle, candidate + 0x82000400, len(rdata0)) == rdata0:
                    return handle, candidate
        next_address = candidate + mbi.RegionSize
        if next_address <= address:
            break
        address = next_address
    kernel32.CloseHandle(handle)
    return None, None


class ProbeLog:
    def __init__(self, path):
        self.path = Path(path)
        self.fh = None
        self.events = []
        self.cursor = 0

    def poll(self):
        if self.fh is None:
            if not self.path.exists():
                return
            self.fh = open(self.path, "r", encoding="utf-8", errors="replace")
        while True:
            line = self.fh.readline()
            if not line:
                break
            if not line.endswith("\n"):
                self.fh.seek(self.fh.tell() - len(line))
                break
            try:
                self.events.append(json.loads(line))
            except json.JSONDecodeError:
                continue

    def wait(self, predicate, timeout, proc, description):
        deadline = time.monotonic() + timeout
        while True:
            self.poll()
            while self.cursor < len(self.events):
                event = self.events[self.cursor]
                self.cursor += 1
                if predicate(event):
                    print(f"[state] {description} at {event.get('ms')} ms: "
                          f"{event.get('label')} {event.get('r5', '')}", flush=True)
                    return event
            if proc.poll() is not None:
                raise RuntimeError(f"Xenia exited while waiting for {description}")
            if time.monotonic() >= deadline:
                raise TimeoutError(f"timeout ({timeout:.0f}s) waiting for {description}")
            time.sleep(0.05)


def build_probes():
    return [
        "# Phase 0: scripted AC Test boot, FFX ABI, cave pool, ring integrity.",
        "0x82800128 env_bgm max=1 r4",
        "0x82B29040 press_dialog max=1 r3",
        "0x82B292C8 press_scene max=2 r4 r5",
        "0x82B28DB8 gameboot_layout max=2 r3",
        "0x825DCE60 boot_subscene max=2 r3",
        "0x8268D138 scene_req max=1000 r4",
        "0x825DBDC8 actest_sortie_create max=4 r3 r4",
        "0x825DBCD8 actest_load_step max=4 r3",
        "0x825DCB68 actest_sortie_load max=1 r3",
        "0x82628E90 actest_menu_ctor max=4 r3 r4",
        "0x82B88A78 ffx_spawn max=120 after=env_bgm r3 r4 r5 r6 "
        "[r4+0x10]:f32 [r4+0x14]:f32 [r4+0x18]:f32 "
        "[r4+0x30]:f32 [r4+0x34]:u32 [r4+0x38]:u32 [r4+0x3C]:u32 "
        "[r4+0x40]:u32 [r4+0x44]:u32 [r4+0x48]:u32",
        "0x82B88D08 ffx_set max=120 after=env_bgm r3 r4 r5 r6 f1 "
        "[r3+0x3C]:u32 [r3+0x40]:u32 [r3+0x44]:u32 [r3+0x48]:u32 "
        "[r3+0x50]:f32",
        "0x8289AD60 ffx_tae max=120 after=env_bgm r3 r4 r10 r11 "
        "[r11+0x0]:u32 [r11+0x4]:u32 [r11+0x8]:u32 [r11+0xC]:u32 "
        "[r4+0x0]:u32 [r4+0x4]:u32 [r4+0x8]:u32 [r4+0xC]:u32",
        "0x8260E290 cave_e290 max=1 r3 r4",
        "0x8260E700 cave_e700 max=1 r4",
    ]


def load_data_refs():
    refs = set()
    for path in sorted(INDEX.glob("*.csv")):
        with open(path, newline="", encoding="utf-8", errors="replace") as f:
            for row in csv.reader(f):
                for cell in row:
                    if len(cell) < 3 or not cell.startswith("0x"):
                        continue
                    try:
                        address = int(cell, 16)
                    except ValueError:
                        continue
                    if DATA_BASE <= address < DATA_BASE + DATA_SIZE:
                        refs.add(address)
    return refs


def choose_ring(handle, host_base, refs):
    data = read_mem(handle, host_base + DATA_BASE, DATA_SIZE)
    if data is None:
        raise RuntimeError("could not read .data for ring selection")
    runs = []
    i = 0
    while i < len(data):
        if data[i] != 0:
            i += 1
            continue
        j = i + 1
        while j < len(data) and data[j] == 0:
            j += 1
        runs.append((DATA_BASE + i, DATA_BASE + j))
        i = j
    data_refs = sorted(refs)
    for cap in (0x8000, 0x4000, 0x2000, 0x1000, 0x800, 0x400, 0x200, 0x100, 0x80, 0x40):
        size = 0x100 + cap * 24
        candidates = []
        for lo, hi in runs:
            if hi - lo < size:
                continue
            cuts = [lo]
            cuts.extend(a for a in data_refs if lo < a < hi)
            cuts.append(hi)
            for a, b in zip(cuts, cuts[1:]):
                if b - a < size:
                    continue
                base = (a + (b - a - size) // 2) & ~0xFF
                if base < a:
                    base = (a + 0xFF) & ~0xFF
                if base + size > b:
                    continue
                clearance = min(base - a, b - (base + size))
                refs_in_span = sum(1 for r in data_refs if base <= r < base + size)
                if refs_in_span:
                    continue
                candidates.append((clearance, base, a, b))
        if candidates:
            clearance, base, lo, hi = max(candidates)
            return {"cap": cap, "size": size, "base": base,
                    "zero_run": [lo, hi], "clearance": clearance,
                    "data_refs": len(data_refs), "data_zero_runs": len(runs),
                    "refs_in_span": 0}
    raise RuntimeError("no static-clean runtime-zero ring range found")


def snapshot_ring(handle, host_base, label, snapshots):
    guest = host_base + RING_BASE
    full = read_mem(handle, guest, RING_SIZE)
    if full is None:
        snapshots.append({"label": label, "ok": False, "error": "RPM failed"})
        return None
    nonzero = sum(1 for b in full if b)
    points = []
    for name, off in (("start", 0), ("mid", RING_SIZE // 2 - 0x80),
                      ("end", RING_SIZE - 0x100)):
        part = full[off:off + 0x100]
        points.append({"point": name, "offset": off,
                       "sha256": hashlib.sha256(part).hexdigest(),
                       "nonzero": sum(1 for b in part if b)})
    result = {"label": label, "ok": nonzero == 0, "nonzero": nonzero,
              "sha256": hashlib.sha256(full).hexdigest(), "points": points}
    snapshots.append(result)
    print(f"[ring] {label}: nonzero={nonzero} points={points}", flush=True)
    return result


def terminate(proc):
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def main():
    global RING_BASE, RING_CAP, RING_SIZE
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "private" / "xenia" / "phase0_live"))
    ap.add_argument("--title-timeout", type=float, default=60.0)
    ap.add_argument("--mission-timeout", type=float, default=190.0)
    args = ap.parse_args()

    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    shot_path = ROOT / "private" / "xenia" / "shots" / "ac_test_start.png"
    for path in (out / "probes.txt", out / "phase0.jsonl", out / "phase0.xenia.log",
                 out / "phase0.stdout.log", out / "driver.log", out / "summary.json",
                 out / "phase0_ac_test.png", shot_path):
        if path.exists():
            path.unlink()

    if not XENIA_EXE.exists():
        raise SystemExit(f"missing Xenia: {XENIA_EXE}")
    if not ISO.exists():
        raise SystemExit(f"missing ISO: {ISO}")

    probes = out / "probes.txt"
    probes.write_text("\n".join(build_probes()) + "\n", encoding="utf-8")
    jsonl = out / "phase0.jsonl"
    xlog = out / "phase0.xenia.log"
    stdout_log = out / "phase0.stdout.log"
    input_file = out / "commands.input"
    input_file.write_text("", encoding="utf-8")

    cmd = [
        str(XENIA_EXE),
        f"--acvd_probes={probes}",
        f"--acvd_probe_log={jsonl}",
        f"--acvd_input={input_file}",
        "--acvd_input_port=1",
        "--protect_zero=false",
        "--readback_resolve=full",
        "--apply_patches=false",
        f"--log_file={xlog}",
        str(ISO),
    ]
    with open(stdout_log, "wb") as log:
        proc = subprocess.Popen(cmd, cwd=str(XENIA_BIN), stdout=log, stderr=subprocess.STDOUT)
    print(f"[launch] pid={proc.pid} out={out}", flush=True)

    driver = None
    log_read = ProbeLog(jsonl)
    snapshots = []
    handle = None
    host_base = None
    summary = {"run": out.name, "ring": {"snapshots": snapshots}}
    try:
        driver_log = open(out / "driver.log", "wb")
        driver = subprocess.Popen([sys.executable, str(DRIVE), str(input_file), str(RECIPE)],
                                  cwd=str(ROOT), stdout=driver_log, stderr=subprocess.STDOUT)
        log_read.wait(lambda e: e.get("label") in ("press_dialog", "gameboot_layout"),
                      args.title_timeout, proc, "title")
        for _ in range(100):
            handle, host_base = find_guest_base(proc.pid)
            if handle is not None:
                break
            time.sleep(0.1)
        if handle is None:
            raise RuntimeError("guest mapping not found")

        refs = load_data_refs()
        selected = choose_ring(handle, host_base, refs)
        RING_BASE = selected["base"]
        RING_CAP = selected["cap"]
        RING_SIZE = selected["size"]
        summary["ring"] = {
            "base": f"0x{RING_BASE:08X}",
            "cap": RING_CAP,
            "size": f"0x{RING_SIZE:X}",
            "range": f"0x{RING_BASE:08X}..0x{RING_BASE + RING_SIZE:08X}",
            "section": ".data",
            "selection": {k: (f"0x{v:08X}" if k == "base" else
                              [f"0x{x:08X}" for x in v] if k == "zero_run" else v)
                          for k, v in selected.items()},
            "snapshots": snapshots,
        }
        print(f"[ring] selected 0x{RING_BASE:08X} cap={RING_CAP} "
              f"size=0x{RING_SIZE:X} refs_in_span={selected['refs_in_span']}", flush=True)
        snapshot_ring(handle, host_base, "title", snapshots)

        log_read.wait(lambda e: e.get("label") == "env_bgm",
                      args.mission_timeout, proc, "AC TEST mission")
        time.sleep(3.0)
        driver.wait(timeout=60)
        if driver.returncode != 0:
            raise RuntimeError(f"recipe failed with exit code {driver.returncode}")
        if not shot_path.exists() or shot_path.stat().st_size == 0:
            raise RuntimeError("recipe did not write ac_test_start.png")
        snapshot_ring(handle, host_base, "mission", snapshots)
        target = out / "phase0_ac_test.png"
        target.write_bytes(shot_path.read_bytes())
        summary["shot"] = str(target)
        summary["pass_pre"] = True
    except Exception as exc:
        summary["pass_pre"] = False
        summary["error"] = f"{type(exc).__name__}: {exc}"
        print(f"[fail] {summary['error']}", flush=True)
    finally:
        if driver is not None and driver.poll() is None:
            driver.terminate()
            try:
                driver.wait(timeout=5)
            except subprocess.TimeoutExpired:
                driver.kill()
        terminate(proc)
        if handle:
            kernel32.CloseHandle(handle)

    log_read.poll()
    counts = {}
    samples = {}
    ffx_ids = {}
    for event in log_read.events:
        label = event.get("label")
        counts[label] = counts.get(label, 0) + 1
        if label in ("ffx_spawn", "ffx_set", "ffx_tae"):
            samples.setdefault(label, []).append(event)
            for key, value in event.items():
                if key == "r4" and label == "ffx_set":
                    ffx_ids.setdefault(label, set()).add(value)
                if key == "r4" and label == "ffx_spawn":
                    ffx_ids.setdefault("ffx_spawn_request", set()).add(value)

    summary["hits"] = counts
    summary["cave_e290_hits"] = counts.get("cave_e290", 0)
    summary["cave_e700_hits"] = counts.get("cave_e700", 0)
    summary["env_bgm_hits"] = counts.get("env_bgm", 0)
    summary["ffx_counts"] = {k: counts.get(k, 0)
                             for k in ("ffx_spawn", "ffx_set", "ffx_tae")}
    summary["ffx_samples"] = {k: v[:5] for k, v in samples.items()}
    summary["ffx_ids"] = {k: sorted(v) for k, v in ffx_ids.items()}
    summary["ring_untouched"] = bool(snapshots) and all(s.get("ok") for s in snapshots)
    summary["cave_zero"] = summary["cave_e290_hits"] == 0 and summary["cave_e700_hits"] == 0
    summary["ffx_observed"] = all(summary["ffx_counts"].get(k, 0) > 0
                                 for k in ("ffx_spawn", "ffx_set", "ffx_tae"))
    summary["pass"] = bool(summary.get("pass_pre") and summary["ring_untouched"]
                           and summary["cave_zero"] and summary["ffx_observed"])
    if xlog.exists():
        text = xlog.read_text(encoding="utf-8", errors="replace")
        for line in text.splitlines():
            if "Module Hash:" in line:
                summary["module_hash"] = line.split("Module Hash:", 1)[1].strip()
                break
    (out / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2), flush=True)
    return 0 if summary["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
