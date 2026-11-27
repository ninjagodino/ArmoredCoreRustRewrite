# tools/xenia-probe: unlock-free map, FFX and camera probes

## Goal

Upgrade the agent-driven Xenia tooling so a single command can, with no campaign progress and no
manual play:

```powershell
xenia-probe --map m3330 --look-at part:o9330 --dump-ffx --shot private\xenia\shots\m3330.png
```

- load an arbitrary 360 mission/map by patching the AC TEST mission id in `ACV2.pe`,
- stream every runtime FFX spawn (`ffx_id`, `dummy_id`, `caller_lr`, `pos_xyz`) from guest RAM,
  including ambient map/object FFX spawned during map load, and annotate the callsites,
- aim the 360 camera at any world point or authored MSB part,
- capture a guest-frame PNG.

Target is the Xbox 360 build only: `ACV2.pe`, image base `0x82000000`, title `4E4D0864`, module
hash `03794C9BD0139BBF` (from the `Module Hash:` line of existing
`private\xenia\*.xenia.log`; `acv_model.xenia.log` shows a different hash and must not be used).

## Decisions already made

1. **Entry point**: AC TEST first. Patch the AcTestSortieScene mission-id getter
   `0x825DBB70`, but reach the mission through the AC Test **menu** (Lua scene 2008) with the
   state machine above. Do not jump straight to the sortie scene: its constructor
   `0x825DBC80` is pre-created by the menu constructor `0x82628E90` with the manager/mode/sortie
   info (`0x825DBDC8`), and a cold jump would skip the garage/save/system setup. Free Battle /
   Mission 00 is a later fallback, not v1.
2. **Camera**: a code cave hooked after `CamCtrl_update` writes eye/target, not racy external
   writes to the live camera object.
3. **Host implementation**: Python + `ctypes` under `tools/xenia-probe/` (`xenia_probe.py`,
   `state_nav.py`), plus a small `acvd-formats` example for disc/MSB queries. Extend
   `tools/xenia/run.ps1` only with `-ApplyPatches`. The existing probe patch/runner stays where it
   is; the new tool is the single entry point for these probes.
4. **FFX hook**: the central spawn `0x82B88A78`, not the TAE dispatcher alone.
5. **Cave/ring addresses**: verified at generation time from the PE image + `private/index`;
   never hardcoded silently.
6. **`--look-at` forms**: literal `X,Y,Z`, `ac`, and `part:<model>[:<instance>]`.
7. **`--map` forms**: `m3330`, `3330`, `ch3330` all mean mission id 3330; `m4000` defaults to
   the AC TEST mission `6000`, with `--mission 4000` / `--map ch4000` for regular m4000.
8. **FFX capture is armed at boot**: when `--dump-ffx` is set, the generated `.patch.toml`
   pre-writes `magic='AXPR'` (`0x41585052`) and `flags=1` (ring enabled) at startup (not after an
   `env_bgm` probe), and the host starts the ring drain immediately after process creation,
   polling every 15-20 ms. Ambient map/object FFX spawned during map load (e.g. `o9341` on
   `m3120`) is therefore never missed.
9. **Ring sizing and atomicity**: `--verify` prefers the largest zero/no-ref writable run in
   `.data`/`.bss` and sizes the ring at 1024 records (24 KB), then 512, 256, 128, and 64 only as
   fallbacks. File-zero bytes are necessary but not sufficient: Phase 0 rejected
   `0x83B80000..0x83B90000` because the index has 498 refs inside it, so every candidate must
   pass the same xref scan. The cave claims `write_index` slots with `lwarx`/`stwcx.` so concurrent
   spawns cannot corrupt slots; the host drains with a 16-slot lag and validates records.
10. **Cave ABI safety**: both caves allocate a 0x160-byte frame with `stwu r1,-0x160(r1)` and
    restore with `addi r1,r1,0x160` (the 0x160 frame clears the 288-byte PPC64 red zone), save
    and restore every clobbered GPR at `0x10(r1)+` with **64-bit `std`/`ld`** (8-byte aligned),
    save/restore the Condition Register (`mfcr`/`stw` then `lwz`/`mtcrf 0xFF`), and save/restore
    LR with `mflr`/`std` + `ld`/`mtlr`. No 32-bit `stw` GPR slots at `0x04/0x08/0x0C`. x/y/z are
    copied as raw 32-bit words with `lwz`/`stw` so no floating-point register is touched or saved.
    `write_index` is claimed with `lwarx`/`stwcx.`; the camera handshake uses the header
    `CAM_FRAME` counter (disable -> wait 2 ticks -> write -> enable).
11. **Cave self-test**: scalar cave words are decoded with `tools/x360dis/x360dis.py`, because
    `ppc_vmx128` only decodes primary opcodes 4/5/6 (VMX128/Altivec) and returns nothing for
    `stwu`, `lwz`, `stw`, `mflr`, `cmpwi`, `b`, etc. The encoder asserts branch displacement
    `abs(target - pc) < 0x02000000` before emitting a `b`.
12. **`--annotate`**: resolve every unique `caller_lr` in `ffx.jsonl` through `acvd-index` (function
    bounds) and `x360dis.py` (decoded PPC+VMX128 callsite slice); write `ffx_annotated.txt` and a
    machine-readable `ffx_summary.json`.
13. **Camera write ordering**: host WPM always writes `CAM_EYE` and `CAM_TARGET` before setting
    `CAM_ENABLE=1`, and clears `CAM_ENABLE=0` then waits two `CAM_FRAME` ticks before rewriting live
    coordinates, so the 30 Hz cave never observes a torn vector. `CAM_FRAME` lives in the ring
    header at `+0x50` and is incremented by the camera cave on every invocation.
14. **No blind menu timing (hard rule)**: the tool never drives the front end with fixed sleeps and
    unconditional button presses. Every press is gated on a live state read from a probe hit or
    guest RAM; a step that does not reach its expected state within 30 s aborts with a debug guest
    screenshot and the last observed state. Boot-straight-to-sortie is not the default: the normal
    flow is state-driven through the AC Test menu so save/system init always runs first.

## State-driven navigation (hard rule)

The old `ac_test.txt` recipe is demo-only and is **not** used by `xenia-probe` for navigation.
`tools/xenia-probe/state_nav.py` owns a small state machine:

| step | wait for state (probe) | action |
|---|---|---|
| boot | `0x82B28DB8` gameboot/press layout, or `0x82B292C8` press scene | press START |
| caution | next observed state change after START (caution dialog probe once identified in Phase 0) | press A once, re-read state |
| storage | dialog/scene state change after A | press A once, re-read state |
| basic guide | `0x825DBDC8`/`0x825DBCD8` AC Test menu construction not yet reached | press A |
| world menu | Lua scene request `0x8268D138` with `r4 = 2001` (`GarageSelectScene`) | press A (Workshop) |
| workshop | `0x825DBDC8` sortie scene pre-created / `@AcTestTop` state | press DOWN x5 only while the menu is live; re-check after each press |
| AC Test menu | `0x825DBCD8` / `0x825DBDC8` hit | press A (Start Test), then A (confirm) |
| mission | `0x82800128` `env_bgm`, or `0x825DCB68` `AcTestSortieScene_Load` | capture / fire / annotate |

Probe facts used by the state machine:

- `0x8268D138` is the Lua `Scr_SetNextScene` binding: `r3` manager, `r4` requested scene id,
  `f1/f2` fade times; the native core is `0x825E3BD8`. Scene 2008 is the AC Test menu
  (`Self:Scr_CreateScene(2008, "AcTestScene")` in `garagescene.lua:223`), and the garage script
  itself sets its first scene to 2001 (`garagescene.lua:226`).
- `0x82628E90` is the AC Test menu constructor. It pre-creates the sortie scene through
  `0x825DBDC8`, which calls the real `AcTestSortieScene` constructor `0x825DBC80` with the manager,
  mode and sortie info. `0x825DBCD8` is the sortie scene load step.
- `0x825DBD90` is **not** the sortie scene entry: it is a 2-instruction cleanup wrapper
  (`mflr/stwu`, call `0x823899F0`, return `this`). Do not hook or jump to it as a scene entry.
- `0x825DCB68` creates the `AcTestSortieScene_Load` object; `0x82800128` is the mission environment
  (`env_bgm`) boundary.

If a native dialog (caution/storage) has no probe yet, Phase 0 must identify one before the
state machine is accepted; a fixed wait is allowed only as a *settle* after a state hit, never as
the primary synchronization. The tool tails the probe `.jsonl` (and may read the scene manager's
current/next id from guest RAM) instead of blocking a shell for minutes.

## Phase 0 evidence status (locked so far)

- **`phase0_noop` did not hang.** The stock `ac_test.txt` control run reached AC TEST at
  121,211.971 ms (`env_bgm` hit), started `AcTestSortieScene_Load` and `Ac4SortieScene`, loaded
  `o0006`/`o2016`, and wrote `ac_test_start.png` at 13:24:31. The apparent "missing" shot was
  deleted when the next run was reset; the `.jsonl` and `phase0_noop_run.out` retain the hit.
  The later failures were our own long foreground reader hitting the harness timeout (which kills
  the background run), not a menu stall. State-driven navigation removes this failure mode.
- **Replacement ring is locked for the current schema.** `state_nav.py` scanned all 13
  `private/index/*.csv` (2,281,617 image addresses; 32,560 `.data` refs) and selected
  `0x83803400..0x83863500`, cap 16384, size `0x60100`, `refs_in_span=0`. Title snapshot after
  `gameboot_layout` and mission snapshot after `env_bgm` both read full-range nonzero 0;
  start/mid/end nonzero 0; full-range SHA-256 `e9f781ac...c72e7`. Static max
  `0x83792000..0x83852100` (cap 32768) also had 0 refs but title runtime nonzero 164,818, so it
  is rejected. `0x83B80000..0x83B90000` remains rejected by 498 indexed refs.
- **Cave 0-hit status is final for the live pass.** `0x8260E290` and `0x8260E700` each logged
  **0 hits** across full boot + AC TEST (`max=1`). `0x8260E980` remains excluded: direct tail
  caller `0x8260EBFC` in `0x8260EBE8`.
- **FFX ABI is empirically closed, except the planned `dummy_id` field.** `env_bgm` hit at
  121,189.012 ms. After it: 34 `ffx_spawn`, 120 `ffx_set` (cap), 42 `ffx_tae`. At spawn,
  `r5` is the FFX id (`0x57C` x2, `0xC5` x12, `0x62` x20), `r4` is the request, `r6=0` in all
  34, and `request+0x3C=0x4184D430` in all 34 (a dummy-object pointer, not an integer id).
  `pos_xyz` is `request+0x10/+0x14/+0x18` f32; e.g. id `0x57C` at lr `0x82833D54` reads
  `-326.187378 / 27.9178219 / 647.668823`. At setter, `r4` is the id (observed
  `0x64,0xC8,0xC9,0xCA,0xCB,0xCD,0xCE,0x12C,0x15E,0x190`), `r3` is the request, and
  `r3+0x40/+0x44` are begin/end pointers. At `0x8289AD60`, `r10=0` in all 42 hits and
  `r11`/`r4` are TAE event/payload pointers; this is a dispatcher, not a spawn/ABI site.
  **The planned `dummy_id` is not available as `r6` or `request+0x3C`; do not freeze the
 24-byte record's dummy word until a new probe dereferences the dummy object.**
- **Boot-straight-to-AC-TEST is not adopted.** `0x825DBD90` is a cleanup wrapper, not the sortie
  scene entry. The real sortie constructor is `0x825DBC80`, pre-created by the AC Test menu
  `0x82628E90` through `0x825DBDC8` with the manager/mode/sortie info; the Lua menu scene is 2008
  and the garage script sets 2001. Jumping straight to the sortie would skip the menu's
  manager/sortie-info setup and likely the garage/save init. Use state-driven navigation through
  the AC Test menu; a partial skip to scene 2008 after garage init can be investigated later.

## Verified starting facts

| address / item | fact | evidence |
|---|---|---|
| patch root | `<Xenia Release>\patches\4E4D0864*.patch.toml` | `portable.txt` in the Release dir; `--storage_root` defaults to the executable folder; `patch_db.cc` regex `^[A-Fa-f0-9]{8}.*\.patch\.toml$` |
| patch data types | `be8/16/32/64`, `f32/f64`, `string`, `u16string`, `array` (raw hex bytes) | `src/xenia/patcher/patch_db.h` |
| patch timing | after `LoadContinue`, before `Precompile` | `kernel_state.cc:658` |
| `0x825DBB70` | 8-byte leaf `li r3,6000`; vtable `0x8204B9B0 +0x4`; no direct callers | `functions.csv`, `imms.csv` (`li 6000`), `slots.csv` |
| mission XML | `0x82389800` formats `$(Data)\mission\ch%04d.xml` | `consts.csv` |
| map path | `0x82630DF0` formats `$(Data)\model\map\%s\%s_map.msb` | `consts.csv` |
| `0x82B88A78` | common FFX spawn, 46 direct callers; `r5` = FFX id; `[r4+0x30]` = scale; request ctor `0x82B88E88` sets `+0x30=1.0`, `+0x34/+0x38/+0x3C` ints | `calls.csv`, `fields.csv`, `fx_polish.jsonl` (`r5` = 0x57C/0xC5/0x62) |
| camera update | `CamCtrl_update 0x82860100` writes eye `cam+0x0C..0x18` and target `cam+0x1C..0x28`; stores at `0x82860374..0x8286039C`; original at `0x828603A0` is `lwz r0,0x84(r31)` | `camupd.c`, `fields.csv` rows 197149-197157 |
| camera object | `r31` is the camera controller in `CamCtrl_update`; eye/target block also used by `CamFx` | `camupd.c`, `camera_follow.csv` |
| camera hook word | `0x828603A0` is `lwz r0,0x84(r31)` = `0x801F0084` | `camupd.c`; confirm with `x360dis --range` |
| render device | `dev = [0x83A4E62C]`; view matrix `dev+0x3D0`, eye `dev+0x400`, projection `dev+0x4A0` | `map_env.csv` sky.depth, `sky_world.txt` |
| PPC64 red zone | 288 bytes (`0x120`) below SP; a `0x160` frame clears it | Xbox 360 PPC64 ABI |
| scalar decode | `ppc_vmx128` handles primary 4/5/6 only; `x360dis.py` falls back to capstone for scalar PPC | `tools/x360dis/x360dis.py:190` |
| function names | `acvd-index q func` prints only `START..END`; no symbol names in `private/index` | `tools/acvd-index/src/query.rs:91` |
| `.text` end | `0x836EE5C8`; last page `0x836EE000..0x836EF000` | `sheets/target.csv`; image size `0x1C50000` |
| highest referenced data | `0x83B92808`; zero refs above it through `0x83BFFFFF` | `private/index/consts.csv` scan |
| code cave candidate | `0x836EE600..0x836EF000` (executable padding, no xrefs); two 0x200-byte caves | `.text` end + index scan |
| ring candidate (rejected) | `0x83B80000..0x83B90000` is all `0x00` in the PE, but the index has **498 refs inside** it (e.g. `0x83B80234`, `0x83B83F20`, `0x83B8DA30`); nearest ref below is `0x83B7E2E7` | Phase 0 scan of `private/index/consts.csv` + PE byte read |
| ring candidate rule | verifier must relocate to a zero/no-ref writable run; file-zero alone is not evidence of free memory | Phase 0 finding |
| AC Test menu | Lua scene 2008 `AcTestScene` is registered by `garagescene.lua:223`; garage init requests scene 2001 | `private/lua360/script/garage/garagescene.lua`, `0x8268D138` |
| AC Test sortie scene | real constructor `0x825DBC80`; pre-created by the AC Test menu `0x82628E90` through `0x825DBDC8` (manager, mode, sortie info); load step `0x825DBCD8` | Ghidra decompile + `calls.csv` |
| `0x825DBD90` | 2-instruction cleanup wrapper (`mflr/stwu` -> call `0x823899F0` -> return `this`), **not** the sortie entry | Ghidra decompile |
| cave E980 reference | `0x8260E980` has a direct tail caller from `0x8260EBFC` (function `0x8260EBE8`), unlike E290/E700 | `acvd-index q callers` |
| control run | stock `ac_test.txt` reached AC TEST: `env_bgm` hit at 121,211.971 ms, `AcTestSortieScene_Load`/`Ac4SortieScene` threads, `o0006`/`o2016` map objects, shot at 13:24:31 | `private/xenia/phase0_noop.jsonl/.xenia.log`, `phase0_noop_run.out` |

## Architecture

```
tools/xenia-probe/
  ├─ state_nav.py ── probe-hit state machine (no fixed-sleep menu presses)
  │    boot -> caution -> storage -> guide -> world menu -> workshop -> AC Test menu -> mission
  ├─ xenia_probe.py
  │    ├─ probe_helper (Rust example) ── mission/ch<id>.xml exists; MSB part world transforms
  │    ├─ verify: ACV2.pe PE sections + bytes + private/index xrefs -> cave / ring base + capacity
  │    ├─ generate 4E4D0864_ACVD_xenia_probe.patch.toml:
  │    │    map patch + FFX cave + camera cave + (--dump-ffx) AXPR magic/version/flags=ring on
  │    ├─ copy patch into <Xenia Release>\patches\ once, run.ps1 -ApplyPatches
  │    ├─ launch and attach immediately (before env_bgm):
  │    │    ├─ RPM: drain FFX ring from write_index history -> private/xenia/<run>/ffx.jsonl (15-20 ms)
  │    │    ├─ RPM/tail: consume state_nav probe hits; press only in the expected state
  │    │    └─ WPM: camera handshake via CAM_FRAME (disable -> wait 2 frames -> eye/target -> enable)
  │    ├─ after drain: (--annotate) acvd-index + x360dis -> ffx_annotated.txt / ffx_summary.json
  │    └─ after state_nav mission state: shot, stop Xenia, write summary.json, remove probe patch
  └─ test_xenia_probe.py
```

No guest code runs before the patch is in place: the patch file must exist before
`xenia_canary.exe` starts. Do not try to hot-patch after boot; the JIT may already have translated
`0x82B88A78` / `0x828603A0`.

## Patch 1: map/mission id

- Site: `0x825DBB70` = `li r3,6000` (`0x38601770`).
- Patch: one `be32` write of `0x38600000 | mission_id` (`li r3,<id>`), id 0..9999.
- `--map m3330` / `3330` / `ch3330` -> `3330`; `m4000` -> `6000` unless `--mission 4000`
  or `--map ch4000`.
- Preflight: confirm `mission/ch<id>.xml` exists on the ISO. Preferred: new
  `crates/acvd-formats/examples/probe_helper.rs -- mission <id>`; fallback to the
  `private/tmp/missions/mission/ch%04d.xml` dump only as a cache.
- Validation: add a temporary probe `0x82389800 mission_load r3 r4 r5 r6` and
  `0x82640830 mission_load2 r3 r4 r5 r6`; the tool must find the requested id in the
  `.jsonl` before claiming the map loaded. Also grep `.xenia.log` for `m<id>_map.msb` /
  `m<id>.msb` resolve lines and for `Patcher: Applying patch`.
- Risk: non-m4000 mission XMLs have only a `Normal` area and no `actestN` layer. First run
  validates whether the loader falls back to `Normal`. If it does not, do **not** block the tool:
  add a documented follow-up patch to force the layer name to `Normal`
  (`0x82A2A658` / `0x82662318`, audit with `acvd-index q func/callers` and `x360dis` first).
  Maps with no authored start are acceptable for probing: the camera override is independent of
  the player AC, so `--look-at` still works.

## Patch 2: FFX ring trampoline

### Discovery gate (must run before freezing the record)

Run one **state-driven** discovery pass with `state_nav.py`; do not use a blind `ac_test.txt`
sleep recipe. The run must log both FFX sites on the same mission so the earlier discrepancy is
closed empirically:

```
0x82800128 env_bgm max=1 r4
0x82b88a78 ffx_spawn max=200 after=env_bgm r3 r4 r5 r6 lr
  [r4+0x30]:f32 [r4+0x3c]:u32 [r4+0x40]:u32 [r4+0x44]:u32
  [r4+0x48]:u32 [r4+0x50]:u32 [r4+0x10]:f32 [r4+0x14]:f32 [r4+0x18]:f32
0x82b88d08 ffx_set max=200 after=env_bgm r3 r4 r5 r6 f1 lr
  [r3+0x30]:f32 [r3+0x3c]:u32 [r3+0x40]:u32 [r3+0x44]:u32
  [r3+0x48]:u32 [r3+0x50]:u32 [r4+0x40]:u32 [r4+0x44]:u32
0x82891f08 muzzle_call max=20 after=env_bgm r3 r4 r5 r6 f1 [r3+0xb0]:f32 lr
```

Acceptance (all required before the cave record is frozen):
- At `0x82B88A78`, `r5` carries the spawn id for the same events where `0x82B88D08` receives the
  setter id in `r4`; `[r4+0x40]`/`[r4+0x44]` are the request's begin/end pointers, **not** the id.
  At `0x82B88D08`, `[r4+0x40]` must read null/unreadable because `r4` is the scalar id, while the
  request fields are at `r3+0x40/+0x44`.
- Prior `fx_polish.jsonl` showed `r5 = 0x57C/0xC5/0x62`; the live run must reproduce ids with the
  same `set`/`spawn` pairing and log `r3/r4/r5/r6` at both PCs. A mismatch reopens the cave design;
  do not freeze it.
- `dummy_id` is the field that reads 101 (`0x65`) for the muzzle, 111 (`0x6F`) for the cartridge,
  and 200/201 (`0xC8/0xC9`) for the `o0006` sign lamps: expected `r6` or `request+0x3C`.
- `pos_xyz` is the field triple that lands at the AC hardpoint / ground point, expected
  `request+0x10/+0x14/+0x18` (fallback `request+0x60/+0x64/+0x68`).
- Record the exact winning offsets in `docs/xenia-probes.md` and the `sheets/systems.csv` row.

Do not write the final cave until this pass confirms the request register (`r3` vs `r4`) and the
dummy/position offsets. The current static evidence is: `0x82891F08` calls
`0x82B88D08(r3=request, r4=0xCE, f1=scale)` and then `0x82B88A78(r3=out, r4=request,
r5=caller r5, r6=0)`; `0x825DBB70` is only the mission-id getter and is unrelated to this ABI.

### Hook and cave (final shape)

- Hook `0x82B88A78`: the first instruction is expected `mflr r12` (`0x7D8802A6`); confirm with
  `py tools\x360dis\x360dis.py --func 0x82b88a78`. Read the original word from the PE, abort if it
  no longer matches, and replace it with `b <cave>` (`0x48000000 | delta`, delta asserted
  `< 0x02000000`). The cave must re-emit the original instruction verbatim before branching to
  `0x82B88A7C`; do not compile in `mflr r12` independently.
- **Pre-arm the ring in the patch, before any game code runs.** With `--dump-ffx`, the TOML
  contains these `be32` writes at the verified ring base (no host WPM, no `env_bgm` gate):

  | offset | value | meaning |
  |---|---|---|
  | `+0x08` | `0x41585052` | magic `AXPR` |
  | `+0x0C` | `1` | version |
  | `+0x10` | `1` | `flags` (`bit0` = ring enabled) |
  | `+0x54` | chosen capacity | `RING_CAP` (power of two) |
  | `+0x58` | `24` | `RING_RECSZ` |

  The same TOML zeroes `write_index`, `dropped`, `flags`, `CAM_ENABLE`, `CAM_SEQ`, and
  `CAM_FRAME`; `flags=1` is written only with `--dump-ffx`. Without `--dump-ffx`, `flags=0` and
  the cave skips all writes.
- Cave at `0x836EE600` (verified executable padding), ABI-safe:
  1. `stwu r1,-0x160(r1)`; save `r0`, `r10`, `r11`, `r12` at **`0x10/0x18/0x20/0x28(r1)` with
     64-bit `std`**; `mfcr r0`, save CR at `0x30(r1)` (`stw`); `mflr r0`, save the caller LR at
     `0x38(r1)` (`std`). The 0x160 frame clears the 288-byte PPC64 red zone.
  2. Load the ring base into `r11`; check magic `0x41585052` (`AXPR`) and `flags` bit0 (`cmpwi`).
     If either fails, restore and exit to the original instruction.
  3. Atomically claim a slot: `addi r10,r11,0`; retry `lwarx r12,0,r10`; `addi r0,r12,1`;
     `stwcx. r0,0,r10`; `bne-` retry. `r12` now holds the claimed index.
  4. `andi. r12,r12,CAP-1`; `mulli r12,r12,24`; `addi r12,r12,0x100`; `add r10,r11,r12` forms the
     slot address in the power-of-two ring.
  5. Store the 24-byte record (fields below) with `stw`/`lwz` raw words; `sync` after the payload.
     The index was already advanced by the claim; the host's 16-slot drain lag prevents it from
     reading an in-flight slot in practice.
  6. Restore CR (`lwz r0,0x30(r1)`; `mtcrf 0xFF,r0`), LR (`ld r0,0x38(r1)`; `mtlr r0`), then
     `r0/r10/r11/r12` with `ld`, `addi r1,r1,0x160`, emit the original instruction,
     `b 0x82B88A7C`.
- Ring record (24 bytes, PPC big-endian):
  `u32 ffx_id, u32 dummy_id, u32 caller_lr, f32 x, f32 y, f32 z`.
  `x/y/z` are copied with `lwz` + `stw` as raw 32-bit words; no `lfs`/`stfs`, no float register
  save/restore.
- Control block (header 0x60 bytes, ring data at `+0x100`):
  `+0x00 write_index`, `+0x04 dropped` (host-computed), `+0x08 magic 'AXPR'` (`0x41585052`),
  `+0x0C version`, `+0x10 flags` (bit0 = ring enabled), `+0x14 CAM_ENABLE`, `+0x18 CAM_PTR`,
  `+0x1C CAM_SEQ`, `+0x20 CAM_EYE xyz`, `+0x2C CAM_TARGET xyz`, `+0x38 CAM_READ_EYE xyz`,
  `+0x44 CAM_READ_TARGET xyz`, `+0x50 CAM_FRAME`, `+0x54 RING_CAP`, `+0x58 RING_RECSZ`,
  `+0x5C reserved`.
- Ring size: `0x100 + CAP*24`. Preferred `CAP=1024` (`0x6100`); fallbacks 512 (`0x3100`), 256
  (`0x1900`), 128 (`0xD00`), 64 (`0x700`). The verifier relocates the whole block to the largest
  suitable run; the old `0x83B92900` candidate is only a starting hint.
- The host attaches as soon as the Xenia process exists and drains every 15-20 ms, starting at
  `max(0, write_index - CAP)` so retained boot/map-load records are emitted from the beginning of
  the   ring. It only consumes up to `write_index - 16` (lag), validates `ffx_id != 0`, a plausible
  `caller_lr` in `.text`, and finite positions, and reports host-computed drops
  `max(0, write_index - cursor - CAP)`. Before stopping Xenia, wait ~100 ms and perform a final
  drain with the lag removed so the last 16 records are not lost.
- Residual risk: a producer stalled longer than the 16-slot lag could still be read mid-write. If
  that is observed, escalate the record to 32 bytes with a `seq` publication word and re-verify a
  512-record (16 KB) ring; do not change layout silently.

## Patch 3: camera override trampoline

- Hook `0x828603A0` (after both game stores; original `lwz r0,0x84(r31)` = `0x801F0084`) with
  `b <cave>`; re-emit the original word at the cave exit, then `b 0x828603A4`.
- Cave at `0x836EE800`, ABI-safe:
  1. `stwu r1,-0x160(r1)`; save `r0`, `r11`, `r12` at **`0x10/0x18/0x20(r1)` with 64-bit `std`**;
     `mfcr r0`, save CR at `0x28(r1)` (`stw`); `mflr r0`, save LR at `0x30(r1)` (`std`); load the
     ring base into `r11`. The 0x160 frame clears the 288-byte PPC64 red zone.
  2. `CAM_FRAME += 1` every invocation (unconditional tick used by the host handshake).
  3. Write `r31` to `CAM_PTR` (publishes the live camera object address).
  4. Copy `r31+0x0C..0x14` to `CAM_READ_EYE`; copy `r31+0x1C..0x24` to `CAM_READ_TARGET`, using
     `lwz`/`stw` raw words.
  5. `lwz` `CAM_ENABLE`, `cmpwi`; if nonzero, copy `CAM_EYE` to `r31+0x0C..0x14` and `CAM_TARGET`
     to `r31+0x1C..0x24` with `lwz`/`stw` (no float registers).
  6. Restore CR (`lwz r0,0x28(r1)`; `mtcrf 0xFF,r0`), LR (`ld r0,0x30(r1)`; `mtlr r0`),
     `r0/r11/r12` with `ld`, `addi r1,r1,0x160`, execute `lwz r0,0x84(r31)`, `b 0x828603A4`.
- Camera struct layout (cite `camupd.c`): eye `+0x0C`, w `+0x18`; target `+0x1C`, w `+0x28`;
  the game writes w=1.0, so copy xyz only.
- **Host write ordering** (`CAM_FRAME` is the handshake):
  - Publish: `CAM_ENABLE=0`; poll `CAM_FRAME` until it advances by at least 2 (timeout 120 ms);
    write `CAM_EYE` + `CAM_TARGET` (one contiguous WPM); `CAM_SEQ += 1`; then `CAM_ENABLE=1`.
  - Update: repeat the same disable/wait/publish sequence; never write eye/target while
    `CAM_ENABLE=1`.
  - The 2-frame wait drains any cave invocation that already saw `CAM_ENABLE=1`; enabling only
    after the vectors are written means a cave tick can never mix old and new coordinates.
- Host `--look-at` math:
  - literal `X,Y,Z`: target = that point.
  - `ac`: target = `CAM_READ_TARGET` (the live point on the player AC).
  - `part:<model>[:<instance>]`: resolve the authored MSB part transform via
    `probe_helper -- msb <map> <model> [instance]`.
  - eye = target + `normalize(CAM_READ_EYE - CAM_READ_TARGET) * --dist` (default 20 m); if
    readback is not ready, use `--eye X,Y,Z` or a documented default offset.
- Verification: after `CAM_ENABLE`, read `CAM_PTR` (nonzero) and the device view eye at
  `[[0x83A4E62C]+0x400]`; warn if it is more than 1 m from the requested eye.

## Cave/ring verifier (`--verify`, also run inside every launch)

1. Parse `private\x360\vd\ACV2.pe` as a PE image: DOS/PE headers, section table, image size
   `0x1C50000`, file offset = `addr - 0x82000000`. Record each section's VA, virtual size, raw
   size, raw pointer, and flags; a range is writable if its section has `IMAGE_SCN_MEM_WRITE`.
2. Code candidates `0x836EE600` and `0x836EE800`: each must have at least `0x200` bytes inside an
   executable section, bytes all `0x00` (or another chosen repeated padding byte that is safe to
   execute), and no references in `private/index/{consts,functions,fields,slots,vcalls}.csv`.
   After encoding, assert each cave fits its 0x200-byte slot and the two ranges do not overlap.
3. Data candidate: prefer the largest zero/no-ref writable run in `.data` (file-backed) over
   `.bss` for `0x100 + CAP*24` bytes, `0x100`-aligned, trying `CAP=1024` (`0x6100`), 512
   (`0x3100`), 256 (`0x1900`), 128 (`0xD00`), 64 (`0x700`). For file-backed `.data`, all bytes
   must be `0x00`; for a virtual-only `.bss` tail (`virtual size > raw size`), zero-fill is
   implicit, so check only the xrefs and the mapped range. Start at `0x83B92900` as a hint, but
   prefer any larger qualifying `.data` run; use `.bss` only when no `.data` run is large enough.
4. If a candidate fails, scan the same section for the next aligned 0x200-byte (code) /
   `0x100 + CAP*24` (data) zero run with no refs. Print the chosen code/ring bases, section
   names/permissions, capacity, and first/last 16 bytes; write them into the generated patch.
5. Verify the hook words: `0x82B88A78` = `0x7D8802A6` (`mflr r12`) and `0x828603A0` =
   `0x801F0084` (`lwz r0,0x84(r31)`). Abort if either changed; the cave re-emits these words.
   Assert each hook-to-cave and cave-to-hook delta satisfies `abs(delta) < 0x02000000`.
6. Re-verify with:
   ```
   acvd-index q const <cave> --limit 10
   acvd-index q const <ring> --limit 10
   acvd-index q func <hook>
   py tools\x360dis\x360dis.py --func 0x82b88a78
   py tools\x360dis\x360dis.py --range 0x82860380 0x828603d0
   ```

## CLI

```
xenia-probe --map <m####|####|ch####> [--mission <id>]
            [--look-at <x,y,z|ac|part:model[:n]>] [--eye x,y,z] [--dist 20]
            [--dump-ffx] [--annotate] [--shot <png>] [--test actestN] [--recipe <path>]
            [--timeout 240] [--out-dir private\xenia\<run>]
            [--hash 03794C9BD0139BBF] [--keep-patches] [--verify] [--dry-run]
xenia-probe --annotate --input <ffx.jsonl> [--out-dir <dir>]
```

- `--verify`: audit caves/ring, print the chosen bases/capacity and the patch TOML, exit. No launch.
- `--dry-run`: same plus preflight (Xenia binary, ISO, mission XML, recipe), no launch.
- `--map m4000` -> 6000; `--map m3330` -> 3330; `--mission 4000` -> 4000.
- `--dump-ffx` pre-arms the ring in the patch and writes `ffx.jsonl` from the moment the process
  starts (one object per record: `n`, `ms`, `ffx`, `dummy`, `lr`, `x`,`y`,`z`, `dropped`,
  `write_index`).
- `--annotate`: after the drain, resolves each unique `caller_lr` and writes `ffx_annotated.txt`
  plus `ffx_summary.json` into the run directory. With `--input <ffx.jsonl>` it annotates an
  existing file and exits without launching; `--map` is not required in that mode. Names are
  best-effort: `private/tmp/named_funcs.csv` when present, otherwise `FUN_<function start>`;
  function bounds always come from `acvd-index q func`.
- Annotation content: for each unique LR, resolve `LR-4` (the `bl` site) with
  `acvd-index q func`, record the containing function `START..END (+offset)`, the resolved name,
  and the `x360dis.py --range <LR-0x40> <LR+0x08> --raw` slice (PPC + VMX128) around the call,
  clamping the low bound to the function start; `ffx_summary.json` holds counts per function and
  unresolved LRs. Annotation failures are warnings, never a nonzero exit.
- `--shot` is captured only after the state machine has reached the mission state (`env_bgm` or
  the `AcTestSortieScene_Load`/`en` boundary); it never relies on a fixed wait from
  `ac_test.txt`.
- `--state-timeout <s>` (default 30) bounds every navigation step; on timeout the tool takes a
  debug guest shot, writes the last observed probe state, and exits nonzero. `--recipe` is a
  manual override for demos and is not used by the default state machine.
- Exit codes: 0 ok; 2 usage/preflight; 3 patch not applied / ring magic not observed; 4 requested
  mission id not observed; 5 no FFX records; 6 camera override not observed; 7 state navigation
  timeout/failed step.
- Patch lifecycle: write `4E4D0864_ACVD_xenia_probe.patch.toml` to the Release `patches/` dir
  before launch, remove it after unless `--keep-patches`. Refuse to start if `xenia_canary` is
  already running; refuse if any other `4E4D0864*.patch.toml` probe file is present.

## Files to add / change

- `tools/xenia-probe/xenia_probe.py` — CLI, PE/xref verifier, patch TOML generator (including
  `AXPR`/`flags` pre-arm writes), PPC encoder (CR save/restore, 64-bit `std`/`ld` GPR slots,
  `stwu` frame, raw-word float moves, `lwarx/stwcx.` claim), early ctypes RPM drain, WPM camera
  handshake, `--annotate`, look-at math.
- `tools/xenia-probe/state_nav.py` — probe-hit state machine, 30 s per-step timeout, debug shot on
  failure, and the single place that presses front-end buttons. It consumes
  `0x82B28DB8`/`0x8268D138`/`0x825DBDC8`/`0x825DBCD8`/`0x825DCB68`/`0x82800128` hits and never
  uses unconditional fixed-sleep menu presses.
- `tools/xenia-probe/test_xenia_probe.py` — `unittest`: PE header parsing, branch encoding with
  the `abs(delta) < 0x02000000` assertion, patch TOML shape, ring capacity/parsing, 64-bit
  cave save/restore encoding, camera frame handshake, look-at math, state-machine timeout/step
  logic, and generated cave words decoded through `tools/x360dis/x360dis.py` (not `ppc_vmx128`
  for scalar instructions).
- `tools/x360dis/x360dis.py` — add a small `disasm_words(words, base)` helper plus a `--words`
  CLI mode so generated cave words decode through the same capstone/VMX128 path as image code.
  Existing `--func`/`--range`/`--trace` behavior is unchanged.
- `tools/xenia/run.ps1` — add `-ApplyPatches` (pass `--apply_patches=true`); keep the default
  `false`. No other behavior change.
- `crates/acvd-formats/examples/probe_helper.rs` — `mission <id>` existence and
  `msb <map> <model> [instance]` transform output as JSON; reuse `vfs::Disc` and `msb`.
- `docs/xenia-probes.md` — new "Unlock-free map/FFX/camera probes" section with the CLI, patch
  addresses, ring record/capacity, cave ABI rules, and annotation format.
- `docs/open.md` / `docs/archive.md`, `sheets/systems.csv` — task status, addresses, evidence.
- No generated `.patch.toml`, `private/`, `external/` artifact is committed.

## Phases and ordered implementation tasks

- **Phase 0 — Evidence lock** (no implementation code): run the dual-PC FFX probe and the
  E290/E700/E980 0-hit run with `state_nav.py`; finish the ring candidate scan (the
  `0x83B80000..0x83B90000` block is already rejected); validate the native dialog state probes.
- **Phase 1 — Static tool**: `probe_helper`, `-ApplyPatches`, encoder/verifier, `AXPR` patch TOML.
- **Phase 2 — Map + state navigation**: map patch validation and the probe-hit state machine;
  no blind sleeps.
- **Phase 3 — FFX ring**: 64-bit-save caves, `AXPR`/`flags` pre-arm, immediate 15-20 ms drain,
  write_index `lwarx`/`stwcx.`.
- **Phase 4 — Camera + shot + annotation**: `CAM_FRAME` 2-tick handshake, `--look-at`, `--shot`,
  `--annotate`.
- **Phase 5 — Docs/commit** (tasks 9-10).

1. Add `probe_helper` example + test: mission XML exists, MSB part lookup.
2. Extend `run.ps1` with `-ApplyPatches`; keep `--apply_patches=false` as default.
3. Add `x360dis.disasm_words`/`--words`; implement the PPC encoder with CR save/restore, the
   `0x160` frame, raw-word float moves, `lwarx/stwcx.`, and the branch-range assertion; self-test
   the generated words through `x360dis`, not `ppc_vmx128`.
4. Implement `--verify`: PE section parse, executable/writable checks, zero-byte/`.bss` handling,
   index xref scan, 1024→512→256→128→64 capacity fallback, hook-word and branch-delta checks.
5. Implement map patch generation + launch + mission-id probe validation; test on m4000 (6000)
   then m3330 (3330). Launch only through `state_nav.py`, not a fixed-sleep recipe.
6. Run the FFX discovery pass through `state_nav.py`; log both `0x82B88A78` and `0x82B88D08`
   with `r3/r4/r5/r6` and `[r4+0x40]..[r4+0x44]`; record the winning request register, dummy
   offset and position offsets in the plan/docs. Do not freeze the cave before this closes the
   `r5`-vs-request-array discrepancy.
7. Implement the FFX cave + pre-armed `AXPR` ring + immediate 15-20 ms drain + JSONL; validate
   known m4000 FFX ids and that map-load ambient records appear before any `env_bgm`/`trigger`.
8. Implement the camera cave + `CAM_FRAME` 2-tick handshake + `--look-at` + `--shot`; validate
   `part:o9330` on m3330 and `ac` on m4000, including a live coordinate update.
9. Implement `--annotate` (run and `--input` mode): unique `caller_lr`, `acvd-index q func`,
   name fallback, `x360dis` slice, `ffx_annotated.txt` + `ffx_summary.json`.
10. End-to-end runs; write docs/sheets; `acvd-index audit`; stage only touched files; commit,
    `git pull --rebase`, push.

## Validation checklist

- `py -m unittest tools\xenia-probe\test_xenia_probe.py` (includes cave words decoded by `x360dis`
  and branch-delta assertions)
- `py tools\xenia-probe\xenia_probe.py --verify`
  - prints two executable 0x200-byte caves, a `CAP=1024` (or fallback) writable ring, and the
    expected hook words; aborts on any mismatch
- Control run: `xenia-probe --map m4000 --dump-ffx --shot private\xenia\shots\m4000_probe.png`
  - navigation is performed by `state_nav.py`; a deliberately blocked state exits 7 within 30 s
    and writes a debug shot, instead of pressing buttons on a blind timer
  - `.xenia.log` contains `Patcher: Applying patch` and `Module Hash: 03794C9BD0139BBF`
  - ring magic `AXPR` (`0x41585052`) appears within the 5 s attach window; `.jsonl` has records with timestamps
    from map load, before any `env_bgm`/`trigger` probe
  - the mission-id probe sees 6000; `ffx.jsonl` contains 1404/197/98 and muzzle/cartridge ids
    when the recipe fires
  - every drained record has `ffx != 0`, a `caller_lr` in `.text`, and finite positions; no
    duplicate/corrupt slot from concurrent spawns
- New map: `xenia-probe --map m3330 --look-at part:o9330 --dump-ffx --annotate --shot ...`
  - mission-id probe sees 3330; `.xenia.log`/probe shows m3330 loaded
  - `CAM_PTR` nonzero and the device view eye matches the requested point within 1 m
  - a live `--look-at` update while enabled produces no NaN/torn readback (2-frame handshake)
  - `ffx.jsonl` non-empty and contains m3330 object/aircraft ids
  - `ffx_annotated.txt` has one block per unique `caller_lr`, each with function bounds/name and
    a decoded PPC+VMX128 slice; `ffx_summary.json` counts match the JSONL
- `acvd-shot diff` only where a 360 reference exists; otherwise record the shot as an appearance
  check, not placement evidence.
- `acvd-index audit` (0 mismatches, 0 PS3); `cargo test -p acvd-formats` for `probe_helper`.

## Risks and contingencies

- **Layer/area mismatch**: ch3330+ have only `Normal`; the AC TEST scene may still request
  `actestN`. Validate before declaring success. If the map does not load, implement the
  `Normal` layer-name fallback (`0x82A2A658` / `0x82662318`) or move to Free Battle later.
- **No player start** on non-m4000 missions: acceptable for probing; the camera override is
  independent. Do not add a spawn patch in v1.
- **FFX register/offset ambiguity**: the discovery pass is mandatory; do not freeze the cave
  until the dual-PC log closes the `r5` vs `request+0x40/+0x44` discrepancy and dummy/position are
  confirmed against known dummies.
- **Blind menu timing**: fixed sleeps already produced a false "hang" in Phase 0 (the stock
  recipe actually reached AC TEST at 121 s; a later run was killed when a foreground reader hit
  the shell timeout). `state_nav.py` is the only navigator; never add unconditional
  `press`/`wait` menu sequences to the probe tool. Long waits must be state-bounded and run in the
  tool process, not as a blocking shell command around a background Xenia.
- **Boot-straight skip**: do not jump to the sortie scene or `0x825DBD90`. The sortie is
  pre-created by the AC Test menu (`0x82628E90` -> `0x825DBDC8` -> ctor `0x825DBC80`) with
  manager/mode/sortie info; the Lua menu scene 2008 and the garage/save init must exist first.
  A partial skip to scene 2008 after the garage is initialized may be explored later, but only
  after Phase 0 proves the dialog/scene states.
- **Ring candidate refs**: the old `0x83B80000..0x83B90000` block is rejected (498 refs); the
  verifier must scan `consts`/`fields`/`slots` and never assume file-zero bytes are free.
- **Patch hash**: abort if the run's `Module Hash` differs from `--hash`; do not silently write
  patches for another build.
- **Code page permissions**: code caves must be in `.text`; if a page is not executable or the
  padding byte is not safe, scan the next executable zero run.
- **Ring publish race**: `write_index` is atomic, but a producer can still be writing a slot when
  the host reaches it. The 16-slot lag plus record validation makes this unlikely; if a corrupt
  record appears, move to 32-byte records with a `seq` publish word and a 512-record ring.
- **`.bss` patch writes**: the pre-arm writes must land in mapped guest memory. Verify the ring
  base is inside the image and, if Xenia rejects the write, relocate to the file-backed `.data`
  tail instead of silently continuing.
- **Host attach delay**: if the process starts before the RPM handle is open, the ring already
  holds the boot/map-load records; the drain reads retained history from `write_index - CAP`.
  Poll the magic for up to 5 s after attach (the patch lands after `LoadContinue`), then abort with
  exit 3 if it is still absent rather than assuming an empty run.
- **Cave ABI**: do not add scratch registers without saving them; the current sets are
  `{r0,r10,r11,r12,CR,LR}` (FFX) and `{r0,r11,r12,CR}` (camera). Only the low 0x20 of the
  `0x160` frame is written (`old_sp-0x148` or lower), so the 288-byte red zone is untouched;
  restore SP before re-emitting the original instruction.
- **Annotation names**: `private/index` has no symbol column. Names are best-effort from
  `private/tmp/named_funcs.csv` (or a running Ghidra bridge), otherwise `FUN_<start>`; never block
  the probe run on Ghidra.
- **Single Xenia**: abort if `xenia_canary` is already running; one run at a time.
- **Existing patch files**: the patch DB applies every enabled `4E4D0864*.patch.toml` under
  `patches/`; remove stale probe files and never leave a probe patch enabled after the run.

## Out of scope for this task

- Free Battle / Mission 00 navigation and unlock checks.
- Live transforms of moving parts (v1 resolves authored MSB positions).
- Rendering FFX or changing `acvd-game`; this is probe tooling only.
- Any PS3/RPCS3 address or artifact.
