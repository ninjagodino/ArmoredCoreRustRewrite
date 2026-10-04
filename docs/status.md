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
  fired; ready position waits on weapons). Speed blur open points: whether the per-call ease
  (not dt-scaled) runs at 1/60 or 1/30 s, the zoom-blur source width (taken as 1280), the
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
    upper body), ammo HUD.
- **UI**: DRB reader (menu/HUD layouts). **Param enums**: TDF reader.
- **Effects** (FFX), **sound** (FSB4/FEV1), **movies** (PAMF), **mission events** (EVD),
  **AI** (decompiled Lua in `private/lua`, no sheet yet).

## Done

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
  overlay. DRB layouts are still unread.
