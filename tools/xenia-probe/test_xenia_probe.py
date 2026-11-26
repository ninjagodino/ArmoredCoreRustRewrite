#!/usr/bin/env python3
"""Unit tests for tools/xenia-probe (no Xenia launch, no game input).

Run:
    py -m unittest tools\\xenia-probe\\test_xenia_probe.py -v
"""
from __future__ import annotations

import json
import math
import struct
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

import ffx  # noqa: E402
import mapwarp  # noqa: E402
import parts  # noqa: E402
import patchgen  # noqa: E402
import ppcgen  # noqa: E402
import ring  # noqa: E402
import validate  # noqa: E402
import xenia_probe  # noqa: E402

PROFILE = tomllib.loads((HERE / "profiles" / "vd.toml").read_text(encoding="utf-8"))
X360DIS = ROOT / PROFILE["paths"]["x360dis"]
PE = validate.PE(ROOT / PROFILE["paths"]["pe"])


def decode(words: list[int], base: int) -> str:
    out = subprocess.run(
        [sys.executable, str(X360DIS), "--words",
         *[f"0x{w:08x}" for w in words], "--base", f"0x{base:08x}"],
        cwd=str(ROOT), capture_output=True, text=True, timeout=60,
        errors="replace")
    if out.returncode != 0:
        raise AssertionError(out.stderr or out.stdout)
    return out.stdout


class TestPPCEncoders(unittest.TestCase):
    def test_scalar_encodings(self):
        c = ppcgen.Cave(0x8260E290)
        c.stwu(1, 1, -0x160)
        c.std(0, 1, 0x10)
        c.ld(0, 1, 0x10)
        c.mfcr(0)
        c.mtcrf(0xFF, 0)
        c.lwarx(12, 0, 10)
        c.stwcx_dot(0, 0, 10)
        c.sync()
        words = c.finish()
        self.assertEqual(words[0], 0x9421FEA0)
        self.assertEqual(words[1], 0xF8010010)
        self.assertEqual(words[2], 0xE8010010)
        self.assertEqual(words[3], 0x7C000026)
        self.assertEqual(words[4], 0x7C0FF120)
        self.assertEqual(words[5], 0x7D805028)
        self.assertEqual(words[6], 0x7C00512D)

    def test_branch_range_assertion(self):
        self.assertEqual(ppcgen.branch_word(0x82B88A78, 0x8260E290) & 0xFC000003,
                         0x48000000)
        with self.assertRaises(ppcgen.AsmError):
            ppcgen.branch_word(0x82000000, 0x84000000)
        c = ppcgen.Cave(0x8260E290)
        c.mark("loop")
        c.nop()
        c.bne("loop")
        c.finish()

    def test_labels_and_decode(self):
        c = ppcgen.Cave(0x8260E700)
        c.stwu(1, 1, -0x160)
        c.mark("retry")
        c.addi(10, 10, -1)
        c.bne_minus("retry")
        c.addi(1, 1, 0x160)
        c.blr()
        text = decode(c.finish(), c.base)
        self.assertIn("stwu", text)
        self.assertIn("bne", text)
        self.assertIn("blr", text)


class TestCaves(unittest.TestCase):
    def test_ffx_cave_fits_and_decodes(self):
        words = ffx.build_ffx_cave(0x8260E290, 0x83803400, 1024)
        self.assertLessEqual(len(words) * 4, 0x200)
        text = decode(words, 0x8260E290)
        self.assertIn("stwu", text)
        self.assertIn("lwarx", text)
        self.assertIn("stwcx.", text)
        self.assertIn("mflr", text)
        self.assertNotIn(".word 0x", text)
        self.assertEqual(words[-1] & 0xFC000003,
                         ppcgen.branch_word(0x8260E290 + (len(words) - 1) * 4,
                                            0x82B88A7C) & 0xFC000003)

    def test_camera_cave_fits_and_decodes(self):
        words = ring.build_camera_cave(0x8260E490, 0x83803400,
                                       PROFILE["ring"]["offsets"])
        self.assertLessEqual(len(words) * 4, 0x200)
        text = decode(words, 0x8260E490)
        self.assertIn("sync", text)
        self.assertIn("lwz", text)
        self.assertIn("0x20(r31)", text)   # CamFx shown eye
        self.assertIn("0x30(r31)", text)   # CamFx shown target
        self.assertIn("0x60(r30)", text)   # camera-block clip flag
        self.assertIn("addi     r6, r31, 0x20", text)
        self.assertIn("82bfb33c", text)
        self.assertNotIn(".word 0x", text)

    def test_mapwarp_cave_fits_and_decodes(self):
        words = mapwarp.build_cave(0x8260E700, 0x83803400, "m3120", 1)
        self.assertLessEqual(len(words) * 4, 0x200)
        text = decode(words, 0x8260E700)
        self.assertIn("blr", text)
        self.assertIn("stb", text)
        self.assertIn("stw", text)
        # The hook replaces the formatter's final blr; the cave is a leaf and
        # must not build a frame or save volatile registers.
        self.assertNotIn("stwu", text)
        self.assertNotIn("mfcr", text)
        self.assertNotIn(".word 0x", text)

    def test_mapwarp_target_validation(self):
        with self.assertRaises(ValueError):
            mapwarp.build_cave(0x8260E700, 0x83803400, "m31200", 1)
        with self.assertRaises(ValueError):
            mapwarp.build_cave(0x8260E700, 0x83803400, "x3120", 1)


class TestPatch(unittest.TestCase):
    def test_patch_toml_shape(self):
        spec, info = xenia_probe.make_patch(
            ROOT, PROFILE, mission_id=6000, map_id=3120, warp_mode=1,
            dump_ffx=True)
        text = patchgen.render_patch(spec)
        data = tomllib.loads(text)
        self.assertEqual(data["title_id"], "4E4D0864")
        self.assertEqual(data["hash"], PROFILE["module_hash"])
        self.assertEqual(len(data["patch"]), 1)
        patch = data["patch"][0]
        self.assertTrue(patch["is_enabled"])
        self.assertEqual(len(patch["be32"]), 31)
        self.assertEqual(len(patch["array"]), 3)
        self.assertLessEqual(info["cave_sizes"]["mapwarp"], 0x200)
        self.assertGreater(info["cave_sizes"]["mapwarp"], 0)
        addresses = {entry["address"] for entry in patch["be32"]}
        self.assertIn(int(PROFILE["mission"]["getter"]), addresses)
        self.assertIn(int(PROFILE["ring"]["base"]) + 0x08, addresses)
        self.assertIn(int(PROFILE["ring"]["base"])
                      + int(PROFILE["ring"]["offsets"]["warp_calls"]), addresses)

    def test_probe_lines_use_cave_exits(self):
        _, info = xenia_probe.make_patch(
            ROOT, PROFILE, mission_id=6000, map_id=3120, warp_mode=1,
            dump_ffx=True)
        lines = xenia_probe.probe_lines(PROFILE, info)
        exits = info["cave_exits"]
        self.assertTrue(any(f"0x{exits['ffx']:08X}" in line for line in lines))
        self.assertTrue(any(f"0x{exits['camera']:08X}" in line for line in lines))
        # Mapwarp is proven by the header counters, not a per-call probe.
        self.assertFalse(any("warp_cave_exit" in line for line in lines))

    def test_patch_toml_round_trip_file(self):
        spec, _ = xenia_probe.make_patch(
            ROOT, PROFILE, mission_id=3120, map_id=3120, warp_mode=0)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "4E4D0864_test.patch.toml"
            patchgen.write_patch(path, spec)
            data = tomllib.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(data["patch"][0]["be32"][0]["value"], 0x38600C30)


class TestStaticValidation(unittest.TestCase):
    def test_hook_words_match_profile(self):
        for name, hook in PROFILE["hooks"].items():
            self.assertEqual(PE.word(int(hook["addr"])), int(hook["word"]),
                             f"{name} hook word changed")

    def test_flat_mapping(self):
        # The flat image maps guest VA directly; the section raw pointers have
        # a header gap and must not be used.
        self.assertEqual(PE.read(0x82B88A78, 4), b"\x7d\x88\x02\xa6")
        self.assertEqual(PE.section_for(0x8260E290).name, ".text")
        self.assertEqual(PE.section_for(0x83803400).name, ".data")

    def test_validate_without_index(self):
        result = validate.validate(PROFILE, ROOT, scan_index_refs=False)
        self.assertTrue(result["ok"], result["errors"])
        self.assertEqual(result["ring"]["zero"], True)

    def test_scan_index_semantics(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "calls.csv").write_text(
                "caller,site,callee,kind\n"
                "0x8260e290,0x8260e294,0x8326e3d8,call\n"
                "0x82000000,0x82000004,0x8260e290,call\n",
                encoding="utf-8")
            (root / "functions.csv").write_text(
                "start,end\n0x8260e290,0x8260e700\n", encoding="utf-8")
            hits = validate.scan_index(root, {"ffx": (0x8260E290, 0x8260E490)},
                                       set())
            self.assertEqual(len(hits["ffx"]), 1)
            self.assertIn("0x8260e290", hits["ffx"][0])


class TestRing(unittest.TestCase):
    class FakeMem:
        def __init__(self):
            self.data = {}

        def read(self, addr, size):
            out = bytearray()
            for i in range(size):
                out.append(self.data.get(addr + i, 0))
            return bytes(out)

        def u32(self, addr):
            return struct.unpack(">I", self.read(addr, 4))[0]

        def write(self, addr, data):
            for i, value in enumerate(data):
                self.data[addr + i] = value
            return True

        def write_u32(self, addr, value):
            return self.write(addr, struct.pack(">I", value & 0xFFFFFFFF))

    def test_header_roundtrip(self):
        offsets = PROFILE["ring"]["offsets"]
        base = 0x83803400
        buf = bytearray(0x70)
        struct.pack_into(">I", buf, offsets["magic"], PROFILE["ring"]["magic"])
        struct.pack_into(">I", buf, offsets["write_index"], 7)
        struct.pack_into(">I", buf, offsets["cam_frame"], 42)
        struct.pack_into(">3f", buf, offsets["cam_read_eye"], 1.0, 2.0, 3.0)
        struct.pack_into(">I", buf, offsets["warp_calls"], 9)
        struct.pack_into(">I", buf, offsets["warp_map"], 3)
        struct.pack_into(">I", buf, offsets["warp_msb"], 2)
        header = ring.parse_header(bytes(buf), offsets)
        self.assertEqual(header["write_index"], 7)
        self.assertEqual(header["cam_frame"], 42)
        self.assertEqual(header["cam_read_eye"], (1.0, 2.0, 3.0))
        self.assertEqual(header["warp_calls"], 9)
        self.assertEqual(header["warp_map"], 3)
        self.assertEqual(header["warp_msb"], 2)

    def test_drain_and_drop(self):
        mem = self.FakeMem()
        base = 0x83803400
        offsets = PROFILE["ring"]["offsets"]
        # Write a header through the fake memory map.
        header = bytearray(0x60)
        struct.pack_into(">I", header, offsets["magic"], PROFILE["ring"]["magic"])
        struct.pack_into(">I", header, offsets["write_index"], 3)
        struct.pack_into(">I", header, offsets["ring_cap"], 4)
        struct.pack_into(">I", header, offsets["ring_recsz"], 24)
        mem.write(base, header)
        for i in range(3):
            rec = struct.pack(">IIIfff", 0x10 + i, 0x4184D430 + i,
                              0x82833D54, float(i), 1.0, 2.0)
            mem.write(base + 0x100 + i * 24, rec)
        drainer = ring.RingDrainer(mem, PROFILE, lag=0)
        records = drainer.drain(0, final=True)
        self.assertEqual(len(records), 3)
        self.assertEqual([r.ffx for r in records], [0x10, 0x11, 0x12])
        self.assertEqual(drainer.invalid, 0)

    def test_dropped_accounting(self):
        mem = self.FakeMem()
        base = 0x83803400
        offsets = PROFILE["ring"]["offsets"]
        header = bytearray(0x60)
        struct.pack_into(">I", header, offsets["magic"], PROFILE["ring"]["magic"])
        struct.pack_into(">I", header, offsets["write_index"], 10)
        struct.pack_into(">I", header, offsets["ring_cap"], 4)
        mem.write(base, header)
        for i in range(4):
            rec = struct.pack(">IIIfff", 0x20 + i, 0, 0x82833D54,
                              float(i), 0.0, 0.0)
            mem.write(base + 0x100 + i * 24, rec)
        drainer = ring.RingDrainer(mem, PROFILE, lag=0)
        drainer.cap = 4
        drainer.cursor = 0
        records = drainer.drain(0, final=True)
        self.assertEqual(len(records), 4)
        self.assertEqual(drainer.dropped, 6)

    def test_camera_handshake(self):
        class FakeDrainer:
            def __init__(self):
                self.frame = 0

            def camera_status(self):
                self.frame += 1
                return {"cam_frame": self.frame}

        mem = self.FakeMem()
        drainer = FakeDrainer()
        result = ring.apply_camera(mem, drainer, PROFILE, (1.0, 2.0, 3.0),
                                   (4.0, 5.0, 6.0))
        self.assertTrue(result["enabled"])
        offsets = PROFILE["ring"]["offsets"]
        base = int(PROFILE["ring"]["base"])
        self.assertEqual(struct.unpack(">3f", mem.read(base + offsets["cam_eye"], 12)),
                         (1.0, 2.0, 3.0))
        self.assertEqual(struct.unpack(">I", mem.read(base + offsets["cam_enable"], 4))[0], 1)
        self.assertGreater(drainer.frame, 2)

    def test_vector_math(self):
        self.assertAlmostEqual(ring.vec_dist((1, 2, 3), (4, 6, 3)), 5.0)
        self.assertAlmostEqual(ring.vec_norm((0, 0, -2))[2], -1.0)


class TestMisc(unittest.TestCase):
    def test_nav_gate_is_strict(self):
        self.assertEqual(PROFILE["nav"]["title_label"], "press_dialog")
        self.assertEqual(PROFILE["nav"]["mission_label"], "env_bgm")
        self.assertNotIn("title_fallback", PROFILE["nav"])
        self.assertNotIn("mission_fallback", PROFILE["nav"])
        self.assertGreater(PROFILE["nav"]["post_env_settle_ms"], 0)
        self.assertGreater(PROFILE["nav"]["camera_ready_timeout"], 0)

    def test_parse_look_at(self):
        self.assertEqual(parts.parse_look_at("1,2,3"), ("xyz", (1.0, 2.0, 3.0)))
        self.assertEqual(parts.parse_look_at("ac"), ("ac", None))
        self.assertEqual(parts.parse_look_at("part:o9341:2"),
                         ("part", ("o9341", "2")))
        self.assertEqual(parts.parse_look_at("o9341"),
                         ("part", ("o9341", None)))
        with self.assertRaises(ValueError):
            parts.parse_look_at("part:")

    def test_choose_mission_m3120_uses_disc_mission(self):
        args = type("A", (), {"mission": None, "test": "actest1"})()
        mission, warp, source = xenia_probe.choose_mission(ROOT, PROFILE, 3120, args)
        self.assertEqual((mission, warp, source), (3120, 0, "map_mission"))

    def test_choose_mission_without_xml_falls_back_to_warp(self):
        args = type("A", (), {"mission": None, "test": "actest1"})()
        mission, warp, source = xenia_probe.choose_mission(ROOT, PROFILE, 3304, args)
        self.assertEqual(mission, 6000)
        self.assertEqual(warp, 1)
        self.assertEqual(source, "ac_test_warp")

    def test_choose_mission_m4000(self):
        args = type("A", (), {"mission": None, "test": "actest1"})()
        mission, warp, source = xenia_probe.choose_mission(ROOT, PROFILE, 4000, args)
        self.assertEqual((mission, warp, source), (6000, 0, "ac_test"))

    def test_choose_mission_explicit(self):
        args = type("A", (), {"mission": 3120, "test": "actest1"})()
        self.assertEqual(xenia_probe.choose_mission(ROOT, PROFILE, 3120, args),
                         (3120, 0, "explicit"))

    def test_ffx_annotate(self):
        records = [
            {"n": 0, "ms": 1, "ffx": 0x10, "owner": 0, "lr": 0x82833D54,
             "x": 0.0, "y": 0.0, "z": 0.0, "dropped": 0, "write_index": 1},
        ]

        class FakeSymbols:
            def label(self, lr):
                return {"lr": lr, "site": lr - 4, "function": 0x82833D20,
                        "function_end": 0x82833D60, "name": "FUN_82833D20",
                        "offset": 0x34}

            def decode_slice(self, lo, hi):
                return "82833d50  bl 0x82b88a78"

        with tempfile.TemporaryDirectory() as tmp:
            summary = ffx.annotate(records, FakeSymbols(), tmp, target=(0, 0, 0))
            self.assertEqual(summary["count"], 1)
            self.assertTrue((Path(tmp) / "ffx_annotated.txt").exists())
            self.assertTrue((Path(tmp) / "ffx_summary.json").exists())
            self.assertEqual(summary["near"]["within_10m"], 1)

    def test_wait_event_timeout(self):
        class FakeLog:
            def __init__(self):
                self.events = []
                self.cursor = 0

            def poll(self):
                return None

        class FakeProc:
            def poll(self):
                return None

        xenia_probe.wait_event.start = time.monotonic()
        with self.assertRaises(xenia_probe.ProbeError) as ctx:
            xenia_probe.wait_event(FakeLog(), lambda e: False, 0.02, FakeProc(),
                                   "never", interval=0.005)
        self.assertEqual(ctx.exception.code, 7)


if __name__ == "__main__":
    unittest.main(verbosity=2)
