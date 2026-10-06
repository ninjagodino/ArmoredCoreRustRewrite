# Status and task list

Each chat takes **one** open task, works it, and updates this file before ending: move it to
Done with a one-line summary, record findings (360 addresses, file layouts, dead ends) under the
task, and add any new tasks it uncovered. Per-system data/runtime status also lives in
`sheets/systems.csv`; keep both in step.

## Open

Rough priority order; reorder freely.

**Goal: a playable AC test demo** (garage AC TEST: map m4000 + `m4000_actest.msb`, see Done
"AC test scene"). Gameplay should match the 360 build; menus may stay partial. Tasks toward it,
in order:

- **AC vs world collision** (blocks the demo): the AC walks through walls and objects; only a
  downward ground ray exists (`collision.rs`). No paramdef carries an AC body size (`q name
  Radius` finds only bullets, `ENEMY_GRAPHICS_ST.CollisionRadius` and camera `HitRadius`). Find
  the 360 AC-vs-map query: the ground ray filter 0x100002 (`0x829e7b68`) and the eye-floor ray
  0x200002 (`0x829e7be0`) are in the collision layer, so their neighbours / callers from the AC
  update `0x828bef18` should include the horizontal sweep and its shape (capsule / spheres per
  part?). Then a Xenia probe walking into a wall. Also wall kick (`ackickrhit` material) and
  slopes too steep to stand on. `Collision` now has an XZ grid (16 m cells) for the queries.
- **AC test targets**: `m4000_actest.msb` parts of kind 2 (22: ACs `a0000`-`a0003` at the vs
  UNAC start (-797.168, 17, 616.934), enemies `e0010` / `e0110` / `e0210` / `e1020` / `e2030`)
  on layers `actest2`-`actest8` (LAYER_PARAM_ST; the layer link in the part record is unread),
  with `param/actestdata.bin` (+ `actestdata.def`, unread) choosing the test; AI in
  `script/enemy/*_actest.lc`, `script/arena/actest_*.lc`. Draw them, take hits and damage.
- **Damage model**: AP, bullet hit vs AC / enemy hit shapes, `damage_power`, impact, the
  weapons' open units (see Weapons leftovers).
- **Lock-on / FCS** (see FCS / lock-on HUD), energy and boost gauges (UI).
- **Map look**: map lighting / fog / sky (`ch_env/m4000_env.msb` is not an MSB: another format,
  77 of them), LOD (`_l1` / `_l2` FLVERs), broken models (`_b.flv` / `_b_h.hmd`), the `movie`
  texture (a runtime video surface), the area bounds (POINT kinds 100-102: operation / warning
  / caution area) and the water return point (kind 50).

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
  zoom-blur source width (taken as 1280) and the filter fade at +0x54 (taken as 1).
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
- **Paint**: map `_c` mask regions to accolor channels. Shader side decoded from the 360 build
  (see Done); open: which accolor channel feeds each `FC_AC_OverlayCol` slot (c180..c188,
  slots 0-6 reachable). Leads: accolor loader `0x82331478` (reads `$(Data)\Param\AcColor\color%04d.bin`
  into an 0x358-byte struct; cache wrapper `0x82331680`, called from `0x82331bb0` and
  `0x8249ca7c`); the code that writes PS constants 180-188 is not found (a Xenia dump of c180-188
  during an AC draw is the quicker route). Then apply it in `acvd-game` (overlay blend, a
  `StandardMaterial` extension or custom material over the diffuse map).
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
    `a00_001`/`a00_003` play on that side's bones while a ready weapon is deployed.
  - Arm raise / aim (done, separate from the kick; `weapons.rs` `ArmAim`). Probe file
    `private/xenia/armraise.txt` (user fired right rifle, left sniper rifle, then held fire):
    `AimArm` `0x8284a570` is sent every frame (r4 1 = right, 0 = left; slot 0x0a / 0x0b) from
    the weapon method `0x828a2488` (vtable `0x8209a5c8` +0x184, weight 1.0), starting about
    6 frames before the shot (at the trigger press) and stopping about 1.0-1.17 s after the last
    shot. Raise blend `0x8287f448` f1 = 10 frames; on frames with no aim, `0x82853660` releases
    slots 0x0a/0x0b through `0x8287f5a8` with f1 = 60 frames (the lowering), and `0x8289d738`
    releases slot 1 with 120. The rewrite holds the aim 60 frames after fire is let go, blends
    the weight up over 10 and down over 60, and turns `*_arm01` (parent space, over the clip
    and under the kick) so the barrel (weapon root to muzzle point 101) lies along the aim,
    clamped to that joint's LockMin/MaxX (85°). Ready-stance weapons are left to their body
    clip. Open: the hold timer inside the `+0x184` caller (no static call site found; probe
    `0x828a2488 lr` next run), and the solver's TraceAng→LockAng posture blend on the lower
    joints (`r_arm02..05`), not reproduced. Chain found statically: AC vtable
    `0x82044fc0` +0x90 = `0x8284a570(unit, target point, f1 weight)` → `0x8289e808` →
    `0x8289e468` → `0x8287f448` (activates the control with blend time from tuning record
    `0x82330c38` index 0x10 field +4, via `0x82bf1778`) → `0x82befb98` → `0x82bef6e0`
    (control +0x28 = weight, +0x2c = dt, per-object target at runtime +0x80). Solver
    `0x82cb4d98`: base angle = TraceAng + (LockAng − TraceAng) × weight, then aimed at the target
    within LockMin/Max X/Y at LimitRotAngVel 180°/s. Next: the virtual `+0x90` caller that
    computes the weight and the lower-after-idle timer.
  - Shot kick (done, see Done): read from `param/jcondata.bin` (`acvd-formats::jcon`), not the
    stale `jcondata.xml`. Open: (1) which weapons take `sniper_$(LR)`: bit 31 from `0x82456f68`
    is set when `0x82479820` (weapon record byte +0x13c == 3) is false and `0x82475ac0`
    (sub-record byte +0x1f) is true; those records are not mapped, so `weapon_kind` 5 stands
    in. (2) The joint-layer blend: `0x82c42048` sets anim-player +0x5c (ReactTime when the ramp
    starts, 30 when it ends) and zeroes +0x60. The code reads that as a linear ease from the
    current angle over that many frames. On the 360 the ReactTime switch only fires when
    `clock - delay == 0` exactly, so with delay 5 and 2-frame steps it may never fire.
    `0x82beefdc` also scales anim-player value +0x2c by 0.3 (`0x82016f20`) while the weight
    is 0; meaning unknown. Xenia probe recipe (user firing a rifle):
    `0x82bee808 kick_start r3 f1 f2 lr`, `0x82beeb38 kick_weight max=400 r31 f30 [r31+0x8]:f32 [r31+0x10]:f32`,
    `0x82c42048 blend_time max=200 r3 f1 lr`, `0x8287f348 gun_ctrl r30`,
    `0x8287f33c sniper_ctrl r30`.
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
- **Sheet values from the PS3 balance data**: the generated data (`acvd-data`) now comes from the
  360 disc (migration 4), so it matches the Xenia probes. The two discs differ in 112 of 622
  `acvparts.bin` records, 95 rows of 6 PARAM files and 6 tuning values
  (`private/tmp/acparts_ps3_x360.txt`). Sheet rows or code comments that quote a number derived
  from those fields before the flip (legs `walk` / `turn` / `std_gravity` / `max_load_downer`,
  weapon `weight` / `init_speed` / `reload_time` / `missile_lock_time` / `en_drain` /
  `no_decay_range` / `damage_power`, generator `power`) may quote the PS3 value: re-derive them
  from the 360 rows. Which disc is the later regulation is not known.
- **Unnamed 360 entries left**: 38 TPF (`_unknown/image/<hash>.tpf.dcx`, sizes 5-370 KB, often
  in identical pairs), 3 DRB menus (`_unknown/lang/<hash>.drb.dcx`) and 1 PNG. Not found
  under `/lang/<l>/menu/`, `nowload/`, `model/image/`, `image/` with the exe / Lua / named
  stems (`private/tmp/guess360d.py`).
- **Normal maps are not decoded**: codes 23 (DXN: BC4 x then BC4 y, uploadable as BC5
  after `tpf::xenos::untile`) and 24/25 (CTX1: needs a CPU expand to RG8 or a BC5 re-pack)
  are identified only (`texture_formats.csv`; Python decoders in
  `private/tmp/t2/fmtguess.py`). Nothing renders normal maps yet; when it does, rebuild z
  from x/y. A few `*_n` maps are still BC1/BC3 (the e9110-e9113 PS3 copies, B = 0 or 255).
- **Xenia re-checks of earlier emulator captures** (rows say "Xenia re-check pending"):
  `camera_follow.csv` `look_at_height`, `base_transform`, `pitch` (probe the AC+0x234 pitch
  update; its 360 address is still to find from the AC update `0x828bef18`), `follow_ease`,
  `dash_inv`, `side_moving`, `shake`, `eye_lift`. Pending 360 addresses: the per-id part
  getters behind `0x8246fbb0` (`ac_part_fields.csv`), the AC+0x1104 update gate, the
  camera-effects view builder, the offset-row lerp.
- **Movies**: the 360 ships ASF `movie/jp/*.wmv` (WMV3 video, WMA2 / WMA Pro audio, see
  `formats.csv` `.wmv`); nothing demuxes or decodes them. A pure-Rust VC-1 decoder is a large job;
  the WMA Pro audio could reuse `acvd-formats::xma`'s WMA Pro core.
- **Mission events** (EVD), **AI** (decompiled Lua in `private/lua360` from the ISO, with the
  `LogAI` state names; no sheet yet).
- **XMA decode speed**: `xmacheck` takes 33 s for `bgm` and 50 s for `se_stream` (release). Fine
  for per-cue decode at load; stream music would want a faster IMDCT (FFT) or decode-on-play.

## Done

- AC test scene: `acvd-game` now starts in the garage AC TEST map, drawn from the disc.
  - **Which map**: the 360 exe has `AcTestScene` (`garagescene.lua` `Scr_CreateScene(2008,
    "AcTestScene")`) and `AcTestSortieScene` (0x825dbcd8, load step 0x825dcb68); the disc has
    `model/map/m4000/m4000_actest.msb` (and `m4000_aitest.msb`) next to `m4000_map.msb`, plus
    `param/actestdata.bin`. The actest MSB has no terrain: 10 points (AC初期位置 start
    (-324.65, 29, 649.5) yaw 90, the vs-UNAC start, operation / warning / caution areas, a
    water return point, 3 ambient sounds), 10 layers (`actest2`-`actest8`, `normal`, `tmp`) and
    22 kind-2 parts (test ACs and enemies). The terrain is `m4000_map.msb` (550 parts: 57 map
    pieces, the rest `o####` objects).
  - **MSB** (`acvd-formats::msb`): sections are chained by header (`magic, type-name offset, n,
    n offsets`: n-1 records then the next section), replacing the backward scan (which missed
    the actest parts); `points()` reads POINT_PARAM_ST. Point kinds across the disc: 0, 1, 50,
    100-102, 200 (1790 start points), 250, 270, 500, 850, 1000, 1100, 2000, 3000.
  - **HMD** multi-mesh (`acvd-formats::hmd`, `sheets/hmd.csv`): +0x10 is the mesh count; 0x3c
    mesh records (transform, parent at +0x28, node / vertex offsets) after the strings; each
    mesh's triangles are contiguous and index its own vertices; `Hmd::model_vertices` applies
    the hierarchy. Object hit models (`o0006` 5 meshes, up to 78) now load. `examples/mapcheck`:
    349/349 MSB, 2755/2847 HMD (the rest are version 0x492, enemies only).
  - **Runtime**: `acvd-game::map` draws every map piece (`{map}_m.dcx.bnd|m####.flv`) and object
    (`model/obj/o/o_m.bnd.dcx|o.flv`) with the map's own `{map}_htdcx.bnd|{tex}.tpf.dcx` textures
    (only `movie` is missing); collision takes the same parts' `_h.hmd` (42,384 triangles on
    m4000, 16,241 before objects) in a 16 m XZ grid. The AC spawns at the layout's start point
    (`--layout`, default `actest`; yaw negated for the X mirror), facing the test area. Fix: an
    airborne ground ray now starts at last tick's height, so a fast fall off the overpass no
    longer steps through the ground. `--hits` shows the hit meshes. Checked by `--shot` at the
    start and after 6 s of `--hold w,shift`.

- Delete the PS3 paths (migration 4): every tool reads the 360 ISO by default
  (`vfs::default_disc`), and `vfs::Disc` is ISO-only (directory reader, `usrdir`, `PS3_DUMP` and
  `is_x360` gone). `acvd-sheets all` on the ISO: 0 errors, 276,168/276,168 texture and
  341,850/341,850 model checks; `acvd-data` is now generated from the 360 disc.
  - **Removed**: `acvd-formats::edge` (Edge index groups), FLVER index size 8 and the PS3 vertex
    members 0x10/3, 0x10/6, 0x2f/2, 0xf0/0; the FSB MPEG path (`fsb::MODE_MPEG`,
    `sound::mpeg_to_wav`, the `symphonia` dependency: every 360 sample is XMA or PCM16); the
    `FACE_ALIGN` cube padding and `tpf::block_chain_size`; the PS3-vs-360 survey examples
    `tpfscan.rs` / `flvbones.rs`; the PS3 branch of `tools/decompile-lua.ps1` (ISO only, to
    `private/lua360`). Disc tests (`fmg`, `drb`, `ccm`, `fsb`, `fev`, `fontdef`) read the ISO
    through `vfs::test_disc`.
  - **Kept, because the 360 disc ships PS3 files**: `model/ene/e9110`-`e9113` (`.tpf.dcx` and
    `_a.bnd.dcx`, byte-identical to the PS3 disc copies) are the 8 DCX files using EDGE
    compression (the other 9,404 are DFLT), and their 4 TPFs keep platform byte 2, 32-byte
    entries and linear BC1/BC3 chains (no cube maps). So `dcx` keeps the EDGE reader and `tpf`
    keeps a linear path: `stored_size` / `Texture::linear` for platform 2, faces back to back,
    and `block_level_span` takes no stored size. Test `tpf::tests::x360_packs_untile_or_read_linear`.
  - **Sheets**: `target.csv` title id `4E4D0864`, version 0.0.0.3, media id `0x1307F51A` (from
    `default.xex` execution info, optional header `0x00040006`), BHD5/BHF3 layout, DCX DFLT, ASF
    video. `formats.csv`: dropped `.list` / `.pam` / `.pem` / `.sdat`; `.fpo` / `.vpo` are Xenos
    shader microcode (magic `10 2A 11 xx`, 679 / 853 entries). `texture_formats.csv` and
    `vertex_types.csv` carry 360 counts, and a row no disc file uses is `sheet.stale_row` again.
    `container_exceptions.csv`: dropped the 4 PS3-only rows, added `texture.truncated` for
    `e4050.tpf.dcx#9`. `systems.csv` `disc` / `textures` / `fonts` / `ai` / `shaders` / `movies`
    rows rewritten for the ISO.
  - `acvd-game --shot` from the ISO renders design 5001 on m3100 with the e1_ext HUD.

- 360 sound, fonts, Lua (migration 3): every 360 sound sample decodes, the 360 menus draw their
  own font, the 360 Lua decompiles, and the 360 zoom-blur shaders confirm `zoom_blur.wgsl`.
  - **XMA** (`acvd-formats::xma`, LGPL-2.1+ port of FFmpeg `wmaprodec.c` / `wma.c` /
    `wmaprodata.h`; tables generated by `private/tmp/xma/gentables.py`): 360 FSB4 banks are the
    PS3 names and paths, little-endian, 112-byte sample headers; XMA samples are mode
    `0x01002020`. XMA2 data is 2048-byte packets (BE u32 header: 6-bit frame count, 15-bit
    carried-over bits, 3 metadata, 8 skip), frames a 15-bit-length bitstream across packets,
    each a WMA Pro frame with flags `0x10d6` (512 samples, up to 4 subframes, DRC byte).
    Streams = ceil(channels / 2). The first frame and 64 more samples are skipped, half a frame
    flushed, then trimmed to the FSB length. Checked bit-close against Xenia's FFmpeg
    `ff_xma2_decoder` (worst diff 1.1e-6; harness `private/tmp/xma/ref.c`), and against the
    PS3 MPEG cues (correlation up to 0.998 at the 1105-sample MP3 lag; some PS3 sources are
    resampled, and the 360 audio is about 6% louder). `examples/xmacheck.rs`: 94 banks, 3316
    samples, 0 failed, 0 frame errors. `acvd-game` plays its 4 cues from the ISO
    (`sound::tests::gameplay_cues_decode_x360`).
  - **PCM16**: 219 samples of `com_05` / `com_06` / `com_07` / `com_11` are mode `0x2130`
    (16-bit mono PCM), big-endian since the bank mode `0x48` has `0x08`; `fsb::Sample::decode`
    reads both kinds.
  - **Fonts**: `fontdef.xml` is byte-identical on both discs; its `$(Platform)` is `xbox` on the
    360 (`0x82303150` loads `$(Platform)` at `0x82001ad4` and `xbox` at `0x82001ae0`), now
    `vfs::Disc::platform`. The task note was stale: `font/e1_ext` is on the 360 disc (load
    bundles), along with `font/s1_xbox*`. `text.rs` reads font sheets the PS3-extracted texture
    rows lack from `font/<name>/<name>_t.bnd`. `acvd-menu assemble` renders the same text
    from both discs, and `acvd-game --disc <iso>` draws the e1_ext HUD labels.
  - **Lua**: 1303 chunks under `script/` in `script.bhd`, the same Lua 5.0 header as the PS3
    (`1b4c7561 50 01 04 04 04 06 08 09`, little-endian). PS3 `airesource/script/`, `scene/`
    and `param/` all sit under 360 `script/` (`param/acctrlparamcalc.lc` is
    `script/acctrlparamcalc.lc`). 373 chunks are byte-identical; 907 differ only because the 360 AI
    scripts keep `LogAI("...")` Shift-JIS debug calls the PS3 build strips
    (`acctrlparamcalc` decompiles identically). 23 are 360-only (`script/short/*`, the scene
    scripts under their new names), and the PS3-only ones are those renamed scene / param
    files. `tools/decompile-lua.ps1 -Disc <iso>` decompiles all 1303 to `private/lua360`.
  - **Movies**: 23 ASF `movie/jp/*.wmv` (WMV3; WMA2 stereo, or WMA Pro 6ch for `prologue` and
    `inisettings` at 1280x720), the PS3 `.pam` names minus `dummy_moh` / `monitor_test01`.
    Recorded in `formats.csv`; no player (Open, "Movies").
  - **Zoom blur**: the 360 `ZoomBlur_*` Xenos shaders (from `shader/filter_shader.bnd`, via
    `examples/discls.rs --members`) match the PS3-derived math; the vertex program puts
    `texcoord x 2 - 1 - g_vParamVS.zw` in TEX1.xy and the per-corner thin in TEX1.z
    (`camera_follow.csv` `speed_blur_draw`).
  - `examples/discls.rs`: list (`PREFIX`), `--get`, `--extract PREFIX DIR`, `--members ASSET DIR`.

- 360 TPF and FLVER: `acvd-formats` reads both from the ISO; `acvd-game --disc <iso>` renders
  the default scene like the PS3 dump (0.13% of pixels differ by more than 8/255, all from
  texture recompression). `extract --disc <iso>` + `preflight`: 276,160 of 276,168 texture and
  341,850 of 341,850 model checks pass (the rest: see Open, migration 4). PS3 `all`: 0 errors.
  - **TPF**: platform byte 1 on 10,552 of the 10,556 360 packs (`e9110`-`e9113` keep 2); flag2
    is 3 on all, but the 360 entry is 28 bytes with no `unk2`. Texture data is the Xenos tiled
    image (`tpf::xenos`, ported from Xenia `texture_util.cc` / `texture_address.h`): per level,
    every face's slice in turn (cube faces level-major); a slice is padded to 32x32 blocks (past
    level 0 from the power-of-two base) and 4 KB; from the first level whose short side is <= 16
    texels, the rest share one packed tail slice at Xenia's `GetPackedMipOffset` offsets;
    16-bit words big-endian. Every stored BC1/BC3/33 size on the disc matches. Checked by
    untiling: level 0 to 1x1 tail of Authenticate00 and am0010 (mean diff <= 7/255 against
    the PS3 decode), all 6 faces x 8 levels of `Global_env+c` (each face matches only its PS3
    face), and am9000 byte-identical (`tpf::tests`). `Texture::linear` gives the PS3 layout, so
    the renderer and preflight (`tpf::stored_size`, `TpfSheet.platform`) share one path; the
    renderer now takes format / size / levels from the pack (Authenticate00: PS3 11 levels,
    360 4). The two discs' textures are compressed separately (only 998 of 30,206 level-0
    images byte-identical). New codes 23 (DXN), 24 and 25 (CTX1) are all normal maps (see
    `texture_formats.csv`). `tpf::read` no longer rejects a pack whose last texture overruns
    it (e4050); `TextureRow.truncated` + preflight `texture.truncated` report it.
  - **FLVER**: 360 header 0x4A = 1, 0x4B = 1 (now `unk4b`), 0x4C = 0xFFFF, index size 16;
    face sets are big-endian `u16` strips with 0xFFFF restarts (flags 0 and 0x80000000, never
    Edge); one interleaved buffer per mesh. New members (`vertex_types.csv`): 0x12/3 normal and
    0x14/6 tangent as signed bytes stored w z y x (am0010 mesh 0 vs PS3: dot 0.994 / 0.988, w
    = PS3 bone byte), 0x12/2 bone indices reversed (`examples/flvbones.rs`, deleted in migration 4: every matched
    vertex of 7 dynamic meshes). The same 8 FLVERs as on PS3 stay pending (0x20007, FLVER0).
  - Sheet staleness for `texture_formats.csv` / `vertex_types.csv` is a `*.not_on_disc`
    warning while both discs are checked (each disc uses a different set).
  - Survey tools: `crates/acvd-formats/examples/tpfdds.rs` (DDS dumps for viewing);
    `tpfscan.rs` / `flvbones.rs` (360 vs the PS3 copy) were deleted in migration 4;
    `private/tmp/x360vsps3.py` (normals).

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

- 360 shader access and ACColor pixel shader decode (partial paint task; tools in `private/tmp`).
  - **360 archives**: `bind/dvdbnd5_layer{0,1}.bhd` (BHD5, big-endian: header `u32` bucket count
    at 0x10, bucket table offset at 0x14, buckets `(count, offset)`, entries 16 bytes
    `hash, size, offset64`) index `bind/dvdbnd_layer{0,1}.bdt`, read straight from the ISO
    (`py private/tmp/x360bnd.py get /path OUT`). Path hash: lowercase, `/` separators, leading `/`,
    `h = h*37 + c` in 32 bits (checked: `/param/accolor/color5001.bin` 856 bytes,
    `/param/coloringset.bin` 2121). Not every PS3 path exists loose; `flver_shader.bnd`,
    `filter_shader.bnd`, `static_shader.bnd`, `debug_shader.bnd` and `material/mtd.bnd` are members of
    `/bind/boot.bnd` (BND3, 1035 entries); `flver_shader.bnd` is a zlib stream whose BND3 members
    (1217) are each zlib too, `FlverShader.xml` listing material shader to VS/PS (`ACParts_g`,
    `ACParts_g_Glow`, `ACParts_Wep_g`, `ACParts_Wep_g_Glow` use the `Flver_ACColor*` shaders).
  - **Shader container** (`.fpo` / `.vpo`, magic `0x102A1100`): constant table (D3DX CTAB) at
    0x28 with register names and sampler names; the microcode is the last `u32` at
    `header[+0x18] + 4` bytes of the file; three literal `vec4`s sit just before it (c253 =
    2, 8, 6, 0; c254 = 0.5, -0.5, 0.3, 1; c255 = 0.299, 0.587, 0.114, 0.4375).
    `private/tmp/xenosdis.py` disassembles it (ported from xenia-canary `ucode.h` /
    `shader_translator*.cc`; output for the whole set is under `private/tmp/x360_fl2/`).
  - **ACColor decode** (all four variants, `Normal/Flver_ACColor*.fpo`): samples `g_AC_ColorMap`
    (s13, the `_c` mask) and `g_AC_CamouflageMap` (s15; the decal variant also `g_AC_DecalMap`
    s3). `L = R`; where `R >= 0.4375` (`c255.w`, separates levels 6/12 from 20/24/27) `L =
    lerp(R, camouflage.x, B)`. Index = `floor(L*8 + 0.3)` (levels 6/12/20/24/27 of 31 give 1, 3, 5,
    6, 7); `a0 = clamp(index, 0, 6)`; colour = `FC_AC_OverlayCol[a0]` (`c[180+a0]`, 9 registers).
    The colour is an overlay blend over the diffuse: `2*d*c` below d = 0.5, `1 - 2*(1-d)*(1-c)`
    above, mixed back by mask G (`result = d + G*(overlay - d)`), then lit as usual. Read from
    the disassembly only: `d` is the diffuse rgb times its alpha and a `FC_HTTextureRenge_0` range
    factor, the exact blend order is unverified against a render.
  - **Not done**: the CPU fill of c180-c188 (which accolor channel feeds which slot) and the
    material hookup in `acvd-game`; see the Paint item under Open.

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
  Shot kick: every hand-weapon shot kicks the arm with its `param/jcondata.bin` joint control
  (`acvd-formats::jcon`; header `u32 0, count, table offset, string offset`, 12-byte controls,
  0x40-byte objects in the runtime layout the XML reader `0x82b73b28` fills: +0x08 AxisY angle,
  +0x0c LockAng, +0x10/+0x14 LimitRotAngVelX/Y, +0x18 ReactAng, +0x1c TraceAng, +0x20
  ReactDelay, +0x24 ReactTime, +0x28-+0x3c LockMin/Max X/Y/Z). The disc's `jcondata.xml` is an
  older revision (AC4 `kata`/`ude`/`te` bones, −12° on arm01-03) and was what made the
  old kick about 3× too big. `gun_R`/`gun_L`: arm01 −10° and arm02 −2° (delay 5, time 6), arm05
  −5° (delay 0, time 6); `sniper_*`: arm01 −2° (time 6). Every arm-slot weapon is registered
  with `gun_$(LR)` or `sniper_$(LR)` (`0x8287f2d8` for slots 0x0a/0x0b/0x0f/0x10), not just
  rifles. The shot path `0x82cf8110` → `0x82847ea8` → AC handler `0x82847f18` (vtable
  `0x82044fc0` +0xa4) → `0x82bef490` → `0x82bee808` restarts each object's clock with weight
  1.0 (`0x82000fc4`) and return time 30 (`0x8210a498`). The per-object step `0x82beea90`
  advances the clock by dt × 60 (`0x8200fce0`). After ReactDelay frames the weight ramps
  0 → 1 over ReactTime frames, then drops to 0. The solver `0x82cb4d98` adds ReactAng × weight
  × π/180 to the joint (`0x8371c0e8`). The joint eases over ReactTime frames during the ramp
  and over 30 frames after it, so the arm settles in about half a second. The kick is applied
  as a local X rotation (LockMin/MaxX is the wide limit on arm01).

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
  125 km/h gives intensity 0.82. The zoom-blur pixel math is read from the 360 `ZoomBlur_*`
  Xenos shaders (`private/tmp/xenosdis.py`, now also taking vertex shaders, magic `0x102A1101`).
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
