# Status and task list

Each chat takes **one** open task, works it, and updates this file before ending: move it to
Done with a one-line summary, record findings (360 addresses, file layouts, dead ends) under the
task, and add any new tasks it uncovered. Per-system data/runtime status also lives in
`sheets/systems.csv`; keep both in step.

## Open

Rough priority order; reorder freely.

- **Animation**: TAE event reader; core control (upper body faces the aim while legs strafe);
  upper/lower body layering; boost_a and part `*_a` clips (`Booster_Frame` waits on these).
  - TAE lead: `motioncollate` `motion_ow_disarm` / `motion_ow_equip` are the TAE owner ids
    (100, 1100, 1200, 4100, 7100, 8100, ... = `tae/ac/action_ow_disarm/own000100.tae` ...); the
    360 TAE event dispatcher is `0x8238eb08` (switch on event type: 10, 20, 100-104, 150-152,
    200-202, 300, 301, 400, 405, 410, 450, 500, 900, 910, 920, 1000, 1010, 1020, 1030, 1040).
  - Blend units open (see Done): find the 360 reader of a hokan row's `GeneralFrame`
    (`acvd-index q field` on the `ACANIM_HOKANPARAM_ST` offsets; `AcMotion_loadCollate`
    `0x8249e840` stores `HokanParamID`), then one Xenia probe on it while walking (row 2 = 60).
    Recipe in `sheets/anim_blend.csv` row `blend`.
- **Camera leftovers**: impact / foot-step shake and the ready-position first-person view (both
  need triggers free play doesn't produce: earlier emulator breakpoints on the camera message
  0x68 / 0x69 handlers never fired; ready position waits on weapons). Xenia re-check: probe the
  handlers `0x8285cf48` / `0x8285cf68` / `0x8285cf88` / `0x8285cfb0` (registered for messages
  0x66-0x69 by `0x836cad88` into the table at `0x83713b70`) during a mission with landings and
  hits; the same run re-checks `dash_inv` (0x66) and `side_moving` (0x67) in
  `sheets/camera_follow.csv`. Speed blur: the per-call ease runs once per 1/30 s
  game frame (Xenia probe, `frame_time` / `speed_blur` in `sheets/camera_follow.csv`); the
  runtime (`blur.rs`) still steps it per 60 Hz tick and needs to step per 1/30 s. A camera action
  with BlurRate 2 (seen for about 0.8 s in a run that included firing) is not mapped: probe the
  row id controller slot `0x48` (`0x8285e500`) returns to name it. Speed blur open points: the
  zoom-blur source width (taken as 1280), the
  filter fade at +0x54 (taken as 1), and the vertex program `ZoomBlur_ScreenSpaceQuadShaderVS`
  (TEX1 assumed to be the vertex NDC; `private/tmp/rsxvp.py` is a first, still wrong, decoder).
  Follow-camera open points: which base-transform factor carries the waist offset (the dump
  shows the result, see `look_at_height`); the writer of controller +0x18 (taken as the turn
  input for `eye_shift`); `FollowOffsetY` (+0x18 of the offset row, 3.5 m, "when the camera
  approaches the AC") has no reader found.
- **Movement leftovers** (360 movement vtable `0x8208fea0`, builder `0x828212f8`, integrator
  `0x82822ea0`):
  - Non-boost air steering: struct index 0x14 (Lua param 50; not yet in `ac_ctrl_calc.csv`).
  - Glide: states 4/5 take a separate integrator path (`0x8281e408`); the runtime still brakes
    above the glide max.
  - Per-state current max (`0x82822af0`): switch on +0x1bc, x terrain/slope scale +0x334, x +0x324.
  - Dash / air-boost acceleration multiplier (field 406, also the default divisor at `0x5c4`).
  - Tick rate (Xenia probes `private/xenia/move_tick.txt`, `vertical.txt`, flying design 5010;
    evidence in `sheets/ac_ctrl_calc.csv`): the game steps once per 1/30 s frame, and each step
    equals two 1/60 s ticks: position (height at entity `+0x104`) moves 2 x velocity, the
    acceleration adds 0.073 = 2 x `walk_acc_tick`, gravity -0.050 = 2 x `gravity_tick`, and a
    full jump reaches `jump_rise_tick` (0.7148) in one frame. So the runtime's 60 Hz tick is
    right. The boost gravity multiplier (movement `+0x284`, 0.0217) applies only while boost
    mode is on and the AC falls; the runtime now does that (`control.rs` step). Open: whether
    hold time scales jump height smoothly (only a tap 0.159 and a full 0.714 seen); the fall
    cap (not reached); over-max decay and air drag per frame vs per tick; the boost/fall
    switch is decided per 1/30 s frame in the game, per tick here.
- **Paint**: map `_c` mask regions to accolor channels via the ACColor fragment programs in
  `shader/flver_shader.bnd`.
- **Assembly gaps**: shoulder attach flag 5 (VMX `0x8288a058`; the parallel-to-+Y case
  rotates the up hint by ±90° and that axis is still unread), recon mounts, LOD switching;
  8 pending FLVERs (`0x20007` and FLVER0).
- **Maps leftovers**: rest of MSB (MODEL/EVENT/POINT/ROUTE/LAYER/TREE), `.smd`, map FLVER
  visuals, water-layer materials (none named water on m3100), collision filters 0x100002 /
  0x200002. HNAV/HTR navigation readers.
- **FCS / lock-on HUD**: AcFcs lock-sight cone (20° × 20°, datum 20 m back); not a camera mode
  (see `lock_on` row in `sheets/camera_follow.csv`).
- **Weapons leftovers** (hand-weapon tracers done, see Done):
  - Units unconfirmed: `init_speed` (read as km/h), `BulletRigidSt.gravity` (paramdef label
    重力加速度, no unit; read as m/s added per 60 Hz tick, so 1.0 drops rifle 10002 rounds
    steeply), `reload_time` (read as ticks). Needs the 360 bullet update reading these
    (`acvd-index q field` on the `BulletRigidSt` offsets), then a Xenia probe on a fired round's
    velocity (user at the controls, firing).
  - `magazine` (+0x138) name unconfirmed: may be total ammo.
  - Not done: bullet max/min speed and brake, hit/damage (`hit_id`, `damage_power`), energy
    drain, missiles (`bulletmissile`), blades, shoulder weapons (category 12 bullet ids at
    +356/+360), weapon modes, bay shift, core aim (shots follow the camera pitch, not a posed
    upper body). The lock-sight HUD shows the hand-weapon ammo counts (`acvd-game::hud`).
  - Firing clips: `ready_position` (+0x153) and `weapon_kind` (+0x0f) are on the sheet. The
    360 call that starts `a00_001` on a held trigger was not found (joint-control bit 31 at
    `0x82456f68` selects `sniper_$(LR)` vs `gun_$(LR)` and was not tied back to +0x153).
    Shots wait until the deploy clip ends; that timing is the clip, not a TAE event.
    Ready weapons (`ready_position`: cannons, sniper cannons, the other heavy classes) play
    acmotion 78/75 while fire is held and 79/76 on release (`sheets/ac_states.csv`; the blend
    rows are キャノン構え / キャノン解除, b_apply_lower 1). Sniper rifles (kind 5) do not. The arm
    clip only overrides the bones it animates (the `r_shoul_p*` / `l_shoul_p*` pads), so it
    does not plant the rest of the arm back on the clip bind: those tracks are two identical
    identity keys, and writing them was erasing acmotion 78's `r_arm01` pose. The arm's
    `a00_001`/`a00_003` play on that side's bones while a ready weapon is deployed. Rifles
    (kind 4) kick `gun_$(LR)` ReactAng (−12° on arm01-03, −5° on arm05, 6 frames;
    parser `0x82b73b28` stores the angle at +0x18). The kick axis is not a field; it is applied
    as a local X rotation because LockMinX/LockMaxX is the wide limit on those bones.
- **UI**: in-game HUD next steps: AP / energy state for the hidden `AP*` / `EN*` digits, the `AlphaAnimSprite` gauges (`Gauge_LWeapon` ...; record layout unread), the side `Weapon` panels (part name + `CurAmmo` runtime text at the `ACV_FE_Normal` LeftArm / Shoulder / RightArm anchors); trace the Dialog color tint (inferred, see Notes); animation tables (PINA/KINA/OINA/MINA...). Open: Text word +0x0c (0, 502, 1000-1004, 1030, 1200-1204; not a scale), Text flags byte 1 / low half (0x0204000f ...; base ctor 0x824da1d8 passes them to 0x824d9450 / 0x824d95f0). **Param enums**: TDF reader.
- **Effects leftovers** (FFX player done, see Done): every slot meaning in
  `sheets/ffx_actions.csv` is inferred from the effect files. The 360 action-id to class map
  is still missing. Class names (`FXClusterEmitter_Cone`, `FXClusterAppearance_Model`,
  `Sfx_PointSprite` ...) are in the executable, each source-path string followed by its
  vtable (near 0x82172a40 and 0x82230e80); `FXBasicActionHandler` is referenced at 0x83121988
  and 0x83121c10. Open questions:
  - Blend values other than 2 (alpha) and 4 (additive).
  - Euler order of transform 36 (YXZ assumed).
  - Whether particles follow the emitter: 108 models follow, 71/82 stay in the world.
  - Point-sprite size unit (action 82).
  - Light intensity (only colour and radius are stored).
  - Not drawn: distortion (43), radial blur, tracer/line actions, actions 2031/2035/2118/3000.

  Gameplay gaps:
  - Which motion fires which booster is a guess: main 300 boosting forward, back 302 boosting
    backward on leg points 25-28, foot 301 rising.
  - Not wired: quick boost (303-305), booster light 299, ground dust (`groundsfxparam.bin`
    walk/landing rows), water splashes, cartridges (`cartridge_sfx_id`).
  - Muzzle scale: f0001218 as stored is a 16 m flash and a 20 m sprite (user: far too
    large), so the game scales weapon effects at spawn. `weapons::MUZZLE_SCALE` 0.25 is a
    guess; find the spawn call that reads `muzzle_sfx_id` (+0x02 of the weaponsfx row) and its
    scale argument (`acvd-index q field 0x2` / `q name muzzle`), then a Xenia probe on that
    call while firing.
  - Hits always use `default2`: the collision mesh keeps no material.
  - The `hit_sfx_type` to `bullethitsfxparam.bin` row mapping is assumed.
- **360 disc data migration** (in this order; one task per chat). Game code is already 360-only,
  and `vfs::Disc` reads the 360 ISO (Done, "360 disc VFS"); the default disc stays the PS3 dump
  until 3 and 4 land, then `vfs::default_disc` flips to the ISO:
  1. Done: extract from the 360 disc (see Done, "360 extract").
  2. **Xenos TPF and 360 FLVER**: 360 texture formats (tiled DXT) and FLVER vertex / index
     buffers without Edge compression; `texture_formats.csv`, `formats.csv` rows updated.
     From the ISO today every AC part FLVER fails `header 0x4B at 0x4b is nonzero` and every
     TPF `TPF platform 1 is not PS3` (`acvd-game --disc <iso>` logs both; map `.hmd` and FFX
     already read unchanged).
  3. **XMA sound, fonts, Lua, movies**: XMA FSB banks (`sheets/sound_cues.csv` playback; from
     the ISO `se_weapon` samples report "not MPEG"), the `s1_X360` font path (`font/e1_ext/` is
     not on the 360 disc), Lua from `script.bhd` (readable as `script/<name>.lc` through
     `Disc`), WMV movies (loose `movie/jp/*.wmv`; the PS3 build has PAMF). Re-read the
     zoom-blur Xenos shaders (`speed_blur_draw`).
  4. **Delete the PS3 paths**: remove Edge / PAMF / RSX code, the `ACVD Unbound` defaults and
     the directory side of `vfs::Disc`, PS3 rows in `target.csv` (title id, PARAM.SFO),
     `formats.csv`, `texture_formats.csv`. The 360 extract's preflight then needs the 9
     PS3-only exception rows dropped (`texture_formats.csv` 33, `vertex_types.csv` 0x10/3,
     0x10/6, 0x2f/2, 0xf0/0, `container_exceptions.csv` m7540 / m7770 / e9120: PS3-only maps
     and enemy) and the 4 PS3-only `formats.csv` rows (`.list`, `.pam`, `.pem`, `.sdat`).
  - **The two discs ship different balance data** (see Done, "360 extract"): 112 of 622
    `acvparts.bin` records, 95 rows of 6 PARAM files and 6 tuning values differ. The generated
    data (`acvd-data`) still comes from the PS3 dump, while every Xenia probe runs the 360
    data. Until the default disc flips, check any value derived from those fields (legs
    `walk` / `turn` / `std_gravity` / `max_load_downer`, weapon `weight` / `init_speed` /
    `reload_time` / `missile_lock_time` / `en_drain` / `no_decay_range` / `damage_power`,
    generator `power`) against `private/tmp/acparts_ps3_x360.txt` before comparing it with a
    probe. Which disc is the later regulation is not known.
  - **Unnamed 360 entries left**: 38 TPF (`_unknown/image/<hash>.tpf.dcx`, sizes 5-370 KB, often
    in identical pairs), 3 DRB menus (`_unknown/lang/<hash>.drb.dcx`) and 1 PNG. Not found
    under `/lang/<l>/menu/`, `nowload/`, `model/image/`, `image/` with the exe / Lua / named
    stems (`private/tmp/guess360d.py`).
- **Xenia re-checks of earlier emulator captures** (rows say "Xenia re-check pending"):
  `camera_follow.csv` `look_at_height`, `base_transform`, `pitch` (probe the AC+0x234 pitch
  update; its 360 address is still to find from the AC update `0x828bef18`), `follow_ease`,
  `dash_inv`, `side_moving`, `shake`, `eye_lift`. Pending 360 addresses: the per-id part
  getters behind `0x8246fbb0` (`ac_part_fields.csv`), the AC+0x1104 update gate, the
  camera-effects view builder, the offset-row lerp.
- **Movies** (PAMF), **mission events** (EVD),
  **AI** (decompiled Lua in `private/lua`, no sheet yet).

## Done

- 360 extract: `acvd-sheets extract` (and `archives.rs`, tuning, AC parts) reads through
  `vfs::Disc` (`files` / `read` / new `size` / `head`), so `--disc` takes the dump or the ISO;
  the default stays the PS3 dump. `extract --disc <iso>`: 18,855 files, 160 tables, 65,984
  rows, every PARAM row round-trips, 0 orphan params / def errors (30 s; the dump takes 2 min).
  The ported code gives the PS3 dump's old output except list order (paths sort as `/` strings).
  - **Naming**: every BHD5 entry now has a path in `private/x360/dvdbnd_names.csv`
    (`private/tmp/name360.py write`). 2,080 of the 2,283 unnamed hashes are the PS3 dump's
    `_unknown/<dir>/<decimal hash>.<ext>` files (same path hash): all 972 `_unknown/param`
    (mission `EVENT_MESSAGE_ST` / `EVENT_MESSAGE_TEXT_MAP_ST` tables) and 498 FMG among them.
    `vfs::Disc` reads such a name by its hash, so 360 and PS3 sheet ids match. 161 real names
    come from the 360 exe's path formats (`private/tmp/guess360c.py`: `$(Data)\bind\mission\ch%04d.bnd`
    151, `bind/boot.bnd`, `bind/boot_2nd.bnd`; chance hits expected 0.12) plus 8 type-matched
    hits (`lang/jp/menu/{gameboot,betaversion_msg,copyrightlogo_xbox}.{drb,tpf}.dcx`,
    `material/menu03{20,50}_mtd.bnd`). The last 42 are named by magic (Open, migration).
    Guessing the event PARAM / FMG names (`/lang/<l>/text/mission/<id>_comNN`, AiResource
    roots from `system/acv2.ini`) found nothing above chance.
  - **Load bundles** (`vfs::BUNDLES`): `bind/boot.bnd` (6.5 MB, `system/paramlist.xml`,
    `font/fontdef.xml`, every `dbmenu/*.dbp`, shader binders ...), `bind/boot_2nd.bnd` and the
    151 `bind/mission/chNNNN.bnd` (mission XML, MSB, `.hnav` / `.htr`, `.evd`, material) are
    BND3 whose member names are disc paths. 6,297 members, 2,781 paths; 611 exist nowhere
    else on the disc, the other 5,185 are byte-identical to their BHD5 copy, and no path
    differs between bundles. `Disc` indexes the members (headers only, `bnd3::read_header`) as
    the last lookup source; the archive walk skips the bundles (`vfs::is_bundle`).
  - **Preflight from the ISO**: the `.bdt` / `.bhd` `formats.not_on_disc` warnings clear;
    new `formats.csv` rows `.wmv`, `.xex`, `.manifest` (`$SystemUpdate`), `.xpr` (XPR2 in
    `model/break/*_t.bnd`, 11; the PS3 has `.tpf` there). Left: FLVER / TPF / vertex errors
    (task 2) and 9 PS3-only exception rows (task 4).
  - **PS3 vs 360 data** (`private/tmp/sheetdiff.py`, `paramdiff.py`; outputs
    `private/tmp/{sheetdiff,paramdiff,acparts}_ps3_x360.txt`): same 160 types, layouts and
    def versions. Value differences: `acvparts.bin` 112 records (legs: 5 `turn`, 14
    reverse-joint `walk` -120..-900 with `std_gravity` 0.02 to 0.033 and `max_load_downer`
    0.85 to 0.83; generators `power` / `weight`; weapons `weight` +30..+350, `init_speed`
    -10..-250, `reload_time`, `missile_lock_time`, `en_drain`, `no_decay_range`,
    `damage_power`), `growpartsarmunitparam` 66 rows (shootPrecision / initSpeed),
    `growpartsbladeparam` 6, `hometowninfo_as` areaId (cn/en/kr), `partsshoplineup_trial` 8,
    tuning `MenuParam` (4) and `ServerSystemParam` (2). Only on the PS3: `param/bullet{arise,
    blade,explosion,missile}.bin` (copies of `bullet/*`, 723 rows), maps m1640 / m7540 /
    m7770 with their online maps, enemy e9120, object o7775, `font/s1_ps3*` (360:
    `s1_xbox*`). Only on the 360: the 3 menus above. `airesource/AIAcWeaponChipParameter.bin`
    differs only in case.

- 360 disc VFS (`acvd-formats`: `xdvdfs`, `bhd5`, `bnd3::read_bhf3`, `dcx` DFLT, `vfs::Disc`).
  - **`vfs::Disc`** opens a dump directory or the 360 ISO (`vfs::X360_ISO`); every consumer
    (`acvd-render`, `acvd-game`, `acvd-viewer`, `acvd-menu`, `acvd-sheets dump`, the
    `acvd-formats` examples) reads through `Disc::read` / `asset` / `exists` / `list` / `files`
    instead of `std::fs` on a `USRDIR` path. `--disc` takes either; `vfs::default_disc` is the
    PS3 dump while present (see Open, 360 migration).
  - **ISO lookup order**: loose XDVDFS file (33: `bind/*`, `default.xex`, `movie/jp/*.wmv`,
    `$SystemUpdate`, `NxeArt`), then BHD5 path hash in layer 0 then 1, then `script/<name>`
    in `bind/script.bhd` (BHF3, format 0x2c: names + sizes, no ids, 0x14-byte entries; 1303
    zlib Lua entries named relative to `script/` with `\`; data in `script.bdt`, BDF3 header,
    offsets absolute). Listings come from `private/x360/dvdbnd_names.csv` (the 2,283 rows that
    had no path are named since "360 extract") plus the script names and loose files.
  - **DCX DFLT** (360): same `DCX`/`DCS`/`DCP`/`DCA` header as EDGE with `0x14` = 0x2C, `DCP`
    `DFLT`, `0x20, 0x09000000, 0, 0, 0, 0x00010100`, `DCA` size 8, then one zlib stream of
    `compressed_size` at 0x4C (ends at end of file on `am0010_m.bnd.dcx`: 0x4C + 0x38d21 =
    232,813 bytes). 8 files on the 360 disc are still EDGE.
  - **Check**: `cargo run --release -p acvd-formats --example disccheck` reads all 15,961 named
    360 files and expands every DCX (9,218 DFLT, 8 EDGE) and every member of the 3,740 BND3
    binders: 0 failures. Unit tests cover both readers; `vfs` disc tests read
    `param/accolor/color5001.bin` (856), `movie/jp/tu_boost.wmv`, `script/acctrlparamcalc.lc`
    and `am0010.flv` from the ISO; `ffx::disc_effects` now runs on the 360 effect binder (all
    FFX parse unchanged). `acvd-game --disc <iso>` loads map m3100 collision (84,864 hit
    triangles, same as the dump) but no AC (FLVER), textures or HUD font yet.

- Static fact index, sheet audit, and the move to 360-only RE (`tools/acvd-index`).
  - **Index**: `cargo run --release -p acvd-index -- build` (or `tools\analyze-x360.ps1`) reads
    `ACV2.pe` in about a second into `private/index/*.csv`: 113,722 functions (78,663 `.pdata`,
    35,059 leaf: `bl` targets, data pointers and `lis`/`addi` code pointers at a function
    boundary), calls, address constants (floats and strings decoded), immediates, field
    accesses, 505 switch tables, virtual calls, 234,862 function pointers in 7,434 tables, and
    2,487 names (PARAMDEF offsets, `.dbp` labels, Lua param ids). Query with `acvd-index q ...`
    (forms in `.cursor/rules/decompile-view.mdc`); never read whole CSVs into a chat. Spot
    checks: movement vtable `0x8208fea0` `+0xec` = `0x82826058`; integrator `0x82822ea0` called
    from `0x82824078`. The task's "548 switch sites" counted 68 indexed virtual tail calls
    (`lwzx` + `mtctr` + `bctr` over a vtable), not switch tables.
  - **Audit**: `acvd-index audit` checks every 360 address in `sheets/*.csv` (function, inside
    a function, referenced data), `vtable X slot +Y = Z` claims, and the offsets / floats written
    after a function address, into `private/index/audit.md`. Now: 306 addresses, 0 mismatches,
    0 slot mismatches, 0 PS3 citations. The 196 "offset not in the cited function" notes are
    mostly offsets of a caller's or callee's struct; check one by hand when a row depends on it.
  - **PS3 to 360**: the one-time pass matched the 156 PS3 addresses cited in the sheets to 360
    functions (token overlap plus call-graph anchoring, then each kept match checked by hand on
    its offsets and constants; `0x828590f8` and `0x8249e840` stay instruction-pattern matches,
    marked so in their rows). Every sheet row now cites 360 addresses; facts with no 360
    address found say "360 address pending", values from earlier emulator captures say "Xenia
    re-check pending" (open task). Preflight (`evidence.ps3`) rejects RPCS3 / EBOOT / TOC /
    01.02 / PS3 `FUN_00`/`FUN_01` citations in any sheet cell. PS3 RE tooling is gone
    (`analyze-eboot.ps1`, Ps3GhidraScripts and RPCS3 in `setup-tools.ps1`, the rule fallback).
  - **BHD5 names**: `private/x360/dvdbnd_names.csv` (archive, hash, size, path) from hashing
    the PS3 dump's 18,861 USRDIR paths with the `/`-rooted path hash: layer0 11,458 / 13,479,
    layer1 3,168 / 3,430 named (86.5%). Extension swaps (`.pam` to `.wmv`, `ps3` to `x360`)
    named none; the PS3 paths with no 360 hash are 1,298 Lua (in `script.bhd`), 972 `.param`,
    515 `.fmg`, 343 extensionless and 200 `.dcx`.
  - Helpers (one-time, `private/tmp`): `ps3cites.py`, `ps3to360.py`, `bhd5names.py`,
    `bhd5more.py`, `rewrite360.py` (the sheet edits), `requote.py` (keeps unchanged CSV
    records byte-identical), `idxfind.py` / `idxhas.py` (offset queries over the index).

- Socket facing and hanger racks (`acvd-game::assemble`, `sheets/assembly_slots.csv`).
  `param/acattachinfo.bin` is 30 records of 16 bytes at `0x10` (string table at `0x1F0`):
  parent category, socket, child category, flag, name offset, then `u32` 0, `u16` extra.
  The flag byte is switched at 360 `0x8288cea0`. Flags 0/1 (`0x8288c0b8`) build a basis on
  the socket forward against world +Y; flags 2/3 (`0x8288bef8`) against world +Z, so a
  booster nozzle (local -Y) lies along the socket forward. Each booster root uses its own
  socket (`l_boost` 8, `l2_boost` 25, `r_boost` 9, `r2_boost` 26); a missing 25/26 copies
  the inner booster's place. Hanger racks are the fixed models `hgl0001` / `hgr0001` on arm
  dummies 80/81; the hanger weapon then sits on rack dummies 85/87 with the rack's rotation
  (flag 4 shares the unread VMX path, so it copies the rack instead). Checked on designs
  5013 and 6003 from the front, side, back and three-quarter.

- Sound, the first free-play cues (`acvd-formats::fsb` / `::fev`, `acvd-game::sound`,
  `sheets/sound_cues.csv`). PS3 banks are FSB4 MPEG; the 360 build names the cue.
  - **Names**: `FUN_82b47758` sprintfs from the table at `0x8371c688` (`%03d`, `c%08d`,
    `a%08d`, `b%08d`, `w%08d`, …). Play only when the id is > 0.
  - **Shoot**: `FUN_828995d0` formats category 4. The id is `acweaponsoundparam.shoot` of the
    part's category-10 `hit_id` (`FUN_82892790` / `FUN_82891e68`). No row: `FUN_82891e18`
    writes shoot `0x12B`. `≤ 0` is silent. `w00000034` is two `se_weapon` layers, both played.
  - **Boost**: `AcSfxCtrl` vtable `0x82098C84`, slot `0x82098cb0` = `FUN_82899280` plays
    `b00000000` (boost start). The sustain loop is `b00000010` (`main_boost11`), chosen because
    it is the single-layer main boost; which state calls which slot is not traced. Gate matches
    the booster VFX (boost and horizontal speed > 0.05 m/tick).
  - **Jump**: `FUN_82899168` formats category 1 from `AC_SOUNDPARAM_ST +0xA` (`se_jump`). Every
    `acsoundparam` row is 24, so the cue is `c00000024`. Rising edge of airborne with `vy > 0`.
  - **Playback**: symphonia decodes the MPEG payload to a WAV (Bevy's default audio feature
    has no MP3 decoder). FMOD pads each MPEG frame to a multiple of 4 bytes; that pad is
    stripped first. Not positional — `FUN_82b46df0` takes a position, left for later.
  - **Left**: reload / charge / fly loop, footsteps (`se_walk` is 0), `b00000003` speed
    crossfade, `b00000006` dash (`FUN_828992d0`), 3D, reverb, FFX action 68, XMA.

- FFX effects. `acvd-formats::ffx` reads the DLsE tree (layout in the module doc) and all
  1871 effects of `sfx/acv_commoneffects.ffxbnd`.
  - **Player** (`acvd-game::sfx`): static nodes draw billboards, sprites, sfx_m models and
    point lights; clusters 2023/2032/2034 and the 10003 spark burst draw one batch mesh each,
    with flipbooks, colour keys, cone emitters, gravity and drag. Slot readings are in
    `sheets/ffx_actions.csv`.
  - **Frame grids**: columns, then total frames. s5009 (1024×128) with (8, 8) is 8 cells of
    128; s1020 (512×64) is 8 cells of 64; s4021 with (1, 4) is 4 rows.
  - **Effect points**: FLVER dummy colour byte 1 gives the effect point (`Rig::effects`),
    spawned as `EffectPoint`s with +Z along the dummy's forward.
  - **Boosters**: `mapsfxparam.bin` row 0 (main 300, back 302, foot 301) plays on bs nozzles
    31-34 and leg points 25-28 / 21, 23.
  - **Weapons**: the `acweaponsfxparam.bin` row is the part's `hit_id` (fallback row 1). The
    muzzle effect plays at weapon point 101, the bullet effect on the shot, and the
    `bullethitsfxparam.bin` `default2` effect at ground hits.
  - **Testing**: `--sfx <id>` previews one effect; `--burst <n>` saves `n` shots 0.05 s apart.
    The user confirmed boosters, muzzle flash and hits on screen.

- Hand-weapon fire (`acvd-game::weapons`): `acvparts.bin` category 10 fields `magazine`,
  `missile_lock_time`, `bullet_id`, `hit_id`, `init_speed`, `en_drain`, `reload_time`,
  `no_decay_range`, `damage_power`, `damage_recoil` (`sheets/ac_part_fields.csv`, matched
  against `growpartsarmunitparam.bin`). F / left mouse / R2 fire the right arm, C / right
  mouse / L2 the left (button defaults from `manual.fmg`). Tracers leave the weapon-root joint
  along the follow camera's pitched forward, fly with the `bulletrigid` / `bulletenergy` row
  of `bullet_id`, and despawn on the ground or after 5 s. `weapon_kind` (+0x0f) and
  `ready_position` (+0x153) are on the same sheet. A hand `_a` binder's `a00_000`/`001`/`002`/`003`
  are stowed, deploy, fire and stow. Weapons with `ready_position` and `a00_001` (cannon 2010,
  autocannon 1210, H.E.A.T. cannon 2230) stay on `a00_000` until fire is held, then play `a00_001`
  and do not shoot until it ends; release plays `a00_003`. `a00_002` restarts on each shot
  (handgun 410). A ready weapon with no weapon deploy clip (howitzer 2310) still waits for
  the body stance. While a ready weapon is deployed, that side's arm `a00_001` (stow `a00_003`)
  poses the shoulder pads the clip animates. Its other tracks are a two-key identity hold and
  are not written, so the body stance keeps the arm. Tank sets (`acv_t`) do not ship anims
  75/76/78/79, so a tank's ready weapon deploys the gun without that body clip.
  Every ready weapon (part 2010 cannon, 2230 H.E.A.T., 2610 sniper cannon) plays
  `sniper_ready_r`/`_l` (acmotion 93/95) to the end before the shot and `sniper_stow_*`
  (94/96) on release. A sniper rifle (kind 5, part 2410) does not take that stance.
  A rifle (kind 4) adds the `gun_$(LR)` ReactAng kick on arm01-03 (−12°) and arm05 (−5°) for
  6 frames, as a local X rotation.

- Movement units: every NewAcBehavior input the runtime reads has its unit and 360 evidence
  (`tuning_fields.csv`); part stats + `AcCtrlParamCalc.lua` formulas live in `ac_ctrl_calc.csv`.
  Field 438 (0.01) is the over-max decay: above the current max the integrator `0x82822ea0`
  (slot +0xec = `0x82826058` "speed > max") only steers and scales speed by 0.99 per tick while
  moving, floored at the max; at or under it, it adds the acceleration and clamps. Builder fields
  218-226 / 257-261 are defaults that the per-AC Lua params overwrite (movement struct = object +4).
- Animation blending (`sheets/anim_blend.csv`): state clip changes crossfade per bone group
  from `acanimhokan.bin` row `motioncollate.HokanParamID + acmotion.InterpolateID` (tanks use
  block 500). Frames are assumed 60 Hz, unverified. 360: `0x8249e840` `AcMotion_loadCollate`
  (HokanParamID to struct +0x84). Dead ends: 400/500 immediates (TAE event types), the
  `hokanparam` string at `0x820b3374` (per-model resource getter `0x82ca5aa8`, not this param),
  row getters through `0x82330c68(mgr, table, id)` (no consumer reads the hokan row at fixed
  offsets). Helpers: `private/tmp/x360{half,imm,loads,argcalls,rowuse,hokanid}.py`.
- Map collision: `acvd-formats::hmd` + `msb::parts`; `acvd-game` default `--map m3100` stands
  on disc hit meshes (`sheets/hmd.csv`). `--plane` / `--water` remain. Triangle vertex =
  `u16 >> 1`; nodes 0x34 bytes; MSB rotations are degrees through `flver::Xform::local`, then
  X-mirror. m3100 materials are sand/concrete/iron/clay/plyerhit/ackickrhit/camera.tga.
- Sheet pipeline: extract / preflight / gen; every PARAM row round-trips byte-for-byte.
- Formats: DCX (EDGE), BND3, FLVER2 (8820/8828), Edge indices, TPF (BC1/BC3), `.ani`, `.dbp`
  tuning binaries, `acvparts.bin`, FMG, CCM/CCF.
- Viewer: browses and textures every FLVER.
- Game: assembles preset designs (frame, weapons, booster, shoulder), skins to one skeleton,
  plays the legs' motion set; piloting with 60 Hz movement from `AcCtrlParam` and build weight;
  state-driven clips (`sheets/ac_states.csv`).
- Follow camera (`sheets/camera_follow.csv`): behaviour/offset rows, follow easing, pitch,
  sideways roll, per-state camera action (FOV, eye rates, move shake, EyeDistance / EyeOffsetY /
  EyeOffsetX eased over EyeFadeInFrame), base point at the waist (`center` bone), look-at pushed
  1000 m out before the eye lift and the side shift (eye on the AC's right, flipping with the turn
  direction), eye floor above water. Standing framing checked against an earlier emulator spawn
  shot and memory capture (rows `look_at_height` / `base_transform`; Xenia re-check pending: probe
  `CamCtrl_place` `0x8285ff28` after spawn and log the placed eye and look-at).
  Delay follow, side-moving and dash inversion traced but unused in free play.
- Speed blur (`speed_blur` / `speed_blur_draw` in `sheets/camera_follow.csv`): CPU ease 360
  0x82bfa9f0 / 0x82bf9c98 from horizontal and vertical km/h × the action's BlurRate; filter
  block 0x83a8b1e0+0x660; zoom-blur draw 0x82c98368 (centre rectangle copied, 10-tap frame).
  `acvd-game` draws it as a Bevy `FullscreenMaterial` (`blur.rs`, `zoom_blur.wgsl`); boosting at
  125 km/h gives intensity 0.82. The zoom-blur pixel math was read from the PS3 disc's shader
  binder; re-read the 360 `ZoomBlur_*` Xenos shaders (`private/tmp/xenosdis.py`, filter shader
  name table at 0x8371e978) in the 360 data migration.
- Tooling: ghidra-cli bridge on `private/ghidra360cli` with all 78,661 `.pdata` functions defined;
  camera functions renamed `CamCtrl_*` / `CamFx_*`.
- Text: FMG reader (`acvd-formats::fmg`); 1250 UTF-16BE banks, 15 Shift-JIS `partsname_*.fmg`.
- Fonts: CCM/CCF reader (`acvd-formats::ccm`); versions 0x10000/1 (24-byte glyphs) and 0x10002
  (28-byte). `acvd-game` draws `fontdef.xml` ID 1 (`e1_ext`) plus `partsname_en.fmg` as a HUD
  overlay. DRB: `acvd-formats::drb` decodes dialogs (GLD), objects (OGLD), shapes (Sprite, MonoRect/Frame, GouraudRect/Frame, Text, Dialog, Null) and textures on all 69 layouts; the module doc has every record layout. Sprite texture ids >= 1000 are runtime slots (emblems 10000+, movies 101xx, maps 102xx). `acvd-render::menu` + the `acvd-menu` viewer (`cargo run -p acvd-viewer --bin acvd-menu -- staffroll`) draw a dialog tree with its sibling `.tpf.dcx` textures; verified on staffroll (rotated strip seamless) and vssortie timer / Data_Rule (flipped corners and arrows). Text (360 `DrbShape_createText` 0x824ac270): byte +0x15 font (`fontdef.xml` ID, read by `acvd-formats::fontdef`), +0x16 align (low 2 bits left/right/center, 0x8 vertical center), +0x17 mode: 0 static RTS string at +0x1c, 1 message (bank +0x1c, id +0x20; 36-byte record; bank 1 = `menu.fmg`, e.g. PauseLabel 0x109a = PAUSE; `TextMgr_getMessage` 0x82b1a8b0, bank 2 code-filled), 2 runtime (capacity +0x1c), 3 special classes. `acvd-render::menu` draws them; fonts load the exact `CcmFile` (e10 ships a .ccm and the .ccf fontdef names, with different advances). Sprite blend byte: 1 alpha, 2 additive (`menu::AdditiveSprite`; `piece1/2` and `rocksight_insight` are art on black that only works added). A Dialog shape's color is applied as a multiplicative tint of its sub-dialog: inferred from `ACV_FE_LockSightCenter`, whose digit plates are the white `FE_font_base` nine-slice under `000000ff` Dialogs (360 Dialog factory 0x824ac1f8 is shared with FormSprite; the tint is not traced). In-game HUD (`acvd-game::hud`): `sortie.drb.dcx` `Top_outline` + `ACV_LockSight_base` + `ACV_FE_LockSightCenter` (laid out around (0,0), placed at screen center), letterboxed 1280x720; ammo digits `LWep*` / `RWep*` follow `Armament` (digit sprites are authored '0' at (317,135)-(330,151) of `ACV_FE_Locksight_02`, 0-9 in 13-texel steps; read with `acvd-menu sortie --atlas ACV_FE_Locksight_02`).

- Animation timing: clips play at 60 fps (was 30, too slow) and acanimhokan blend values are read as milliseconds (was 60 Hz frames, 2-20 s fades). User-confirmed walking looks right; still unverified against the 360 code.

- Dash / air-move lean: acmotion rows 17-40 (dash) and 68+ (air move) all use anim 55 / 56 (a01_055/056, 360 frames, a key every 45 = the 8 directions, wrap at 360); the frame is a heading in degrees clockwise from forward, not time. acvd-game poses it from the body-relative stick angle (Motion.wheel, eased at an assumed 540 deg/s). 360 code that picks the frame and its rate not found yet; dash-jump charge (anim 57) is also a wheel clip, not wired.

- Lean command RE (360): ground dash start builder 0x82883870 (called from vtable fn 0x8284b210; turn arg 0/1/2 adds 0/1/2 to the row id, ids 0x11,0x14,... = acmotion rows 17,20,..), quick-boost 360 builder 0x828844f0 (row 218), air-float 0x82883d30 (row 222), dash-jump charge 0x828839d0 (rows 49-56). Each takes the move vec2, angle = atan2 (0x823a5d20) wrapped to +-pi, fraction = angle/2pi (+1 if negative), start time = 1 - fraction (mirrored left/right, matching the by-eye swap), sent via 0x82883370 -> cmd struct (+4 start, +8) -> 0x82854060 -> clip time at 0x8289a750. Not yet found: whether the command is re-issued each frame, which vec2 it is (stick or velocity), and any smoothing.

- Armored Core V (retail 360, no Verdict Day extras) unpacked to private/x360/v/ACV.pe (xdvdfs + xexunpack from armoredcoredumps), Ghidra project private/ghidraV (ACV, PowerPC:BE:64:A2ALT-32addr base 0x82000000, .pdata 0x82215c00 size 0x8f2b0, 88,788 functions; start the bridge with --project <abs>/private/ghidraV/ACV --program ACV.pe, pass both flags on every command). The lean command builders are identical there: dash 0x827fbba0 (called by 0x827bc190, which also stores the move vec2 at this+8/+0xc), quick-boost 0x827fc938, air-float 0x827fbfa8, jump/charge tables; helper atan 0x823433d8, command 0x827fb5f8. Callers that pass the vec2 not found yet.
