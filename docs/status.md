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
  - Blend units open (see Done): needs one RPCS3 read breakpoint, recipe in
    `sheets/anim_blend.csv` row `blend`.
- **Camera leftovers**: impact / foot-step shake and the ready-position first-person view (both
  need triggers free play doesn't produce: RPCS3 breakpoints on the 0x68 / 0x69 handlers never
  fired; ready position waits on weapons). Speed blur: the per-call ease runs once per 1/30 s
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
- **Assembly gaps**: socket rotation, recon/hanger mounts, LOD switching; 8 pending FLVERs
  (`0x20007` and FLVER0).
- **Maps leftovers**: rest of MSB (MODEL/EVENT/POINT/ROUTE/LAYER/TREE), `.smd`, map FLVER
  visuals, water-layer materials (none named water on m3100), collision filters 0x100002 /
  0x200002. HNAV/HTR navigation readers.
- **FCS / lock-on HUD**: AcFcs lock-sight cone (20° × 20°, datum 20 m back); not a camera mode
  (see `lock_on` row in `sheets/camera_follow.csv`).
- **Weapons leftovers** (hand-weapon tracers done, see Done):
  - Units unconfirmed: `init_speed` (read as km/h), `BulletRigidSt.gravity` (paramdef label
    重力加速度, no unit; read as m/s added per 60 Hz tick, so 1.0 drops rifle 10002 rounds
    steeply), `reload_time` (read as ticks). Needs the 360 bullet update reading these, or one
    RPCS3 breakpoint on a fired round's velocity.
  - `magazine` (+0x138) name unconfirmed: may be total ammo.
  - Not done: bullet max/min speed and brake, hit/damage (`hit_id`, `damage_power`), energy
    drain, missiles (`bulletmissile`), blades, shoulder weapons (category 12 bullet ids at
    +356/+360), weapon modes, bay shift, core aim (shots follow the camera pitch, not a posed
    upper body). The lock-sight HUD shows the hand-weapon ammo counts (`acvd-game::hud`).
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
    scale argument (or one RPCS3 breakpoint on it).
  - Hits always use `default2`: the collision mesh keeps no material.
  - The `hit_sfx_type` to `bullethitsfxparam.bin` row mapping is assumed.
- **Static fact index and sheet audit** (planned, not built): one script pass over `ACV2.pe`
  writing per-function facts (bounds, direct callers / callees, the table slots that point at it,
  including leaf functions missing from `.pdata`, loaded strings / constants, struct offsets read
  and written) plus every vtable and switch table (548 `lwzx` + `mtctr` + `bctr` sites), joined
  to names from PARAMDEF, TDF (198 files, reader still todo), `.dbp` labels and Lua param ids.
  Then an audit that checks each sheet row's 360 addresses, offsets and constants against it.
  54 sheet rows cite only PS3 addresses and need byte-matched 360 equivalents.
  - Output lives only on disk: the script writes the index sheets, and chats query the rows
    they need (by address, slot or id). Never read the whole index into a chat.
  - The script does the reading; model tokens go to writing it and spot-checking samples
    against known rows (movement vtable `0x8208fea0` slot `+0xec` = `0x82826058`).
  - Budget: about a day and 200-400k tokens for the function and vtable index, about a day
    more for switch tables (only runs read by an `lwzx` / `mtctr` / `bctr` sequence count).
- **Movies** (PAMF), **mission events** (EVD),
  **AI** (decompiled Lua in `private/lua`, no sheet yet).

## Done

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
  of `bullet_id`, and despawn on the ground or after 5 s.

- Movement units: every NewAcBehavior input the runtime reads has its unit and 360/PS3 evidence
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
  direction), eye floor above water. Standing framing checked against an RPCS3 spawn shot and the
  01.02 dump `20261003_172321_044` (camera matrix 0x2188790).
  Delay follow, side-moving and dash inversion traced but unused in free play.
- Speed blur (`speed_blur` / `speed_blur_draw` in `sheets/camera_follow.csv`): CPU ease 360
  0x82bfa9f0 / 0x82bf9c98 from horizontal and vertical km/h × the action's BlurRate; filter
  block 0x83a8b1e0+0x660; zoom-blur draw 0x82c98368 (centre rectangle copied, 10-tap frame).
  `acvd-game` draws it as a Bevy `FullscreenMaterial` (`blur.rs`, `zoom_blur.wgsl`); boosting at
  125 km/h gives intensity 0.82. PS3 RSX fragment programs decode with `private/tmp/rsxfp.py`
  (wrapper: size at +4, constant patches from +0x20); BND3 entries extract with
  `private/tmp/bnd3x.py`; the 360 filter shader name table is at 0x8371e978.
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

- Runtime probes on the 360 build (`tools/xenia`). The project's Xenia Canary fork
  (`external/xenia-canary`, branch `acvd-debug` = upstream `0b0d57a1b` + `acvd-probe.patch`)
  logs registers and guest memory before chosen guest instructions, so runtime checks use the
  Ghidra addresses directly (no PS3 translation, no hand-set breakpoints). `setup.ps1` builds it
  (VS 2022, Python, Vulkan SDK); `run.ps1 <probes> [-Seconds N]` boots the 360 ISO with
  `protect_zero=false`, `readback_resolve=full` and no game patches, and writes one JSON line per
  hit (syntax in `probes.example.txt`). Smoke test at boot: entry `0x82d1ef30` hit once;
  `TextMgr_getMessage` `0x82b1a8b0` is called from `0x824ac358` (inside `DrbShape_createText`)
  with r4 = 1 (bank) and r5 = message id, matching the DRB Text notes. Xenia's compatibility
  issue #278 lists Verdict Day as gameplay; not yet run inside a mission.
