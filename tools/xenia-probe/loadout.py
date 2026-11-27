#!/usr/bin/env python3
"""Weapon-arm loadout override for AC Test probes.

Hooks the assembly part-slot getter ``0x824556F0`` (``GetPartSlot(obj, slot)``)
and rewrites the returned slot descriptor for the arms slot (2) from the
starter arm id (410) to the requested weapon arm id.  The part record pointer
is at ``descriptor+8``; the 360 tests ``record+0x13C == 1`` for
``is_weapon_arm`` and reads the embedded weapon id at ``record+0xF4``
(``0x824793C8`` and ``0x82479668``).

The same hook suppresses every category-10 (weapon) slot record while the
override is active.  A weapon-arm AC must not also carry the starter L/R hand
or bay weapons: the 360 sortie loader would create them and the weapon arm's
embedded weapon would conflict with two live hand guns.  The category byte is
at ``record+0xC`` (checked by ``0x82470070``); zeroing the slot id, category and
embedded weapon id makes the loader skip the slot without disturbing the
head/core/arms/legs/fcs/generator/booster records.

The cave is ABI-safe (0x160 frame, LR/CR/volatiles saved) and re-implements
the original two tail branches:
  slot == -1 -> 0x82459E68 (global part table)
  otherwise  -> 0x82454DA0 (assembly slot dispatcher)

The mutation is gated by the ring header ``+0x5C`` word. The probe driver
leaves it zero through the menu (so title/garage previews see the starter
design) and writes 1 with WriteProcessMemory after observing the
``actest_sortie_load`` probe. ``+0x5C`` is the unused warp-mode word for the
m4000 AC Test run.
"""
from __future__ import annotations

from ppcgen import Cave

HOOK_ADDR = 0x824556F0
HOOK_WORD = 0x7C8B0774  # extsb r11, r4
GATE_FLAG_OFFSET = 0x5C
STARTER_ARM_ID = 410
ARM_WEAPON_OFFSET = 2000
SLOT_ARMS = 2
CATEGORY_WEAPON = 10


def weapon_id_for(arm_id: int) -> int:
    """The 360 embeds the weapon-arm gun at arm+2000 (9420 for am7420)."""
    if not 7000 <= arm_id <= 7699:
        raise ValueError(f"weapon arm id out of range: {arm_id}")
    return arm_id + ARM_WEAPON_OFFSET


def build_cave(base: int, arm_id: int, *,
               starter_arm: int = STARTER_ARM_ID,
               gate_addr: int = 0x8380345C) -> list[int]:
    weapon_id = weapon_id_for(arm_id)
    c = Cave(base)
    c.stwu(1, 1, -0x160)
    for reg, off in ((0, 0x10), (10, 0x18), (11, 0x20), (12, 0x28)):
        c.std(reg, 1, off)
    c.mfcr(0)
    c.stw(0, 1, 0x30)
    c.mflr(0)
    c.std(0, 1, 0x38)
    c.std(3, 1, 0x40)
    c.std(4, 1, 0x48)

    # Original dispatch: r4 is a sign-extended slot index.
    c.extsb(11, 4)
    c.cmpwi(11, -1)
    c.beq("global")
    c.bl(0x82454DA0)
    c.b("after")
    c.mark("global")
    c.bl(0x82459E68)
    c.mark("after")
    c.std(3, 1, 0x50)

    # Only rewrite while the AC Test sortie loader has signalled its turn.
    c.lis(11, gate_addr >> 16)
    c.ori(11, 11, gate_addr & 0xFFFF)
    c.lwz(12, 11, 0)
    c.cmpwi(12, 0)
    c.beq("exit")

    # Arms slot (2): starter arm 410 -> weapon arm.
    c.lwz(4, 1, 0x48)
    c.extsb(11, 4)
    c.cmpwi(11, SLOT_ARMS)
    c.bne("check_weapon")
    c.lwz(10, 3, 8)
    c.cmpwi(10, 0)
    c.beq("check_weapon")
    c.lhz(11, 10, 0)
    c.cmpwi(11, starter_arm)
    c.beq("do_arm")
    c.lhz(11, 10, 4)
    c.cmpwi(11, starter_arm)
    c.bne("check_weapon")
    c.mark("do_arm")
    c.li(11, arm_id)
    c.sth(11, 3, 0)       # descriptor slot id
    c.sth(11, 10, 0)      # record id
    c.sth(11, 10, 4)      # key-builder id (0x8247E090 reads +4)
    c.li(11, 1)
    c.stb(11, 10, 0x13C)  # is_weapon_arm (0x824793C8)
    c.li(11, weapon_id)
    c.sth(11, 10, 0xF4)   # embedded weapon id (0x82479668)

    # Suppress every category-10 slot in this assembly.
    c.mark("check_weapon")
    c.lwz(3, 1, 0x50)
    c.lwz(10, 3, 8)
    c.cmpwi(10, 0)
    c.beq("exit")
    c.lbz(11, 10, 0xC)
    c.cmpwi(11, CATEGORY_WEAPON)
    c.bne("exit")
    c.li(11, 0)
    c.sth(11, 10, 0)
    c.sth(11, 10, 4)
    c.stb(11, 10, 0xC)
    c.sth(11, 10, 0xF4)

    c.mark("exit")
    # Probe evidence at the final blr: slot, descriptor, record, id, category.
    c.lwz(5, 1, 0x48)
    c.lwz(6, 1, 0x50)
    c.lwz(6, 6, 8)
    c.cmpwi(6, 0)
    c.beq("probe_null")
    c.lhz(7, 6, 4)
    c.lbz(8, 6, 0xC)
    c.b("probe_done")
    c.mark("probe_null")
    c.li(7, 0)
    c.li(8, 0)
    c.mark("probe_done")

    c.lwz(0, 1, 0x30)
    c.mtcrf(0xFF, 0)
    c.ld(0, 1, 0x38)
    c.mtlr(0)
    for reg, off in ((0, 0x10), (10, 0x18), (11, 0x20), (12, 0x28)):
        c.ld(reg, 1, off)
    c.addi(1, 1, 0x160)
    c.blr()
    return c.finish()
