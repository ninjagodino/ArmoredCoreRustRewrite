# Status and task list

Each chat takes **one** open task, works it, and updates this file before ending: move it to
Done with a one-line summary, record findings (360 addresses, file layouts, dead ends) under the
task, and add any new tasks it uncovered. Per-system data/runtime status also lives in
`sheets/systems.csv`; keep both in step.

Runtime probes are agent-driven: `run.ps1 PROBES -Recipe tools\xenia\recipes\ac_test.txt` boots
the disc, drives the menus into AC TEST with the scripted controller and plays the recipe's
walk / look / boost / glide / quick-boost / jump steps (`docs/xenia-probes.md`; recipes
`walkabout`, `boost_vfx`, `boost_glide`). Firing is `trigger R 1 SEC` / `RB`; no committed recipe
fires yet, so the first firing run should confirm the binding with a weapon probe. The m4000
start point faces a wall. Recipes below
replace the older "user at the controls" runs; ask the user only for states no recipe reaches
(a specific story mission).

## Open

Rough priority order; reorder freely.

**Goal: a playable AC test demo** (garage AC TEST: map m4000 + `m4000_actest.msb`, see Done
"AC test scene"). Gameplay should match the 360 build; menus may stay partial. Tasks toward it,
in order:

- **Map look**: lighting / fog / sky / tone map / per-draw light-set ids, area bounds (POINT kinds 100-102), destructible map props (`_b.flv` / `_b_h.hmd`), and water return (`POINT` kind 50 + `env::Scene.water_height`) are in (see below);
  still open: the `movie` texture (a runtime video surface).
  - Done: `acvd-formats::env` reads `ch_env/*_env.msb` (MSB chain at version 0x00989A6C,
    EVENT_PARAM_ST only); `sheets/map_env.csv` has every field, the shader constants built from
    it (0x82c2bc48 lights, 0x82c2b4d8 fog, 0x82bd2268 gamma) and the Flver_ColDif pixel-shader
    maths, checked against the uploads probed in `private/xenia/envlook_mission2.jsonl`.
    `acvd-game` `env.rs` + `env_lit.wgsl` light map pieces with it; `env_sky.wgsl` draws the
    `o9705` Map_Sky dome (texture from `model/obj/o9705/o9705.tpf.dcx`) on the far plane, which
    is what makes the mountains past its ~825 m radius visible. The AC's two Bevy lights and
    ambient take set 0; clear colour from the type-101 record; camera tonemapping off.
  - Done (sky look): there is no skybox cubemap; every map's sky is one of six hemisphere
    domes `o9700`-`o9705` (radius ~822 m, horizon to zenith, MSB part at the origin, scale 1).
    The 360 sky pass (`0x827f3d38`) draws it at (camera x, part y, camera z) scaled by the
    type-101 scene sub-block `+0x1c` (debug name 天球スケール, `0x827ffa80` -> runtime +0x28;
    100 everywhere but m3710 = 5), with the projection's depth row zeroed and viewport MinZ = 1
    (`0x827d1d90`) so it sits on the far plane. Probes: `private/xenia/sky_obj.txt` (object
    matrix diag(100) at the camera X/Z), `sky_view.txt` (the zero-depth projection),
    `sky_wvp.txt` / `sky_world.txt` (camera / view block). `env.rs::follow_sky` does the same;
    the m4000 start view now matches `ac_test_start.png`. The "light-blue band" was the dome
    seen from 720 m off-centre; the texture itself is right. The camera X/Z overwrite is
    `MODEL_PARAM_ST` `+0x14` sub-record `+1` bit 7 (`0x80`, set on `o9705`) copied into
    draw_info `+6` bit `0x8000` by `0x829a5e60` and applied by `0x829911f0` / `0x8299f5e4`
    (world-matrix X/Z = camera X/Z at draw_ctx `+0x510` / `+0x518`, part Y kept). Sub-record
    `+2` is the sky-pass flag (set on water `m9100`/`m9101`, bit `0x2000` of the same word).
  - Done (tone map + light-set id): type-101 sub-block 2 (`0x827ffa80` -> `0x827fc428` ->
    `0x82800450`) feeds `m_ToneMapFilter` (`0x83a8b1e0+0x54b0`, execute `0x82c18628`):
    `+0` mode (0 Exp / 1 Divide / 2 Reinhard / 3 Filmic / 4 RGBM), `+4` lum, `+8` mul,
    `+0xc` key, `+0x10` white. FLVER output is `col * c31` with `c31.rgb = 0.5`; Divide
    (`ToneMap_DivideToneMapPS`) is `mul * x / (1 + x)` with `x = src * (1 / (lum * key))`.
    m4000: mode 1, 0.5, 1.1, 0.6, 10 (`c32 = 3.333`, `c33 = 1.1`). `env_lit.wgsl` /
    `env_sky.wgsl` apply that; bloom (`+0x1a`) is not drawn. Per-draw light-set id is
    `MODEL_PARAM_ST` `+0x14` sub-record `+0` `i8` (`0x829a5e8c`), overridden by
    `PARTS_PARAM_ST` `+0x4c` sub-record `+0` `i8` when `>= 0` (`0x829a60a8`, `-1` inherits);
    `0x829a5d00` writes it into draw_info `+0x10..+0x1c`. m4000_map.msb: `m0000..m1010` and
    `m9050..m9051` = 1, `m9000..m9018` = 2, `m9100..m9101` = 3, `o9705` = 0. Replaces the
    `env.rs::model_slot` name-range heuristic.
  - Done (bloom, `Map_Diffuse_Multi`, & `Water.spx`):
    - **Bloom (`type-101` sub-block 2 `+0x19..+0x3c`, `0x827fc428` -> `0x82800450` -> `0x82c18580`)**:
      `acvd-formats::env::Bloom` parses `+0x19` iters (`3`), `+0x1a` `bloom_enable` (`1`, checked
      at `0x82c1858c`), `+0x1c` threshold (`0.9`, `0x82c17a48`), `+0x20` offset (`0.8`,
      `0x82c17aac` -> `1/offset`), `+0x24/+0x28` separable Gaussian blur scales (`1.0`/`1.0`,
      `0x82c183d0`/`0x82c184c8`), and `+0x2c..+0x38` tint/scale (`[1.0; 4]`). `acvd-game::env`
      attaches `Hdr` + additive Bevy `Bloom` (threshold `0.9`, softness `0.4`, intensity `0.22`)
      alongside `Tonemapping::None` so `_Glow` vents, booster jets, and emissive projectiles bloom
      without double-tonemapping `env_lit.wgsl` / `env_sky.wgsl` / `env_water.wgsl`.
    - **`Map_Diffuse_Multi` (`Flver_ColDifMul.fpo` / `Flver_ColDifBumpMul.fpo`)**:
      Fixed `flver::MemberKind::Color` decoding for big-endian Xbox 360 `D3DDECLTYPE_D3DCOLOR`
      (`[A, R, G, B]` -> `[R, G, B, A]`, verified on `m9000.flv` where byte 0 is the blend alpha);
      `acvd-render::LoadedMesh` now exposes `diffuse2`, `bump2`, `ATTRIBUTE_UV_1`, and
      `ATTRIBUTE_COLOR`. `acvd-game::map` passes `MapMeshMeta` to `env::relight`, and
      `env_lit.wgsl` samples `diffuse1` at `UV0` and `diffuse2` at `UV1` directly (so vertex alpha
      never zeroes `tex1.a`, and `env_sky.wgsl` samples `sky_tex` directly too), blending
      `mix(t1, t2, 1 - vcol.a) * vcol.rgb` before squaring — `m4000` mountains (`m9000..m9017`)
      now blend naturally into rocky grey-brown/green slopes instead of flat orange-brown.
    - **`Water.spx` (`Normal/Flver_Water.vpo` / `.fpo`, `material/m4000_mtd.bnd` `Water.mtd`)**:
      `acvd-game::env` detects `Water` / `WaterRiver` / `WaterWave` MTDs (`m9100`/`m9101`, light
      set 3) and switches them to `WaterMaterial` (`env_water.wgsl`), animating 3 world-XZ scrolled
      `water_n` normal layers (`g_TileScale` `0.0019 / 0.00268 / 0.0462`, `g_TexScroll`,
      `g_BumpMapSmoose = 2.0`), Fresnel reflection (`g_FresnelPow = 70`, `g_FresnelBias = 0.5`,
      `g_FresnelScale = 0.9`) over `g_WaterColor (0.15, 0.10, 0.05)`, directional specular lobes
      from `colors_b` (`c11`/`c12`, `g_SpecularPower = 10`), distance/height fog, and tonemap.
  - Open, in order: (1) sky leftovers: which pixel shader the sky pass binds (NoLightNoFog vs
    Flver_Sky.fpo with fog; the colour already matches the 360 shot). (2) c0 / c16 / c19 / c136
    values (taken as 1, no SSAO). (3) planar water reflection render target (currently reflects
    hemisphere sky/horizon gradient).
- **AC vs world collision leftovers** (walls are in, see Done "AC wall collision"):
  - The upper sphere is built with radius = height = tuning `+0x230` (`0x82863b60`) but runs at
    radius 5.5 / height 6.0; what shrinks the radius is not found. A third sphere (`+0x10`,
    same `+0x230`, skipped when `0x8287fd60(+0x4c)`) and a static radius-6 sphere at 3 m
    (`0x82b8da24` via AC-class slot `0x828aa320`) are not modelled; neither moved the AC in
    the parapet run.
  - The lower sphere's XZ offset follows the mean of up to four leg contacts (virtual `+0x174`
    on `+0x48`) x `+0x3c` (tuning `+0x380` when `0x82479820` is true, else 1.0); runtime keeps
    it at the feet. Which legs take `+0x380` is open.
  - The 360's real resolve (sweep + slide inside `0x82ceec98` / `HitCheckerSphereGeometry`
    `0x82cf4428`) is not decoded; the runtime pushes the deepest contact out horizontally, 3
    passes a tick, skipping triangles with |normal.y| >= 0.7 (not game data).
  - Walk speed: the 360 AC TEST AC walks at 0.12 m per tick at entity `+0x130`
    (`private/xenia/wall_walk3.jsonl`), the runtime's default design 5001 at 0.249; check
    whether that is the design (AC TEST may use another AC) or a 2x step.
  - Slopes too steep to stand on, AC vs objects that move, AC vs AC (wall kick / `ackickrhit`
    filter is now in, see Done "Wall Kick / Wall Jump").
- **AC test targets**: spawning, rendering, hit shapes, AP/defense damage, destruction (`_b.flv`), AC TEST enemy & test-AC AI (`script/enemy/*_actest.lc`, `script/arena/actest_*.lc`), and player AC damage/stagger/respawn are in (see Done "AC TEST Enemy AI & Player Damage / AP"); still open: enemy skeletal animations, navmesh pathfinding (`HNAV`/`HTR`), and vs-UNAC (`actest7`).
- **Boost gauge** (Lock-on / FCS, Generator EN drain & recharge, and Damage model are now in, see Done).

- **Animation**: part `*_a` clips beyond the hand weapons, arms and boosters; overed-weapon sets
  (TAE reader, Core Control, and upper/lower body layering are now in, see Done).
  - Blend units open (see Done): find the 360 reader of a hokan row's `GeneralFrame`
    (`acvd-index q field` on the `ACANIM_HOKANPARAM_ST` offsets; `AcMotion_loadCollate`
    `0x8249e840` stores `HokanParamID`), then one Xenia probe on it while walking (row 2 = 60).
    Recipe in `sheets/anim_blend.csv` row `blend`.
- **Camera leftovers**: impact / foot-step / rumble camera shake (`0x82bfab38` / `0x8285cf88` /
  `0x8285cfb0` / `0x82bf8f00` wired on landing, TAE kind 103 footsteps, incoming hits/stagger,
  heavy ready-weapon & OW fire, and Boost Charge Kick contact), ready-position zoom camera
  (`accameraaction` row `40` `狙撃構え` -> row `49` `狙撃構え完了後` via `0x8285db68` / `0x8285d548`,
  `EyeDistance 6 m`, `FovMaxAngle` overridden by `cat-10 +0x168` `snipe_fov` via `0x8285e418` ->
  `0x8287a100` -> `0x828516b0` <- `0x82856c60` <- `0x8247b550`, aim turn speed scaled by
  `cat-10 +0x16c` `snipe_turn_scale` via `0x8287b658` <- `0x82478e00`), and 30 Hz speed-blur
  step rate (`BLUR_RATE = 30.0`, `FollowCam::blur_step` matching `0x82bfb748` -> `0x82bfa9f0`;
  the probed `BlurRate == 2` row is row `19` `ドリフト`) are now implemented in `control.rs` /
  `weapons.rs` / `damage.rs`. Remaining open points: `dash_inv` (msg `0x66` `0x8285cf48`),
  `side_moving` (msg `0x67` `0x8285cf68`), distance-attenuated rumble shakes (`0x82bf9de0`,
  `0x82bf9ee8` using `ACCAM_RAMBLE_PARAM_ST`), zoom-blur source width (taken as 1280) and filter
  fade at `+0x54` (taken as 1), which base-transform factor carries the waist offset, the writer
  of controller `+0x18`, and `FollowOffsetY` (`+0x18` of the offset row, 3.5 m).
- **Movement leftovers** (360 movement vtable `0x8208fea0`, builder `0x828212f8`, integrator
  `0x82822ea0`):
  - Non-boost air steering: struct index 0x14 (Lua param 50; not yet in `ac_ctrl_calc.csv`).
  - Glide: states 4/5 take a separate integrator path (`0x8281e408`); the runtime still brakes
    above the glide max. Done: the vertical update `0x82820168` skips the gravity loop
    (`0x8281f960`) in states 4/5 (`0x8281f3d0` at `0x828201b4`), so a glide holds its height
    and now carries on over a drop; `0x8281dd28` caps rising at glide block (+0x288) +0x0 +
    +0x2c (not modelled, a glide never rises). On the ground: `glide_start` (acmotion row 217)
    then `glide` (row 218, 360-frame wheel, booster clip 1). In the air: one directional entry
    (rows 223-230, anims 63-70, 30 frames, sector `0x82825918` / builder `0x82883c80`) then
    `glide_air` (row 222 空中浮遊, anim 62, 360-frame wheel, booster clip 3, builder
    `0x82883d30`). Camera action 26, cue b00000006 (taken as the start). Glide EN drain and
    depletion end are now in (see Done "Generator Energy (EN) drain & recharge"). The damage rows 239-242 / 246-247 are not played.
  - High Boost / Boost Charge leftovers (burst phases, charge curve, decay, re-fire, and EN drain done; see
    Done "High Boost and Boost Charge" and "Generator Energy (EN) drain & recharge"): the
    boost-off ground walk-brake gate (`0x8283fc10` -> `0x82845590` -> `0x8283f3d0`, which holds
    `+0x1c0 = 1` with `hispeed_brake_tick` until horizontal speed drops below the 80 km/h walk
    threshold instead of decaying by `over_max_decel_tick`).
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
- **Paint leftovers** (per-slot `AcColor` overlay blend, camouflage patterns, `_Glow` emissive channel, normal maps `g_Bumpmap`, `g_Specular`, and `_Rim` highlights are in, see Done "AC paint" and "Normal Maps & AC Rim / Specular Lighting"):
  - Decals (`g_AC_DecalMap` s3, composited into the dynamic `_c` A channel in `Coloring_Decal.fpo`) are not wired yet.
- **Assembly gaps**: shoulder attach flag 5 (VMX `0x8288a058`; the parallel-to-+Y case
  rotates the up hint by ±90° and that axis is still unread), recon mounts, character LOD
  (`_l2` / `_l3`; map pieces are done, see Done);
  8 pending FLVERs (`0x20007` and FLVER0).
- **Maps leftovers**: rest of MSB (MODEL/EVENT/POINT/ROUTE/LAYER/TREE), `.smd`, map FLVER
  visuals, water-layer materials (none named water on m3100), collision filters 0x100002 /
  0x200002. HNAV/HTR navigation readers.
- **FCS / lock-on leftovers**: terrain line-of-sight occlusion (`LockCheckRay` `0x829c5f20`). Core Lock-on / FCS, secondary lock-on timer, lead prediction, multi-target missile lock (`missile_lock_max`, `missileLockEffectiveRate`), target lock-on HUD cursor (`MissileLock1..6`), and Scan Mode / Target Scan / Recon Units (`ACV_ScanModeLockSight`, `ACV_Scan_Cur_EnemyParamInfo`, `ACV_MapScan_ReconInfo*`) are now in (see Done).
- **Weapons leftovers** (hand-weapon tracers, bullet units, ballistics, range-decay, shoulder weapons & homing missiles, melee blades / heat piles / Boost Charge Kick, arm firing-stability reload scaling, weapon arms, sub-munition split missiles, Overed Weapons, and hand-weapon firing modes done; see Done):
  - Bay shift is in (`acvd-game::bay`, `sheets/ac_states.csv`): hold R (△) and press that
    arm's fire button. Ready-position weapons purge instead of moving onto the rack. The
    rack holds the stow until the hand weapon is seated (frame 40), then `a00_011` turns the prong (`sheets/hanger_anim.csv`).
  - Not done: core aim for weapon fire (shots follow the
    camera pitch, not a posed upper body). OW `motion_ow_equip` body animation bank switching (`0x8284e9d8`), dynamic re-attachment of OW root `60` onto arm dummy `60` (`acattachinfo` record 1) during deployment, Overed Weapons (`acvparts` cat-11 `110..6010`, `OwCtrl` `0x82885c40..0x82886948`, `ACV_OWLockSight` / `ACV_OW_ErrorLog`), hand-weapon firing modes (full-auto, simultaneous multi-pellet spread, sequential multi-round burst, hold-to-charge / release-to-fire `0x82475b00` / `0x8247d6e0`), arm firing-stability & attack-balance reload scaling (`0x8286bd00` / `0x8286adf8`), weapon arms (`acvparts` cat-2 `+0x13c == 1` / `+0xf4` -> cat-10 `9020..9620`), sub-munition split missiles (`close_act_id == 8` / `shoot_bullet_id` -> `BULLET_BULLETARISE_BIN`), energy weapon per-shot EN drain (`en_drain`,
    `0x828a1820`), shoulder weapons & homing missiles (`bulletmissile`), melee blades & heat piles (`bulletblade`), Boost Charge Kick (`KickState`), and target hit shapes / defense mitigation are now wired. The lock-sight HUD shows AP, live energy, the
    `AlphaAnimSprite` gauges, hand-weapon / weapon-arm ammo counts, shoulder-weapon ammo (`SWep*` / `Gauge_SWeapon`), and OW lock-sight / overload error banners (`acvd-game::hud`).
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
    is 0; meaning unknown. Xenia probe list (recipe: `run ac_test.txt`, then fire the rifle with `trigger R 1 2`):
    `0x82bee808 kick_start r3 f1 f2 lr`, `0x82beeb38 kick_weight max=400 r31 f30 [r31+0x8]:f32 [r31+0x10]:f32`,
    `0x82c42048 blend_time max=200 r3 f1 lr`, `0x8287f348 gun_ctrl r30`,
    `0x8287f33c sniper_ctrl r30`.
- **UI leftovers**: DRB animation tables (PINA/KINA/OINA/MINA); runtime image slots (emblems,
  maps, movies); `NoiseSprite` and the other undecoded shape classes; Dialog color tint (inferred,
  see Notes). Open: Text word +0x0c (0, 502, 1000-1004, 1030, 1200-1204; not a scale), Text flags
  byte 1 / low half (0x0204000f ...; base ctor 0x824da1d8 passes them to 0x824d9450 / 0x824d95f0).
  HUD: the lock-sight show scale
  (StaticDrawParam +0x448 = 0.53, not applied), the weapon-gauge fill formula inside `0x829d0580`,
  hanger panels (shoulder panels, live EN, `Gauge_APEffect` damage lag/blink, and `ErrorLog_Stagger`
  are now wired; Xenia probes `private/xenia/en_drain{,2}.jsonl` confirmed HUD status EN
  `+0x10`/`+0x14` = `58630/58630`). **Param enums**:
  TDF reader.
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
  - Booster routing is traced (see Done). Still open: heat-haze distortion (action 43) draws
    as a refractive quad; the 360 shot's blown-out white core is the additive stack plus the
    engine bloom, which Bevy's tonemap does not match. QB shortage 304 and QB large 305 are
    built into lists at the sfx object `+0xb0` and the four `0x82891c50` slots, and
    `0x82892f08` never plays them (EN shortage and the `0x8283c8d0` TAE events `0xC1D..0xC22`
    from `HighBoostState` are not wired to SFX yet; Boost Charge movement and row 284 wind-up
    are done in `control.rs`). Also not
    wired: ground dust from `groundsfxparam.bin` (walk/landing rows), water splashes,
    cartridges (`cartridge_sfx_id`).
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
- **Xenia re-checks of earlier emulator captures** (rows say "Xenia re-check pending"):
  `camera_follow.csv` `look_at_height`, `base_transform`, `pitch` (probe the AC+0x234 pitch
  update; its 360 address is still to find from the AC update `0x828bef18`), `follow_ease`,
  `dash_inv`, `side_moving`, `shake`, `eye_lift`. Pending 360 addresses: the per-id part
  getters behind `0x8246fbb0` (`ac_part_fields.csv`), the AC+0x1104 update gate, the
  camera-effects view builder, the offset-row lerp. All of these are reachable unattended in
  AC TEST (`ac_test_walk.txt` / `boost_vfx.txt`).
- **Movies**: the 360 ships ASF `movie/jp/*.wmv` (WMV3 video, WMA2 / WMA Pro audio, see
  `formats.csv` `.wmv`); nothing demuxes or decodes them. A pure-Rust VC-1 decoder is a large job;
  the WMA Pro audio could reuse `acvd-formats::xma`'s WMA Pro core.
- **Mission events** (EVD), **AI** (decompiled Lua in `private/lua360` from the ISO, with the
  `LogAI` state names; no sheet yet).

## Done

- **Fast FFT-Based XMA IMDCT & Async BGM Streaming** (`acvd-formats::xma`, `fsb`, `vfs`, `env`, `acvd-game::sound`, `main`):
  - **FFT IMDCT + Fast VLC/Bitreader (`crates/acvd-formats/src/xma.rs`)**: replaced the `O(N²)` cosine matrix IMDCT with an `N/2`-point complex radix-2 FFT via DCT-IV (`ImdctPlan`, 64/128/256-point stack-allocated FFT), added a 9-bit (512-entry) fast lookup table in `Vlc::read`, and added a `u64::from_be_bytes` fast path in `Bits::peek`. Full `bgm.fsb` (26 full-length stereo tracks) decode in `xmacheck` dropped from **33 s → 10.9 s** (3.0× faster, ~0.42 s per full track, 0 frame errors, identical peak `1.105`).
  - **Single-Sample FSB Range Reads (`vfs::Disc::read_range`, `fsb::read_sample`)**: reads only the FSB4 header (~3 KB) and the requested sample's compressed payload (~4 MB) instead of loading the entire 122 MB `sound/bgm.fsb` into memory.
  - **Async Map/Garage BGM (`env::Bgm`, `acvd-game::sound`)**: parsed `EVENT_BGM` (type 400, 360 `0x82800128`) in `acvd-formats::env`, resolved map BGM cues via `sound/acv2_bgm_bgm.fev` (with garage fallback `grg` in `sound/acv2_bgm_menu_bgm_menu.fev`), decoded on a background thread with zero startup stall, and added `F9` mute toggle (skipped under `--shot`).

- **VFS Binder Cache, Indexed Data Lookups & `Arc<ani::Anim>` Clip Cache** (`acvd-formats::vfs`, `acvd-data`, `acvd-game::pose`):
  - **Bounded LRU Binder Cache (`crates/acvd-formats/src/vfs.rs`)**: added a 192 MiB `BinderCache` inside `Disc` (`X360`) that caches decompressed, BND3-parsed containers with an O(1) `by_name` entry map across all `|` nesting levels, avoiding repeated ISO reads, DCX decompression, and header parsing during map/collision load and first-use animation/SFX transitions.
  - **O(1) Generated Table Lookups (`crates/acvd-data/src/lib.rs`)**: backed `ac_part`, `ac_state`, `model`, `models_named`, `textures_named`, `assets_named`, and new `assets_in(binder)` with `OnceLock<HashMap<...>>` indexes while preserving exact disc order and public signatures.
  - **Zero-Clone Animation Clips (`crates/acvd-game/src/pose.rs`)**: switched `Motion::clip`, `UpperLayer::clip`, `Motion::cache`, and `Motion::ow_cache` to `Arc<ani::Anim>`, indexed clips by `anim_id` (`clip_by_id` / `ow_clip_by_id`) to eliminate per-transition `format!` + linear scans, and switched `pose::motion` to `acvd_data::assets_in`.

- **Destructible Map Props (`_b.flv` / `_b_h.hmd`), Water Return (`POINT` kind `50`), & Overed Weapon Deployment Polish (`motion_ow_equip` + Dynamic Arm Re-Attach)** (`acvd-formats::env`, `msb`, `acvd-game::map`, `collision`, `control`, `assemble`, `pose`, `weapons`, `main`, `sheets/systems.csv`):
  - **Destructible Map Props (`0x82c02498`, `DestroyModel` `0x82c84a60`, `DestroyObject` `0x82d0b5f0`)**: `map::spawn` detects every `KIND_OBJECT` (`o####`) whose binder ships `{o}_b.flv` (399 destructible props on `m4000`), preloads and spawns its hidden `_b.flv` child under the same `MapPart` parent (inheriting `MapEnv` relighting), and attaches `DestructibleProp` with HP and `AcObjectParam.damage_id` (e.g. explosive tanks `89110..89113`). `collision::load_map` tags intact `{o}_h.hmd` triangles with `prop_id = part_idx + 1` and preloads `{o}_b_h.hmd` triangles with `only_when_broken = true`; `Collision::break_prop(prop_id)` flips intact/broken collision in O(1) without rebuilding the BVH. `weapons::step_projectiles` / `step_missiles` (via `Collision::raycast_with_prop` + AoE blast radius) and `step_melee` (kick/blade sweeps) damage props; `map::step_destructibles` swaps visuals (`full`/`low` → `_b.flv`), flips collision, and triggers explosion FFX (`PARAM_SFXSESETPARAM_BIN` 502) + destruction SFX (`se_destroy`).
  - **Water Height & Water Return (`POINT_WATER_RETURN = 50`, `0x827ffa80`)**: parsed `Scene::water_height` (`type-101` sub-block 0 `+0x08`, `3.0` on `m4000_env.msb`) in `acvd-formats::env`. Added `map::WaterReturnPoints` loading kind-50 points from `{map}_{layout}.msb` / `{map}_map.msb` (2 return points on `m4000`) plus `water_height` (or `--water <y>` override), and `control::check_water_return` teleporting the AC to the nearest kind-50 return point (snapped to ground, velocity/dash/melee cleared) whenever feet drop below `water_height`.
  - **OW Deployment Polish (`0x8284e9d8`, `0x8284e7c8`, `acattachinfo` record 1)**: `pose::Motion` loads the `motion_ow_equip` clip bank (`model/ac/motion/motion_ow_equip/{ow_equip_ac_motion_name}/{name}_a.bnd.dcx`, anims `200..258`) and `tae/ac/motion_ow_equip/own{:06}.tae` from `MotionCollateSt` when an OW is equipped, switches banks via `Motion::set_ow_bank` while `ow.is_active()`, maps locomotion states to `PARAM_ACMOTION_BIN` OW rows (`201` idle, `211..225` walk, `226..227` turn, `228..229` boost wheels, `232..234` jump/rise/fall, `245..248` QB, `243` kick) in `control::ow_locomotion_row`, and plays OW event clips (`200` arm, `209` charge, `202` attack, `205` purge) in `weapons::step_overed_weapon`. Added `assemble::ow_deployed_place`, `pose::Built::local_for_reattach`, and `weapons::OwArmAttach` to dynamically re-parent OW secondary root `"60"` (`ow0110`/`ow0310`/`ow0610`/`ow6010`) onto arm dummy `60` (orient flag `0`) during deployment and restore stow on disarm/purge.
  - Verified with `acvd-formats` (65 passed), `acvd-game` (117 passed), `acvd-sheets all`, `acvd-index audit`, and `--shot` (`private/shots/ow_deploy_9999.png`, confirming `m4000` loads 399 destructible props + 2 kind-50 water return points + `water_height=3.0`, live enemy fire breaks `o2024` props, and design `9999` arms its OW in `motion_ow_equip` stance).

- **Normal Maps (DXN / CTX1) & AC Rim / Specular Lighting** (`acvd-formats::tpf`, `flver`, `accolor`, `acvd-render`, `acvd-game::paint`, `ac_paint.wgsl`, `map`, `sheets/systems.csv`):
  - **DXN (`23`) & CTX1 (`24`/`25`) Decoding (`tpf.rs`, `acvd-render/src/lib.rs`)**: implemented `tpf::decode_ctx1_block` & `tpf::decode_ctx1` (with full mip-chain & sub-4×4 mip support, matching Xenia `XeCTX1TwoBlocksRowToR8G8` / `texture_load_ctx1.cs.xesl`: 16-bit LE endpoint pairs `[y0, x0, y1, x1]` + 2-bit DXT weights) and wired CTX1 → linear `TextureFormat::Rg8Unorm` and DXN → linear `TextureFormat::Bc5RgUnorm` (both automatically flagged `TWO_COMPONENT_NORMAL_MAP` by Bevy PBR, reconstructing `z = sqrt(1 - x^2 - y^2)` like `Flver_ColDifBump.fpo` instructions 29–32 & `Flver_ACColorColDifSpcBumpIbl.fpo` instructions 33–36). Added `acvd_render::to_linear` / `Packs::linear_image` / `Packs::linear_texture_at` so BC1/BC3 normal & specular maps also sample in linear space. Disc-wide test `acvd_render::tests::x360_all_normal_maps_untile_and_decode` verifies all **3,933 normal maps** (`162` DXN + `235` CTX1-24 + `3,536` CTX1-25) untile & decode with 0 errors.
  - **FLVER Tangents & Material Slots (`flver.rs`, `acvd-render/src/lib.rs`, `map.rs`)**: verified against `Flver_ColDifBump.vpo` / `.fpo` and `am0010.flv` UV derivatives that FLVER `MemberKind::TangentS8` (semantic `6`) stores the bitangent `+dP/dv` with handedness `w = ±1` (`dot = +0.978`), while `cross(N, v_stored) * w` is the tangent `+dP/du` (`dot = +0.978`). Updated `Flver::to_model_space` and `acvd_render::load` to transform tangents through bone/root poses and convert them into X-mirrored Bevy `Mesh::ATTRIBUTE_TANGENT` (`T_bevy = S * (cross(N, v_stored) * w)`, `w_bevy = w`, with MikkTSpace fallback + `w` flip when absent) so `flip_normal_map_y: false` matches the 360 TBN exactly across AC parts, enemies, `acvd-viewer`, and `m4000` map pieces (`env_lit.wgsl` automatically receives normal-mapped `pbr.N`). Exposed `bump` (`g_Bumpmap`/`g_BumpmapTexture`), `specular` (`g_Specular`/`g_SpecularTexture`), and `mtd` on `LoadedMesh`.
  - **`ACParts_Rim*` & `g_Specular` Highlights (`accolor.rs`, `paint.rs`, `ac_paint.wgsl`)**: disassembled `0x82bd8198` (`_phong`/`_rim`/`_glow`), `0x82bd8958` (`0x82008994` = `10.0` rim intensity vs `0x8200b3c8` = `20.0` glow intensity), `0x82bd7bd8` (`c138` upload), `Flver_ACColDifRim.fpo` (instructions 40–46: `max(1.0 - dot(N, V), 0.0) * c138.rgb * c138.w`), and `Flver_ACColorColDifSpcBumpIbl.fpo` (`g_Specular` `s1` gloss/reflectance). Added `accolor::RIM_INTENSITY = 10.0`, bound linear `g_Specular` (`*_s`) and `_Rim` Fresnel parameters in `paint.rs` + `ac_paint.wgsl`, and verified visually on `m4000` (`private/tmp/shot_5001_normal.png`, `private/tmp/shot_9999_normal.png`).

- **Overed Weapons (OW) & Hand-Weapon Firing Modes** (`acvd-game::assemble`, `weapons`, `control`, `fcs`, `hud`, `main`, `sheets/assembly_slots.csv`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`, `sheets/systems.csv`):
  - **OW Assembly & Mounts (`param/acattachinfo.bin`, `0x8288cea0`)**: decoded `acattachinfo` record 25 `(1, 50, 12, 5, "50", 0x0100)` (core dummy `50` → OW root bone `"50"`, orient flag `5`) and record 0 `(12, 60, 12, 1, "60")` (secondary arm/blade/cannon root `"60"` on `ow0110`/`ow0310`/`ow0610`/`ow6010` rides `"50"` while stowed; record 1 `(2, 60, 12, 0, "60")` re-attaches `"60"` to arm dummy `60` on deploy). Implemented flag `5` (`rack_basis(-forward)`) and multi-root follower mounting in `assemble.rs`, added `overed_weapon` (`back_r`, cat-11, prefix `ow`, core dummy `50`, root `50`, orient `5`) to `sheets/assembly_slots.csv`, and tagged `OwHardpoint` / `OwMesh` in `main.rs`. All OW preset designs (`9002` `ow0610`, `9003`/`9004` `ow0110`, `9999` `ow0810`) assemble with zero problems and render painted with `_Glow` vents on `m4000`.
  - **OW `AcCtrlParam` Formulas & State Machine (`0x82858948`, `0x82868798`, `0x82869d40`, `0x8287ef68`, `0x82885c40..0x82886948`, `AcCtrlParamCalc.lua` lines 162–166, 253–283, 418–423)**: decoded category-1 core OW fields (`safety_begin` `+0x28`, `ow_mode_time` `+0x2c`, `vs_hi_en` `+0x30`), category-5 generator `ow_en_output` (`+0x20`), all 6 category-11 OW records (`110` Grind Blade, `310` Mass Blade, `610` Huge Cannon, `810` Multiple Pulse, `1010` Giga Missile, `6010` Mega Blade: shared weapon sub-record at `+0x134` with `damage_type = 4` unmitigated via `0x82874190` → `0x827d0928`, plus `charge_en` `+0x20`, `lockout_secs` `+0x28` `0x824776e0`, `attack_speed_scale` `+0x54` `0x82477400`, `attack_accel_scale` `+0x58` `0x82477440`, `calorific_base` `+0xf4`, `limit_capacity` `+0xfe` `0x82477660`, `arise_id` `+0x144` secondary blasts `99615`/`99815`/`101010`, `ow_status_arise_id` `+0x19c`, `ow_type` `+0x1a0`), and `NewAcBehavior` fields `234`/`235`/`242`/`309`/`400`/`518–525`/`555`. Implemented exact 360 `OwCtrl` state machine (`OveredWeaponState`, `weapons::step_overed_weapon`): hold `U` or `R`/`△` for `ow_mode_hold_frames` (`60` ticks, `manual.fmg` 20405) to arm OW & purge/stow left-arm weapon, switch generator output to `ow_en_output` (`control::step_en`), drain `limit_remaining` (`calorific_base * 60` + charge heat `en_step / heatValue_OwCharge`), roll periodic overload AP self-damage (`ow_overload_dmg`, 360 LCG `0x41c64e6d * s + 0x3039` in `[ow_dmgtime_min, ow_dmgtime_max]`), hold `F`/LMB/`R2` to charge `charge_en` (0.8/frame decay on early release `0x82885da8`), release at full charge to unleash melee dash+blade (`10051`/`10053`/`16010`), Huge Cannon (`10050` + 50 m blast `99615`), Multiple Pulse 7-beam fan (`10006` + 30 m TE sphere `99815`→`69810`), or Giga Missile (`59010` + 100 m blast `101010`→`70010`), then auto-purge (`OwMesh` hidden) into `lockout_secs` EN depletion & FCS lock-on lockout (`fcs.rs`). Wired `sortie.drb` `ACV_OWLockSight` / `ACV_OWLockSightCenter` (`0x829d4e40`) and `ACV_OW_ErrorLog` (`0x825aefe0`) in `hud::ow_hud`.
  - **Hand-Weapon Firing Modes (`0x823c7260`, `0x82475b00`, `0x8247d6e0` → `0x8247ea28`, `0x82475c98`, `0x82858638`)**: decoded category-10 `charge_time` (`sub +0x3e`, record `+0x172`, all 26 charge weapons: kind-11 laser rifles `10–109` ticks, kind-17 rail cannons `63–128`, kind-22 laser cannons `127–305`), `fire_mode` (`sub +0x03`, `+0x137`), and `burst_wait` (`sub +0x20`, `+0x154`, kind-6 burst rifles `710..6520` = `5–7` ticks). Implemented `FireMode::{Auto, PelletSpread, Burst, Charge}` and `Gun::step_trigger` in `weapons.rs`: continuous auto, simultaneous cone pellets for hand shotguns/pulse guns/pulse cannons/scatter cannons, sequential multi-round bursts, and hold-to-charge / release-to-fire with `acweaponsoundparam.charge` audio and partial-to-full power scaling.
  - Verified visually on `m4000` (`private/shots/ow_9003.png` Grind Blade `ow0110`, `private/shots/ow_9999_armed3.png` Multiple Pulse `ow0810` armed with centered `ACV_OWLockSight`, `ACV_OW_ErrorLog`, and live `Limit` gauge) and with workspace tests (`acvd-game` 107 passed, `acvd-sheets all` 0 errors, `acvd-index audit` 0 mismatches).

- **Weapon System Depth: Arm Firing-Stability Reload, Weapon Arms & Sub-Munition Split Missiles** (`acvd-game::weapons`, `assemble`, `fcs`, `hud`, `bay`, `main`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`, `sheets/systems.csv`):
  - **Arm Firing-Stability & Attack-Balance Reload Scaling (`0x8286bd00` → `0x8286adf8`, `AcCtrlParamCalc.lua` lines 343–373)**: decoded exact 360 reload-time formula applied by `0x82858638` (`0x828586a8`) to all weapon slots: `arms.shoot_stability` (cat-2 `u8 +0x114`, `110..199`) feeds `arm_shoot_stability = 1 - (shoot_stability - 100) / 300` (param 98, `+0x18c`), `legs.snipe_attack_balance_rate` (cat-3 `f32 +0xcc`, `0.08..0.38` on quads, `1.0` otherwise) + `NewAcBehavior` fields 528–531/563 (`GetExcelParam(34..37, 69)`) feed `attack_balance` (param 75, `+0x130`), `snipe_attack_balance` (param 76, `+0x134`), and `attack_stab_max_rate` (param 78, `+0x13c = 0.3`). `0x8286adf8` reads weapon `reload_stab_factor` (`0x82478b90`, `sub +0x2c`: cat-10 `+0x160`, cat-12 `+0x188`), picks `balance` via `0x8286a0b0` (`ready_position`), computes `t = (reload_stab_factor - 1) / (attack_stab_max_rate - 1)` → `(1 - t) + t * balance`, and multiplies by `arm_shoot_stability` when `!ready_position`. Wired into `weapons::reload_multiplier` and `Armament::from_design`.
  - **Sub-Munition Split Missiles (`0x828ec1a0`, `0x828ec300`, `0x828eaac0`, `0x828e9f48`, `0x828de278` → `0x828de0c8` → `0x828dded8` → `0x828ddca8`)**: decoded `close_act_id == 8` (`act[8]` in dispatch table `0x820a3850` via `0x828e95b8`) and sub-munition spawner: triggers in `0x828eaac0` once `age_frames >= force_trace_frame` and either `close_act_start_frame > 0 && age_frames >= close_act_start_frame` or target distance `< close_act_start_dist`, looks up `shoot_bullet_id` (`+0x7c`) in **`BULLET_BULLETARISE_BIN`** (`BulletAriseSt`, e.g. Spread Missiles `sh1010`/`51010` → 3× child `61010` `damage_power = 652`, `sh1020`/`51020` → 3× child `61020` `damage_power = 842`, enemy cluster missile `3521` → `5122` → 4× `3522`), spawns `once_shoot_count` (`+0x80`) children with velocity `parent_vel + launch_dir * shoot_speed` (`+0x82`) and cone spread + split VFX/SE, and terminates the parent (`+0xa4 = 2` → `0x828e9528`). Wired into `Missile::should_split`, `Missile::build_split_burst`, and `weapons::step_missiles`.
  - **Weapon Arms (`0x824793c8`, `0x82479668`, `0x8247e090`, `0x82457400`, `0x82456a10`, `0x8288e218`)**: decoded cat-2 `is_weapon_arm` (`+0x13c == 1`, all 18 weapon arms `7020..7620`) and `weapon_arm_id` (`+0xf4` `i16`, naming embedded cat-10 weapon records `9020..9620` across blades `7020`, sniper cannons `7110..7140`, missiles/rockets `7210..7230`, HEAT/scatter cannons `7310..7420`, laser cannons `7510..7540`, and pulse cannons `7610..7620`). `assemble.rs` skips separate hand/hanger/shoulder slots when `is_weapon_arm != 0`; `main.rs` attaches `Hardpoint`s to `r_armhand`/`l_armhand`; `weapons.rs` resolves the unified built-in weapon with reload stability scaling and fires from right/left `"arms"` FLVER muzzle `EffectPoint`s (even `102/104/106/108` right, odd `101/103/105/107` left) with arm aim, dual recoil kick, muzzle FFX, and sound; `fcs.rs` enables multi-lock acquisition for missile weapon arms (`am7210`/`7220`), `hud.rs` mirrors live weapon-arm ammo/gauge readouts, and `bay.rs` disables bay shifting.
  - Verified visually on `m4000` (`private/shots/weapon_arms_6009.png`: design `6009` `am7420` scatter-cannon weapon arms assembled cleanly with 5 models, firing dual bursts with live `0128` HUD ammo; `private/shots/split_missile_5013.png`: design `5013` firing Spread Missile `sh1010` splitting mid-flight at frame 45 into 3 sub-munitions) and with workspace tests (`acvd-game` 101 passed, `acvd-sheets all` 0 errors, `acvd-index audit` 1297 cited addresses / 0 mismatches).

- **Enemy Skeletal Animation & Navmesh Pathfinding (`HNAV` / `HTR` Reader)** (`acvd-formats::nav`, `acvd-game::map`, `targets`, `pose`, `ai`, `main`, `sheets/formats.csv`, `sheets/systems.csv`):
  - **`HNAV` & `HTR` Binary Formats (`crates/acvd-formats/src/nav.rs`)**: decoded big-endian 360 `HNAV` (`HNAV0002`, magic/version check `0x823fbf38`, loader `0x823ef980` → `0x823fbfc0` 4×4 `grid_to_world`/`world_to_grid` transform header at `+0x10`, `0x823fc068` node table at `+0x14`, `0x823fc5a8` `kind == 1` macro regions with 18-byte quantized AABBs at `+0x08` + `u32` CSR adjacency, and `kind == 0` 10-byte micro cells + `u16` CSR adjacency) and `HTR` (`HTR\0` + `"0001"`, `0x98`-byte header + `0x70`-byte 8-ary quantized AABB tree indexing macro regions with `0x8000` internal bit and `0xFFFF` empty slots). Disc-wide unit test `nav::tests::disc_all_hnav_and_htr_parse` verifies all **59/59 `.hnav`** and **59/59 `.htr`** files on the 360 ISO parse with 0 errors. `NavMesh` provides HTR-accelerated region lookup + A* over the macro adjacency graph + iterative portal string-pulling (`m4000`: 607 macro regions).
  - **Navmesh Pathfinding in `acvd-game` (`map.rs`, `ai.rs`, `main.rs`)**: `map::MapNav` loads `model/map/ch_nav/{map}.hnav` + `.htr` with FLVER↔Bevy X-mirror conversion; `ai::EnemyAi::apply_navmesh` routes grounded units (tanks, MTs, test ACs) along `HNAV` waypoints around walls/buildings whenever `AiLos` is blocked or a wall lies ahead.
  - **Enemy & Test-AC Skeletal Animation (`targets.rs`, `pose.rs`, `ai.rs`)**: traced enemy motion/joint-control data (`EnemyAnimCreateContextImpl` `0x8293f1a0`, `MechEnemyAction*` `0x82936208`, `param/enemyjcondata.xml` `[03] Cannon` `y`/`x`, `[07] e0210` `l_arm_03`, `[08] e0110` `body_01`, and `model/ene/{m}/{m}_a.bnd.dcx` `a00_*.ani` clip sets). Non-AC enemies now spawn as rigged `SkinnedMesh` entities with cached `EnemyAnimBank` + per-entity `EnemyAnim` driving clip selection & crossfading (MT walk/dash/turn/stagger/sniper/attack clips, `e2030` CHheli `pro_l`/`pro_r` rotor spin + directional tilt, and `e1020`/`e0110`/`e0210` procedural turret/barrel aim tracking). Test ACs (`a0000`–`a0003`) spawn via `pose::spawn_npc` with per-entity `pose::NpcMotion` playing their legs' `motioncollate` locomotion clips (`acmotion.bin` rows `0`, `9..=16`, `17` dash wheel) and Core Control aim tracking.
  - Verified visually on `m4000` (`private/shots/nav_anim_all.png`: all 22 AC TEST targets spawned rigged & animating in motion with `m4000` 607-region navmesh loaded) and with workspace unit tests (`acvd-formats` 62, `acvd-game` 97, `acvd-sheets all` 0 errors, `acvd-index audit` 0 mismatches).

- **Core Control (Upper/Lower Body Layering) & Animation TAE Event Reader** (`acvd-formats::tae`, `acvd-game::pose`, `control`, `weapons`, `bay`, `sound`, `sfx`, `sheets/formats.csv`, `sheets/systems.csv`):
  - **TAE Binary Format (`crates/acvd-formats/src/tae.rs`)**: decoded big-endian `.tae` v`0x0001000B` (plus legacy `0x0001000A` on `_unknown/tae/4246306883.tae`) verified against 360 parsers `0x8239d928` / `0x825d3c40` / `0x82a21150` / `0x82b94d10` and all **618/618 `.tae` files** on the 360 ISO (15,218 animations, 57,737 events, 0 errors). Traced 360 loader path formats (`0x8284e7c8`: `tae/ac/motion_ow_disarm/own{:06}.tae`) and AC TAE event dispatcher (`0x8289acd0`, vtable `0x820997a0` slot `+0x68`): kind `0` FFX (`[ffx_id, dummy_id]`), kinds `1`/`2`/`602` sound (formatted via 360 category table `0x8371c688`), and kind `103` footstep (`[foot_side, step_kind]`, e.g. `own000100.tae` walk `anim 2` at frames `19` and `44`).
  - **Core Control & Upper/Lower Body Layering (`pose.rs`, `control.rs`, `weapons.rs`, `bay.rs`)**: traced 360 Core Control chain `0x8284a828` → `0x8289ea40` → `0x8289e730` → `0x8287f448` (`param/jcondata.bin` `"Core"`: `bone = "core"`, `axis_bone = "c_center"`, yaw `±90°`, pitch `±10°`, `limit_rot_vel = 180°/s`, plus `head_01` lead from `0x8289e608` / `0x8289e158`; engage/release `5`/`120` frames from `param/acanimparam.bin` `0x8289e7cc`, gated by `AC_MOTIONINFO.b_disable_core_ctrl`). `pose::Motion::core_ctrl` tracks FCS locked targets or camera aim + strafe/turn counter-twist while legs dash/strafe. `Driven::upper` splits the `c_center`/`core` subtree from `k_center`/`kosi`/legs so `Motion::play_upper` layers bay shifts (`bay.rs`) and `b_apply_lower == 0` sniper ready/stow clips (`weapons.rs`) over lower-body locomotion without freezing the legs.
  - **TAE Event Playback (`pose.rs`, `sound.rs`, `sfx.rs`)**: `pose::motion` loads the legs' `motion_ow_disarm` `.tae` file alongside the motion set; `pose::animate` emits `TaeFired::{Footstep, Sound, Ffx}` on frame crossing (including loop wrap), consumed by `sound::motion` (leg-type footsteps via `MotionCollateSt.sound_param_id` + direct TAE cues) and `sfx::tae_events` (FFX spawns on matching `EffectPoint` dummy IDs).
  - Verified visually on `m4000` (`private/shots/core_control.png`: sideways boost-dash with upper body cleanly twisted to aim/fire) and with workspace unit tests (`acvd-formats` 60, `acvd-game` 94, `acvd-sheets all` 0 errors, `acvd-index audit` 0 mismatches).

- **3D Terrain Raycasting (FCS `LockCheckRay` LOS, Projectile/Missile Wall Hits, AI Sightlines) & Map Area Bounds** (`acvd-formats::msb`, `acvd-game::collision`, `fcs`, `weapons`, `scan`, `ai`, `map`, `control`, `hud`, `main`, `sheets/hmd.csv`, `sheets/systems.csv`):
  - **360 HMD collision filter & `LockCheckRay` (`ACV2.pe`, `sheets/hmd.csv`)**: traced HMD material mask table `0x8371d460` (`0x82b9db78` → `0x82b9dc38` → `0x82c52498`: `0x00` solid terrain, `0x02` `water`/`ashore`, `0x04` wire, `0x08` `camera`, `0x10` `enehit`, `0x20` `plyerhit`, `0x60` `ackickrhit`), `LockCheckRay` (`0x829c5f20` → `0x829c5e30` → `0x82b9df28`, filter `0x80100006`; mode 1 `0x829c5c00` for FCS `0x829d8d58` skips map objects with `AcObjectParam.camera_collidable == 0` `0x829aabc8`; mode 2 `0x829c5cd0` for AI also skips `AcObjectParam.b_look_line_penetrate != 0` `0x829aacb8`), and bullet/missile raycast (`0x828eea28` → `0x82b9df28`, filter `0x00100004`, solid terrain + water).
  - **Accelerated 3D BVH raycast (`collision.rs`)**: flat binary BVH built once at map load over all 42,384 `m4000` HMD triangles, exposing `Collision::raycast(origin, end, filter)` and early-out `Collision::has_los(origin, end, filter)` with `RayFilter::{FcsLos, AiLos, Bullet, Ground}` and zero per-ray heap allocation; verified exact equivalence vs brute force on synthetic scenes and `m4000`.
  - **FCS LOS, Projectile/Missile/Recon wall hits, AI LOS**: `fcs::update` gates Combat Mode primary and multi-lock missile targets on `LockCheckRay` (`RayFilter::FcsLos`) while Scan Mode + Recon-detected targets remain marked through cover (`scan.rs`); `weapons::step_projectiles`, `weapons::step_missiles`, and sticky `scan::ReconUnit::step` raycast `prev -> next` (`RayFilter::Bullet`) so shots/missiles detonate on buildings/walls with surface-normal-oriented impact FFX + hit SE; `ai::update` gates enemy/test-AC visual detection and weapon firing on `RayFilter::AiLos` (cancelling mid-burst when occluded).
  - **Map Area Bounds (`POINT_PARAM_ST` kinds `100`–`102`)**: `acvd-formats::msb` decodes `+0x38` `PointShape` payloads (sphere/cylinder/box; `m4000_actest.msb` boxes at `(-804, -15, 609.35)`: Operation `100` `[1400, 900, 225]`, Warning `101` `[1350, 850, 175]`, Caution `102` `[1300, 800, 125]`). `map::AreaBounds` + `control::pilot` cleanly clamp the AC inside the operation area (zeroing outward velocity) and `hud::area_warning_hud` displays pulsing `LIMIT OF AREA` / `OUTSIDE OF AREA` warnings (`manual.fmg` 20500, `AreaOverLine` `0x825b3678`).
  - Verified visually on `m4000` (`private/shots/raycast_wall_hit.png`: rifle shots impacting buildings with orange explosion/sparks) and with workspace unit tests (`acvd-formats` 57, `acvd-game` 90, `acvd-sheets all` 0 errors, `acvd-index audit` 0 mismatches).

- **Boost Charge Kick & Melee Blades** (`acvd-game::damage`, `control`, `weapons`, `hud`, `sfx`, `sound`, `ai`, `main`, `sheets/tuning_fields.csv`, `sheets/ac_part_fields.csv`, `sheets/ac_ctrl_calc.csv`, `sheets/ac_states.csv`):
  - **Boost Charge Kick** (`KickState` vtable `0x820928d0`, ctor `0x8283f8a8`, update `0x82844570` → `0x8283e1f0` / `0x828433a0`; `AcMove_boostChargeKick` `0x82823740`):
    - **Trigger & EN**: holding High Boost (`□ + HOLD` / `V`, `manual.fmg` 20301/20401) into High Boost Phase 2 (`0x8237bf28` → `0x8237bcb0` → `0x8283fdb0`) or while Glide Boosting (`0x82840ee0` → `0x8283fdb0`), plus direct keyboard shortcut `K`; `0x8281f278` (AcMove slot `+0xc8`) requires and drains `qb_en_drain` (`BST__HI_EN * 0.7`, Lua param 57).
    - **Movement & clip**: `kick_accel_frames = 3` (Lua param 128) + `kick_after_frames = 35` (Lua param 129) via `0x828590f8` `+0x18`/`+0x1c` → `action_ctrl +0x98`/`+0x9c`, horizontal thrust `kick_accel = glide_acc * 0.01` (Lua param 143) / `kick_max_speed = glide_max_speed * 0.7` (Lua param 21, `0x82822af0` when `+0x1b8 == 6` via `0x8281f3f0`), downward dive when pitched down (`NewAcBehavior +0x538` = 3.0 m/s per tick / 60 at `0x82823938`), plays `acmotion` row 243 (`anim 90`, `0x828845e8`), camera action 42 (`キック`), and mode 3 booster SFX at `kick_sfx_scale` (`+0x520` = 0.7).
    - **Hit volume & damage**: `BULLET_BULLETEXPLOSION_BIN` row `4100` (`init_radius = 7.0 m`, `damage_hit_count = 1`), legs base power `+0x10c` (`i16(12)`, Lua param 60) & recoil `+0x10e` (`i16(13)`, Lua param 59) scaled in `0x8286ac40` → `0x828a3470` → `0x828a33a8` (`0x828a2e50`/`0x828a20d8`/`0x828a2078`/`0x828a20a8`) by `param/damage.bin` fields 11–15 (`ref_speed = 100 km/h`, `ref_weight = 5000`, `max = 10×10`, floor `0.4×`), KE attribute, contact rebound (`0x828433b8`), target knockback/stagger/destruction, and hit VFX/SE row `99000` (FFX 135, `w00009202`/`w00009027`).
  - **Melee Blades (Laser Blade / Heat Pile / Physical Blade)** (`BladeState` phase table `0x83714024` `[0x828ae9e0, 0x828af000, 0x828af1c8]`, `AcMove_bladeDash` `0x828248f8`, `0x828a4928`):
    - **Part resolution**: category-10 parts with `bullet_id` in `BULLET_BULLETBLADE_BIN` (`weapon_kind` 12 KE Physical Blade, 13 CE Heat Pile, 14 TE Laser Blade) load as `Kind::Blade` with `BladeSpec` (`hit_radius`, `blade_radius`, `damage_hit_count_chara`, `decay_power`/`decay_recoil`, `infinite_ammo` when `magazine == 0`, and `physical` via `0x8247f170` = `damage_type != TE`).
    - **Phases, homing & damage**: Phase 0 Charge (`5` ticks, `NewAcBehavior +0x098`/`+0x648` via `0x828af660`), Phase 1 Homing Dash (`0x828af000` / `0x828aeb68` / `0x828ad328` / `0x828ad510`, `blade_max_speed = glide_max * 1.1`, `blade_accel = glide_acc * 0.1`, `blade_rise_max = glide_max * 0.3`, instant-swing cut-in at `12 m` `+0x2f0`), Phase 2 Swing (`12` ticks `+0x5ac`, active blade capsule sweep, FFX `4104` trail & `acweaponsoundparam` cues) & Recovery (`30` ticks `+0x5ae`), playing laser blade rows `100–113` or physical/pile rows `293–306` and camera action 44 (`ブレード`). TE laser blades deal fixed power with `en_drain`; KE physical blades & CE heat piles scale by `param/damage.bin` fields 16–20 (`0x828a43e8`: `ref_speed = 130 km/h`, `ref_weight = 3500`, `max_weight = 1.5`, floor `0.6×`).
  - Verified visually on `m4000` (`private/shots/kick.png`, `kick_hit.png`: design 5001 Boost Charge Kick destroying `e1020_0002` at 156.3 km/h for 18446 KE dmg / 6135 recoil; `pile_swing.png`, `pile_hit.png`: design 6006 Heat Pile homing into and destroying `e1020_0002` / `e1020_0001` for 45594 / 28747 CE dmg) and with unit tests across `damage`, `control`, and `weapons`.

- **AC TEST Enemy AI & Player Damage / AP** (`acvd-formats::acv_parts`, `acvd-game::ai`, `damage`, `weapons`, `targets`, `hud`, `sound`, `main`, `sheets/tuning_fields.csv`):
  - **Enemy weapon catalogue (`param/enemy/parts/acvenemyparts.bin`, `0x824a1498` → `0x824a1118` → `0x824a0430`)**: `0x824a1498` loads both `acvparts.bin` (`0x824a14bc`, `this+0x2f8`) and `acvenemyparts.bin` (`0x824a14e4`, `this+0x4dc`) through the identical category-table loader `0x824a0430` (all 24 category sizes extracted into `acvd_formats::acv_parts::record_size`; category 10 = `0x1a0` = 416 bytes with identical weapon sub-record offsets). AC TEST enemies resolve `EnemyCommonSt` → `EnemyWeaponSt.weapon_id` → `acvenemyparts.bin` category 10 → `PARAM_ENEMY_BULLET_ENEMYBULLET{RIGID,ENERGY,MISSILE}_BIN` + `PARAM_ENEMYWEAPONSFXPARAM_BIN` + `PARAM_ENEMYWEAPONSOUNDPARAM_BIN` (`acv2_se_enemy.fev` added to `sound::Cues`): `e1020` tank `8120` (1500 rigid, 800 KE, 800 recoil, 100 m/s), `e2030` CHheli `8231` (3630 homing missile, 200 CE), `e0010` ATK-MT `8000` (2500 energy, 500 TE, 160 m/s), `e0110` SNP-MT `8030`/`8031` (sniper 3000 KE / 1200 recoil @ 900 m/s + sub gatling 70 KE @ 350 m/s), `e0210` DEF-MT `8060` (gatling 400 KE @ 350 m/s). Test ACs (`a0000`–`a0003`, designs `5011`–`5014`) use their real `AcCtrlParam::calculate` and `weapons::Armament::from_design`.
  - **AC TEST AI behaviors (`acvd-game::ai`)**: mirrors the decompiled 360 Lua scripts (`private/lua360/script/enemy/e{1020,2020,0010,0110,0210}_actest.lua`, `script/arena/actest_{acai,def,snp,erik}.lua`) with `EnemyBasicSt` turn speeds, `EnemyPerformanceSt.no_controll_dist`, `HeriBehaviorParamSt`/`MechBehaviorParamSt` speeds, sight/hearing cones, last-hit alert (`TargetHealth::record_hit_from`), tank patrol/firing, heli 7 s altitude oscillation & missile launches, ATK-MT pass-attack sweeps, SNP-MT snipe vs escape state switch, DEF-MT shield-break dash at `< 50%` AP, and test AC circling/strafing + arm/shoulder fire.
  - **Player AC damage, stagger & clean respawn (`damage::PlayerHealth`, `weapons::Owner`, `weapons::tick_player_health`)**: `Projectile` and `Missile` carry `Owner::{Player, Enemy}`; `Owner::Enemy` sweeps against the player AC's `ac_hit_spheres()` (`0x82863b60`), applies `damage::calculate_damage` (`0x827d0b50`) against `DefenseProfile::from_ac(&design)`, updates `Status::ap`, triggers stagger (`2.0` s, 0.8× KE/CE/TE defense thresholds) when recoil `>= stagger_penalty_threshold`, shows `ACV_FE_Normal::Stagger` (`ErrorLog_Stagger`, dialog `208`, `0x825aefe0`), and handles AP = 0 cleanly with explosion FFX `4041` + `c00000089` and automatic 3.0 s AC TEST respawn at `Start`.
  - **`Gauge_APEffect` lag bar & damage blink (`0x825b3678` → `0x8258c808` / `0x8258c868` / `0x8258c7d8`, `0x829d0290`, `0x829d0b30`)**: traced all 8 fields into `sheets/tuning_fields.csv` (`NewAcBehavior` 379 `ap_lag_decay_frames` = 120 ticks / 2.0 s, 380 `ap_alert_drop_rate` = 0.03; `StaticDrawParam` 300 `+0x430` `ap_damage_blink_hz` = 5.0 Hz, 301 `+0x434` `ap_double_gauge_threshold` = 0.02, 325–328 `+0x484..+0x487` `ap_damage_r..a` = `(255, 80, 0, 255)`). `hud::ApEffectState` decays `lag_ap` at `(peak_ap - cur_ap) / 2.0` s, counts down `AP*` digits in orange, and blinks `Gauge_APEffect` at `((sin(t * 5 * pi) + 1) / 2)` whenever `(lag_ap - cur_ap) / ap_max >= 0.02`. Verified by unit tests (`acv_parts::tests::reads_enemy_parts_catalogue_from_disc`, `ai::tests::*`, `damage::tests::player_health_takes_damage_staggers_and_respawns`, `hud::tests::ap_effect_lag_bar_blinks_and_decays_over_120_ticks`, 79/79 `acvd-game` tests, 143 workspace tests), `acvd-sheets all`, `acvd-index audit`, and `--shot` (`private/tmp/enemy_ai_all_*.png`, `private/tmp/enemy_ai_stagger_*.png`).

- **Scan Mode & Recon Units** (`acvd-game::scan`, `control`, `fcs`, `weapons`, `hud`, `main`, `sheets/design_parts.csv`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`):
  - **Scan Mode toggle, EN recharge & fire lockout (`AC + 0xdb0`, `0x828bbb00`, `0x828b41a0`, `0x828b51e0`, `0x828bef18`)**: `0x828bbb00` (`0x828bbb78..0x828bbbc0`) toggles `AC + 0xdb0` (`0` Combat / `1` Scan, sending events `0x74`/`0x73` via `0x828b5cf0`). `0x828b51e0` passes `0` to `0x82879d58` when `0x828b41a0 != 0`, disabling all weapon firing while in Scan Mode (`manual.fmg` 20407). `0x8286a558` sums `part.en_consumption * (1 - 0x82869370(slot))` using `NewAcBehavior` fields 319–331 (`+0x4c6..+0x4de × 0.01`: 0% cut on head/legs/booster/recon, 50% on core, 100% on arms/FCS/weapons/shoulder/OW/hangers), scaled by `Assemble.bin +0x14` (`0x8286bd68` → `+0x414` / `0x82868268`); `0x828bef18` (`0x828bf6d4..0x828bf6f4`) selects `0x82868268` in Scan Mode for markedly faster EN recharge.
  - **Target Scan (`0x828bbb00`, `0x825a7c10`, `AcCtrlParamCalc.lua` lines 315–317)**: scans living targets inside a `NewAcBehavior +0x454` (`13.0°`, field 287) half-angle cone out to `head.camera_ability` (`+0x140`, `0x824791d8`, 508–812 m). Scan completion takes `scan_complete_frames = 120 * (1 + (100 - head.scan_performance) / 100)` ticks at 60 Hz (`head +0xf4` `i16` `scan_performance`, `NewAcBehavior` fields 557–559, Lua param 148; `0x825a7c10` at `0x825a816c..0x825a81c8`). Holding `F` / LMB / `R2` in Scan Mode populates `ACV_Scan_Cur_EnemyParamInfo` (`0x825a5978` / `0x825a4460`: `EnemyName`, `EnemyAP`, `EnemyKE`, `EnemyCE`, `EnemyTE`) for `NewAcBehavior` field 279 (`scan_display_secs` = `10.0` s).
  - **Category-13 (`recon`) & `BULLET_BULLETRECON_BIN` (`0x824554a8`, `0x828bca28`, `0x82a0d698`)**: `AcAssemblyDesignSt::ob` (slot 9 in `sheets/design_parts.csv`) holds the category-13 recon part ID (`111..420`, 9 records of 308 bytes): `+0x0a` `en_consumption` (`0x82475788`), `+0xd4` `bullet_id` (`0x82477fc8` → `0x82475900(recon, 0)` → `BULLET_BULLETRECON_BIN` rows `9111..9420`), `+0xd8` `hit_id`, `+0xf4` `init_speed` (`i16`), `+0x114` `max_simultaneous` (`0x8247b340`, 1–8), and `+0x116` `reload_ticks` (`0x824780a8`, 20/40 ticks). Carried recon capacity comes from **Core** `+0x114` (`0x824760a8` → `0x828bca28 +0xe6`, 20–90). `BULLET_BULLETRECON_BIN` provides `recon_type` (`0x824781a8`: 1 Sticky, 2 Follow, 3 UAV, 4 Hover), `life_time_frame` (`0x82478148`: 210–9000 ticks), and `effective_range` (`0x824780e8`: 220–500 m).
  - **Runtime & `sortie.drb` HUD**: `Z` / `Select` / `R3` toggles Scan Mode (swapping `ACV_LockSight_base` for `ACV_ScanModeLockSight` while keeping live AP/EN visible, blocking weapon fire, and speeding EN recharge); `F` / LMB / `R2` in Scan Mode drives `ACV_Scan_Cur_Center`, `ACV_Scan_Cur_Scaning`, and `ACV_Scan_Cur_EnemyParamInfo`; `B` / `GamepadButton::East` (○) launches the recon part's FLVER (`model/ac/parts/recon/rc####/...`), enforces `max_simultaneous` / `reload_ticks` / `life_time_frame`, detects targets within `effective_range`, and drives `FE_EspialMachineInfo`, `FE_ReconRaderMode`, `ACV_MapScan_EspialMachine*`, `ACV_MapScan_ReconInfo*`, and `ACV_MapScan_SubTargetS`. Verified by unit tests (`scan::tests::*`, `control::tests::en_high_boost_drains_and_blocks_when_low`, 74/74 `acvd-game` tests), `acvd-sheets all`, `acvd-index audit`, and `--shot` on `m4000` (`private/shots/scan_mode_test.png`, `private/shots/scan_mode_light.png`).

- **Missiles & Shoulder Weapons** (`acvd-game::fcs`, `weapons`, `hud`, `main`, `sheets/ac_part_fields.csv`, `sheets/ac_ctrl_calc.csv`):
  - **Category 12 (shoulder unit) `acvparts.bin` layout (`0x82470680`, cat-12 vtable `0x82023258` slot `+0x08`)**: checks byte `+0x135` (`has_weapon_spec != 0`, 1 on all 85 shoulder records) and wraps `record + 0x15c` as the **exact same weapon sub-record struct** as category 10's `+0x134` (`0x82475aa8`), sharing all getters: `is_missile` (`sub +0x00 == 1`, `0x82478850`, 40 of 85 shoulder units), `damp_range` (`+0x04`, `0x82478910`), `missile_lock_time` (`+0x06`, `0x8247cf48`), `bullet_id` (`+0x08` = `+356`, `0x82475bd8`), `hit_id` (`+0x0c` = `+360`, `0x82475c18`), `init_speed` (`+0x14`, `0x8247cbf8`), `en_drain` (`+0x18`, `0x8247cec0`), `lock_use_burst` (`+0x1c`, `0x82478cd0`), `burst_count` (`+0x1d`, `0x82478980`), `lock_once` (`+0x1e`, `0x824789d0`), `burst_interval` (`+0x22`, `0x82478a48`), `reload_time` (`+0x24`, `0x8247cb80`), `magazine` (`+0x42`, `0x82478af0`), `no_decay_range` (`+0x48`, `0x8247d350`), `guarantee_rate` (`+0x4a`, `0x82478ca8`), `min_power_rate` (`+0x4b`, `0x82478cf8`), `damage_type` (`+0x54`, `0x8247d4d8`), `damage_power` (`+0x58`), and `damage_recoil` (`+0x5c`). Arms `float 4` (`+0x24` `shoulder_mount_count`, `0x824795c8`) multiplies shoulder `burst_count` in `0x82858638` (`0x82858824..0x82858838`, 2× on twin-shoulder arms like `am0120`).
  - **Multi-target missile lock-on (`0x82858638`, `0x82857688`, `0x8247a1a0`, `0x82478cd0`)**: `0x82858638` clamps weapon simultaneous locks (`burst_count` if `lock_use_burst != 0` else `lock_once.max(1)`) against FCS `missile_lock_max` (`+0x135`, `0x8247a1a0`, 1–6). `fcs::step_missile_locks` accumulates locks at `missile_lock_time * missile_lock_effective_rate / 60.0` (`AcCtrlParam` param 81), distributing locks evenly across living targets inside the elliptical lock-sight cone and pruning targets that exit the `+2°` outer hysteresis cone.
  - **`BulletMissile` flight & homing (`0x828ec898` → `0x828e98c0` / `0x828e9b50` / `0x828ea520` / `0x828eb9c8` → `0x828eaf88`)**: `weapons::Missile` reads `BULLET_BULLETMISSILE_BIN` (`accel`, `air_cp`, `before_accel_gravity`, `after_accel_gravity`, `brake`, `accel_start_frame`, `accel_end_frame`, `act_start_frame`, `trace_trun_angle`, `min/max_speed_km_h / 3.6`, `hit_radius`, `proximity_fuse_range`), launches from `ShoulderHardpoint` (`core` dummy 50) on `G`/`X`/`Tab`/`R1` (`manual.fmg`), fans out locked salvos at `burst_interval`, ignites `after_accel_follow_sfx_id` (`4801`) at `accel_start_frame`, homes onto assigned targets at `trace_trun_angle`, and resolves hits against `TargetHealth` spheres + ground with `damage::calculate_damage`, `bullethitsfxparam`, and `bullethitseparam`.
  - **HUD (`hud.rs`, `0x829d0290`, `0x829d1910` / `0x829d1a10`)**: `Readout::Shoulder` drives `SWep*` digits, `Gauge_SWeapon`, `Back_SWeapon`, and `SHWeaponBar_base` (with `ammo_red_zone` fade); `hud::lock_cursor` renders the actual `sortie.drb` lock-on wings (`ACV_OutLine_LockCursor_E`, `_R`, `_L`, and up to 6× `_T` with `CursorLine`, `LockOnText`, `ReloadBar`, and `MissileLock1..6` pips). Removed the bogus `shoulder` → `core` dummy `50` mount from `sheets/assembly_slots.csv` (`core` dummy `50` is the Overed Weapon `ow` socket, `acattachinfo` rec 25; shoulder pods are part of the arm meshes, and `ShoulderHardpoint` attaches to `r_kata`/`l_kata`). Verified by 69/69 `acvd-game` tests, `acvd-index audit`, and `--shot` (`private/shots/msl_drb_lock.png`, `private/shots/msl_hit2_0.png`..`_3.png`).

- **Lock-on / FCS** (`acvd-game::fcs`, `weapons`, `hud`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`):
  - **Head & FCS `acvparts.bin` fields**: traced `Scr_GetPartsParam` per-category getter tables (`0x820236ac` head, `0x820237a0` FCS): head `camera_ability` (`+0x140` `u16`, id 1010, `0x82479188`), FCS `missile_lock_max` (`+0x135` `u8`, id 1500, `0x8247a1a0`), `lock_range` (`+0x13c` `u16`, id 1503, `0x8247a078`), `lock_sight_x` (`+0x13e` `i16`, id 1509, `0x8247a0a0`), `lock_sight_y` (`+0x140` `i16`, `0x8247a0d0`), `lock_calc_speed` (`+0x144` `i16`, id 1501, `0x8247a140`), and `missile_lock_calc_speed` (`+0x146` `i16`, id 1502, `0x8247a170`). `AcCtrlParamCalc.lua` (`NewAcBehavior` fields 532–535, 564–566) computes `first_lock_range_rate`, `first_lock_range` (param 74, `AcCtrlParam +0x12c`), `lock_effective_rate` (param 80, `+0x148`), and `missile_lock_effective_rate` (param 81, `+0x144`); `Assemble.bin` fields 200–202 (`+0x320..+0x328`) provide HUD sight base angles (`20° × 20°`, `0x829d4e40`) and the backward datum offset (`20.0 m`, `0x828577b0`).
  - **Elliptical lock-sight cone, secondary lock timer & lead prediction**: `0x82858178` (vtable `0x820541f0` `+0x54`) → `0x828577b0` packs `(sight_x_rad, sight_y_rad, first_lock_range, datum_back)`. `0x829d8d58` → `0x829d8850` → `0x829d8340` / `0x829dd5b8` tests `dist <= first_lock_range` and the elliptical cone `(x / (z * tan(sight_x)))^2 + (y / (z * tan(sight_y)))^2 <= 1.0` from `datum = ac_pos - aim * 20 m`, with a `+2.0°` outer keep-lock hysteresis margin (`0x829d8758`, `DAT_820b005c`). Per-hand secondary lock time is `weapon.missile_lock_time * lock_effective_rate / 60.0` (`0x82857688`), ticked by `0x829debe8` / `0x829de560`. When secondary-locked, `0x8287b070` → `0x829c2ee8` → `0x829c2808` aims at the linear lead point `target_pos + target_vel * t_flight`; primary-locked aims at current `target_pos` (`0x8287b158`).
  - **Runtime & HUD**: `fcs::update` acquires living targets, tracks velocity, advances per-hand secondary lock, and feeds `Fcs::aim_for_hand` into `weapons::fire` (projectiles + `ArmAim`). `hud::lock_cursor` projects the locked target onto the viewport, draws a hollow lock-on bracket tinted with `StaticDrawParam` 1st/2nd lock overlay colors (`indices 9..12 / 17..20`) that shrinks as secondary lock completes, and populates `sortie.drb` `ACV_OutLine_LockCursor_E` (`EnemyName`, `EnemyAP`, `EnemyInfo`, `0x829d1b40`). Verified by unit tests (`fcs::tests::*`, 66/66 `acvd-game` tests) and `--shot` (`private/shots/fcs_lock_v2_0.png`..`_1.png`: auto-locks and destroys `e1020_0002`, then secondary-locks off-center `e1020_0001`/`e1020_0000` with red reticle + `e1020` / `65` AP readout).

- **Damage model & target hits** (`acvd-game::damage`, `targets`, `weapons`, `sound`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`, `sheets/sound_cues.csv`):
  - **Weapon attribute & AC frame defense stats**: category 10 byte `+0x188` (`+392`, copied at `+0x54` by `0x8247d4d8` ahead of `damage_power` `+0x18c` and `damage_recoil` `+0x190`) is `damage_type`, switched in `0x82874190` (`0` = KE on 104 weapons, `1` = TE on 79, `6` = CE on 39 across all 222 records; `4`/`7` = unmitigated via `0x827d0928`). AC frame parts (categories 0–3) sum `ke_def` (`+0x134` `u16`, `0x8286a670`, Lua `Scr_GetPartsParam(cat, 201)`), `te_def` (`+0x136` `u16`, `0x8286a6d8`, Lua `202`), and `ce_def` (`+0x20` `f32`, `0x82475890`, Lua `float 3`), converted to `(low_def, hi_def, threshold)` via `AcCtrlParamCalc.lua` lines 199–232 / 410–416 (`NewAcBehavior` fields 509–517) and packed by `0x82873e70` (`0x82868ae8`/`0x82868b50`/`0x82868bb8` + `0x82874548..0x82874818`). Stagger threshold (`clashThreshold * 0.8` = `stagger_penalty_threshold`, Lua lines 326–339, `NewAcBehavior` fields 536–540) lowers defense thresholds by `0.8×` for 2 s.
  - **360 three-zone damage formula (`0x82874190` → `0x827d0d08` → `0x827d0cc8` → `0x827d0b50`)**: reads `NewAcBehavior` `+0x605` (`mid_damage_width_pct` = `30%`) and `+0x606` (`mid_damage_min_rate_pct` = `50%`) via `0x827d08b8`. `low_dmg = power * low_def * 0.01` (floored to `0.0` if `< 1.0`, `0x827d0978`), `hi_dmg = power * hi_def * 0.01` (`0x827d09c0`), `upper = thr * 1.30`. `power <= thr`: ricochet (`low_dmg`, `effective = false`); `power >= upper`: full penetration (`hi_dmg`, `effective = true`); `thr < power < upper`: linear ramp from `lerp(low_dmg, hi_dmg, 0.50)` to `hi_dmg` (`effective = true`). Recoil scales by `param/damage.bin` `+0x00` (`recoil_scale` = `1.0`, `0x82984438` at `0x82984518`).
  - **Target hit shapes, VFX/SFX & destruction**: enemies resolve `e####` → `EnemyCommonSt` → `EnemyBasicSt` (AP, KE/CE/TE `low_defense`/`hi_defense`/`defense_threshold`/`stagger_penalty_def_thr`, `small_stagger_threshold`/`stagger_penalty_time`) and `EnemyGraphicsSt` hit spheres (`collision_radius`, `hit_upper_hit_sphere_radius`/`height_offset`, `one_col_sphere_offset`); test ACs use frame AP sum (`0x8286a600`), `DefenseProfile::from_ac`, and body hit spheres (`r = 3.0 @ y = 3.0`, `r = 5.5 @ y = 6.0`, `0x82863b60`). `weapons::step_projectiles` sweeps each bullet segment against living target spheres, applies range decay + `calculate_damage`, spawns `bullethitsfxparam` `dry_ac_low_damage`/`ac_hi_damage` FFX, and plays `bullethitseparam` hit sounds (`acv2_se_bombs.fev`, e.g. `w00009027` ricochet / `w00009224` effective). On `AP <= 0`, `targets::tick` spawns `sfxsesetparam` row 501/502 explosion FFX (`4019`/`4041`), plays `w00009600`/`w00009601` (enemies) or `c00000089` (`acsoundparam.se_destroy`, ACs), and swaps enemy meshes to `{model}_b.flv` (or despawns ACs). Verified by unit tests (`damage::tests::*`, `targets::tests::*`, `weapons::tests::*`, `sound::tests::gameplay_cues_decode_x360`) and `--shot` (`private/shots/damage_boom_0.png`..`_3.png`: `e1020_0002` hit twice by rifle 1720 for `-1135 AP` each, staggered, exploded with FFX 4019, and swapped to `e1020_b.flv`).

- **AC test targets** (`acvd-formats::msb`, `acvd-game::targets`, `pose::spawn_static`, `main`):
  - **MSB layer & AC model links**: `acvd-formats::msb` now decodes `LAYER_PARAM_ST` (section stored at MSB reader `+0x18` by `0x82603878`, indexed by `0x8280cee8` / `0x82b91610` / `0x82b916b8`), `PARTS_PARAM_ST` `+0x64` sub-record `+0` `i16` layer index (`0x82b912b0`, resolved to `Part::layer`), and `MODEL_PARAM_ST` (`kind == 4`) `+0x18` sub-record `+0` `u16` `AcAssemblyDesignSt` ID (`0x82b91348` → `0x82b91470` at `0x82b91488`; `a0000`–`a0003` = designs `5011`–`5014`).
  - **AC TEST mode selection**: `param/actestdata.bin` (`m720_NO TARGET`..`m725_NEXT AC`) is an unused AC4/FA leftover; ACVD AC TEST uses `mission/ch6000.xml` (`0x825dbb70` returns `6000`) and the garage AC TEST settings list (`0x82361b08` / `0x82361ce0`, `0x82a2a658` mapping `0..7` → `actest1..actest8`): `0`/`actest1` empty (360 menu default), `1`/`actest2` 7 `e1020` tanks + 5 `e2030` CHheli, `2`/`actest3` 2 `e0010` + 2 `e0110` + 2 `e0210` MTs, `3`/`actest4` `a0000` ATK-AC (5011, CBT), `4`/`actest5` `a0001` DEF-AC (5012), `5`/`actest6` `a0002` SNP-AC (5013), `6`/`actest7` vs-UNAC (0 MSB parts), `7`/`actest8` `a0003` Eirik AC (5014, retail).
  - **Runtime**: `acvd-game::targets` spawns the active layer's kind-2 parts from `{map}_{layout}.msb` via `--test <mode>` (default `actest2` / `light`; accepts `0`..`7`, `none`/`light`/`mt`/`atk`/`def`/`snp`/`unac`/`eirik`/`all`, or a literal layer name). Enemy targets load `model/ene/{m}/{m}_m.bnd.dcx|{m}.flv`; AC targets assemble, paint (`AcColor`), and hold their idle stance via `pose::spawn_static` independently of the player's `Motion`. Verified by `msb::tests::actest_layout_from_disc`, `targets::tests::*`, and `--shot` (`private/shots/actest_light.png`, `actest_mt.png`, `actest_eirik.png`).

- **Generator Energy (EN) drain & recharge** (`acvd-game::control`, `weapons`, `hud`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`; Xenia probes `private/xenia/en_drain{,2}.txt` -> `.jsonl`, recipe `tools/xenia/recipes/en_drain.txt`):
  - **EN Pool (`AC + 0x214`, `0x828308f0` init, `0x82830910` drain, `0x828307c8` recharge)** & **Generator (`AC + 0x248` `m_pGenerator`, `0x82855ad8` -> `0x8287edc8` ctor, `0x8287edd8`/`0x8287eec8`/`0x8287eef0`/`0x8287ef40`)**:
    - Capacity: generator `u32 +0x134` (`0x8247a390` -> `0x82858408` -> `0x828308f0(cap, 0.0, 1.0)`), `58630` on generator 111 (`en_drain.jsonl`). Critical threshold `+0x0c` is `0.0` (no redline lockout in ACVD; active drains with `is_passive == 0` simply return `0` without draining when `cur_en < amount`).
    - Output: normal mode uses generator `u32 +0x13c` (`0x8247a3d0` -> `0x8286e700` `+0x34c` -> `0x8287edc8` `+0x00`), `15640` EN/s on generator 111; OW mode (`+0x10 != 0`) uses `core.en_conductivity * 0.01 * gene_owen_out` (`0x82868798`, `21505`).
    - Passive drain: `0x8286a4c0` sums `u16 +0x0a` (`0x82475788`) over all equipped parts (minus generator `+0x0a` = `0` in `0x8286bd68`) × `Assemble.bin +0x14` (`1.0`, `0x82868f40`); `9121` EN/s on the AC TEST build. Each frame `0x828bef18` fills `m_pGenerator +0x08 = output * dt`, draws `passive_drain * dt` via `0x8287eef0` (overflowing to `0x82830910(passive=1)` if overloaded), and recharges `AC + 0x214` with the surplus via `0x8287ef40` -> `0x828307c8` (`+217.4` EN per 30 Hz frame = `6519` EN/s net, no recovery delay, identical on ground and in air; Scan Mode `AC + 0xdb0` uses `0x8286a558` with `NewAcBehavior` fields 319-331, not yet wired).
  - **Action & weapon drains**:
    - **High Boost** (`AcActCtrl_startHighBoost` `0x8283ed98` -> `0x828b2648` -> `0x82830910`): drains `qb_en_drain = BST__HI_EN * 0.7` (Lua param 57 `kickEnDrainAtOnce`, probed `9117.5 = 13025 * 0.7` on launch for both tap and charged High Boost) and blocks launch when `en < qb_en_drain`.
    - **Glide Boost** (`0x8281ea30` / `0x8281e9d4`): drains `hi_en * dt` (`BST__HI_EN`, Lua param 56, probed `434.2` per 30 Hz frame = `13025` EN/s); returns `0` and ends glide immediately when `en < amount` (`en_drain2.jsonl` `t=41552ms`: `en = 425.1 < 434.18` ends glide and immediately recharges next frame).
    - **Wall Kick** (`AcMove_wallJump` `0x82824560`): drains `trijump_en_drain * dt * scale` (`BST__HI_EN * 1.5`, Lua params 22/23 at movement `+0x14c`/`+0x150`) each Phase 1 Accel tick, skipping thrust when insufficient.
    - **Ground / Air Boost & Jump** (`0x828225e8` / `0x828234f0` / `0x828227f0`): drain `BOOST_EP * dt` (Lua params 47/50; `0` on all 360 boosters and tank legs, probed `0.00`).
    - **Energy weapons** (`0x828a1820`, `weapons.rs`): `Gun::en_drain` (`category 10 +0x14c`) blocks firing when `en < en_drain` and drains on each shot.
  - **HUD (`hud.rs`)**: `Status` tracks live `en` / `en_max` synced from `Pilot` (and `weapons::fire`), driving `Gauge_EN`, `EN1..EN5` digits, and the `en_red_zone` (`0.3`) fade to red (`private/shots/en_glide_mid.png` `056.12`, `private/shots/en_glide_red.png` `021.00` red).

- **Wall Kick / Wall Jump (三角跳び)** (`acvd-game::collision`, `control`, `sfx`, `sound`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`, `sheets/ac_states.csv`; Xenia probe `private/xenia/wall_kick.txt` -> `.jsonl`, recipe `tools/xenia/recipes/wall_kick.txt`):
  - **Surface & material filter (`0x8371d460`, `0x82b9dc38` -> `0x82c52498`, `0x828aa500`)**: the 27-entry HMD material table at `0x8371d460` stores per-material exclusion masks (`water`/`ashore` `0x02`, `camera` `0x08`, `enehit` `0x10`, `plyerhit` `0x20`, `ackickrhit` `0x60` = `0x20 | 0x40`; normal terrain `concrete`/`clay`/`iron`/etc. = `0x00`). `WallJumpHitChecker` (`0x828aa500`, built by `0x82858f10` from `NewAcBehavior +0x628/+0x62c` = radius `6.0 m` at height `3.0 m` for all leg types) sets filter `0x100042` (`0x82c43c20`), excluding `water` (`0x02`) and `ackickrhit` (`0x40`) as well as objects with `AcObjectParam.enable_wall_jump == 0`. `0x828ab428` rejects contacts with `|dot(normal, +Y)| > NewAcBehavior[+0x4f8] * deg2rad` (`45° * pi/180 = 0.7854`).
  - **State machine (`WallJumpState`, vtable `0x82092a20`, ctor `0x828410a0`, enter `0x828436e0`, update `0x82844648`)**: triggered by `0x82841520` -> `0x82841338` when Jump is pressed airborne outside Jump take-off (state 3) with `action_ctrl +0x114 < 1/60` (set to `NewAcBehavior +0x4ae` = `10` ticks on normal jump by `0x8283d4b8`, and to legs `float 27` `trijump_lockout_frames` `12..60` ticks on wall kick by `0x82841338`):
    - **Phase 0 (`0x828438c0`, Prep)**: lasts legs `int16 4` (Lua param 130 `trijump_prep_frames`: `8/12/15/17` ticks; probed `12` ticks = `207 ms` in `wall_kick.jsonl`), plays `wall_jump_prep` (acmotion rows 237/235/231/233 from body-local wall normal via `0x8284c588` -> `0x82884da8`), allows turning at `trijump_turn_rate` (`0x8281fcd0`, movement `+0x148`). At expiry, `0x828abac0` computes the launch direction (`(0, 1, 0)` for neutral stick or `dot(wish, normal) < -cos(45°)` from `NewAcBehavior +0x5e0`; `wish` for `dot > 0`; `wall_normal` rotated by `±trijump_glance_angle` `85°/65°` from legs `float 28` `0x8247b948` for oblique input), zeroes `vel.y` (`0x8281f100`), emits TAE `0x836`, and enters Phase 1.
    - **Phase 1 (`0x82841190`, Accel)**: lasts legs `float 31` (`0x82476418`, `trijump_accel_frames`: `11..22` ticks, `1` tank; probed `11.0` ticks = 5 full 30 Hz frames + `0.5` tail), plays `wall_jump` (acmotion rows 238/236/232/234 via `0x8284c5e8` -> `0x82884688`), sets movement `+0x1bc = 2` (`0x82822af0` case 2 = `trijump_h_speed_tick`), and calls `AcMove_wallJump` (`0x82824560`, movement `+0x44`): vertical (`0x8281f408`: `dir.y >= 1.0`) applies `v_acc = trijump_v_acc_tick + trijump_h_acc_tick` capped at `trijump_v_speed_tick`; directional applies `h_acc = trijump_h_acc_tick` capped at `trijump_h_speed_tick` (probed `0.6927` = `149.6 km/h`) and `v_acc = trijump_v_acc_tick * 0.5` (`0x8371398c`) capped at `trijump_v_speed_tick * 0.8` (`0x83713988`; probed `0.31035` = `0.38794 * 0.8`), lighting mode 4 booster SFX at `NewAcBehavior +0x4e0` (`0.7`).
    - **Phase 2 (`0x82843b88`, After)**: lasts legs `float 26` (`0x824764c0`, `trijump_after_frames`: `8..12` ticks), turns at `trijump_turn_rate`, exits on timer expiry or touchdown. Camera action `43` (`0x2b`, `三角とび`). Verified by unit tests (`collision::tests::wall_kick_contact_filters_material_and_angle`, `control::tests::wall_jump_*`) and `--shot private/shots/wall_kick_test.png`.

- **High Boost and Boost Charge** (`acvd-game::control`, `sheets/ac_part_fields.csv`, `sheets/tuning_fields.csv`, `sheets/ac_ctrl_calc.csv`, `sheets/ac_states.csv`; Xenia probes `private/xenia/quick_boost{,2}.txt` -> `.jsonl`, recipe `tools/xenia/recipes/quick_boost.txt`):
  - **Offset correction (`struct = movement + 4`)**: `0x82869e10` unpacks Lua params 131-142 and 147 (`AcCtrlParamCalc.lua` lines 288-314; `0x82455fe0` `Scr_GetACVParam` -> `0x8246edd0` -> `0x82475890` float `+0x14+4n`, `0x824758b8` i16 `+0xf4+2n`, `0x824758e0` u8 `+0x114+n`, from category 6 booster or category 3 tank legs), and `0x82821af0` writes into `movement + 4`: object `+0x114` = `quickBoostAccelVMS / 60` (param 132), `+0x118` = `quickBoostAccelHMS / 60` (param 131), `+0x11c` = `quickBoostMaxSpeedHKmH / 216` (param 133), and `+0x120` = `+0x11c * param 138` (`qb_min_speed_rate`, float 11 at `+0x40`). Movement slot `+0x68` (`0x82822980`) is **not** a velocity impulse: it stores `f1` at `+0x220`, adds `direction * (+0x118 * f2) * 60 * dt` to horizontal acceleration `+0x1d8..+0x1e0` (`0x82820f38` -> `0x82820020`), and when `r7 != 0` adds `+0x114 * f2` to vertical acceleration (`0x8281f4b0`) with rise cap `+0x21c` = `+0xe4` (`NewAcBehavior` field 344 `+0x510` = 15 km/h = 0.06944 m/tick).
  - **Player state machine (`HighBoostState`, vtable `0x8200d378`)**: launched by `0x82841d50` -> `0x8283ef90` -> `0x8283ed98` -> `0x82844ea8` -> ctor `0x8237bec8` with the 10-word param block built by `0x828590f8` (`+0x3c..+0x60`, copied to `action_ctrl + 0xbc..0xe4` by `0x82845190`), stepped by `0x8237c5a0`:
    - **Phase 0 (`0x8237c1b8`, Boost Charge wind-up)**: plays `acmotion` row 284 (`クイックブースト溜め（360）`, anim 185, booster clip 4; `0x8284c310` -> `0x82884cd8`) while held up to `qb_charge_max_frames` (param 147, i16 0: 18/15/10 frames; auto-fires at `0x8200d3f0` = 0.9999). On release, evaluates piecewise-linear curve `0x8237bbf8` (`x = hold / max_frames`, ramp start param 137 i16 1, sweet-spot centre param 135 i16 3, width param 139 i16 4 halved by `0x8202081c` = 0.5, overcharge end param 140 u8 2) twice: `w1` from base param 134 (u8 0: 5-9%, tank 80%) sets `+0x4c` (`f1`) and Phase 1 duration `+0x10 = param 141 (i16 5) * w1`; `w2` from base param 136 (u8 1: 45-93%, tank 70%) sets Phase 2 duration `+0x14 = param 142 (i16 6) * w2` (spec-sheet helper `0x825ef928` sums params 141 + 142). Emits TAE event `0x8283c8d0` `0xC1F`/`0xC20` (normal ground/air) or `0xC21`/`0xC22` (`w2 >= 1.0`, Second-Stage sweet spot).
    - **Phase 1 (`0x8237ba68`) & Phase 2 (`0x8237bf28`)**: every frame calls `slot +0x68` (`f1 = w1`, `f2 = 1.0` or fractional tail, `r7 = is_grounded` in Phase 1) and `slot +0xd4(4)` (`0x834b7ed8`, setting `+0x1bc = 4` so `0x82822af0` clamps `0x82822ea0` to `lerp(+0x120, +0x11c, w1)`). Transition `1 -> 2` plays directional rows 285-292 (`0x8284c3a0` -> `0x82884d28`). Phase 2 exits via `0x8283fc10` on timer expiry, ground touchdown (`0x8318fa10`), or Glide Boost cancel (`0x8237bcb0` -> `0x8283fdb0`). High Boost does **not** force boost mode on (`quick_boost.jsonl` Hit 1 settles back to walk speed 26.0 km/h; `quick_boost2.jsonl` Hit 1 with boost on decays by `over_max_decel = 0.01`/tick to 57.9 km/h, tap peak `0.4711` = 101.8 km/h, sweet-spot peak `0.5787` = 125.0 km/h).
    - **Re-fire interval**: `0x8283ed98` writes `NewAcBehavior` field 340 (`QBリロードフレーム` `+0x500` = `0.0`) x 1/60 to `action_ctrl +0x110` (`0x82844050` ticks it down), and `HighBoostState` slot `+0x50` = `0x8241a6b8` returns 0 while active, so a new High Boost is immediately available once Phase 2 ends (~200 ms chain spacing in `quick_boost2.jsonl` Hits 2-5) without waiting for the animation clip.

- **AC paint** (`acvd-formats::accolor`, `acvd-game::paint`, `ac_paint.wgsl`, `sheets/tuning_fields.csv` fields 147–148):
  - **`param/accolor/color%04d.bin` (`0x358` = 856 bytes)**: loaded by `0x82331478` / `0x82331680` and copied into `assembly_data + 0xa0` by `0x82331b18` for `AcAssemblyDesignSt.coloring_id` (`+0x4e`). Initialized by `0x82332420`, compared by `0x8235ddc8`, unpacked by `0x82463630`:
    - `+0x000..0x330` (`816` bytes = `34 × 24`): 34 part-colour groups, each a 24-byte `ColoringSetSt` (`base`, `aid`, `aid2`, `optional`, `joint`, `kojima` as `[u8; 4]` RGBA, indexed at `base + group * 24` by `0x82463700`; channel names at `0x820223ac..0x820223d4`).
    - `+0x330..0x352` (`34` bytes): 34 `u8` camouflage pattern IDs (`camo_id - 1` passed to the composer; `0` = none).
    - `+0x352..0x354` (`2` bytes): zero padding.
    - `+0x354..0x358` (`4` bytes): `[u8; 4]` RGBA `eye` colour (channel `6` in `0x82463698` and `0x8257ff28`).
  - **Part slot → `AcColor` group (`0x83694720` → `0x838a2b58`, `0x82463750`, `0x82463798`, `0x82b4d618`)**:
    `0` head, `1` core + booster (`part_slot == 6` returns group `1`), `2` right arm + `rack_r` (`part_slot == 17`), `3` left arm + `rack_l` (`part_slot == 18`), `4` legs, `5` `armwep_r`, `6` `armwep_l`, `7` `back_r`, `8` `back_l`, `9` `shoulder`, `10` `hanger_r`, `11` `hanger_l`. Because `am*.flv` holds both arms in one FLVER, `0x82b4d618` splits arm meshes by root bone (`r_kata` → group 2, `l_kata` → group 3; `LoadedMesh::root` tracks this in `acvd-render`).
  - **Byte-to-float remapping (`0x82460ef8`) and reversed `FC_AC_OverlayCol` upload (`0x82ca9c70` / `0x82bd6588`, `0x82461ce0`)**:
    `0x82460ef8` reads `param/staticdrawparam.bin` (table `0x37`) field 147 `color_min` (`+0x228` = `0.2`) and field 148 `color_max` (`+0x22c` = `0.8`) and maps each RGB byte linearly into `[color_min, color_max] = 0.2 + 0.6 * (b / 255.0)`. Both `0x82ca9c70` (dynamic AC shader setter calling `0x82bd6588`) and `0x82461ce0` (baked `_c` composer, `0x824620f0..0x82462168`) upload pixel-shader constants `c180..c186` (`0xb4..0xba`, device offsets `0x22c0..0x232c`) in **reverse** order: `c180 = eye`, `c181 = kojima`, `c182 = joint`, `c183 = optional`, `c184 = aid2`, `c185 = aid`, `c186 = base`.
  - **Camouflage patterns (`0x824611b8`, `0x82460e58`, `0x82461a98`, `0x82ca9c70`, `0x82461ce0`)**:
    `0x824611b8` (called from `0x82669608`) loads `model/image/camouflage/camouflage_t.bnd|camouflage.tpf` (`model_image:\camouflage\camouflage` at `0x8203c948`) into `0x838a2a8c` and stores the 11 `256x256` BC1 textures `ca0001`..`ca0011` at `0x838a2a90`. `0x82460e58` indexes `0x838a2a90` by `camo_idx = AcColor.camo[group] - 1` (`-1` when `camo == 0`, set by `0x8257ff28` / `0x8257fa88` / `0x82b4ded8`). Both `0x82ca9c70` (`0x82ca9e34..0x82ca9f14`) and `0x82461ce0` (`0x82461f48..0x82461ffc`) bind `ca{camo_id:04}` to `g_AC_CamouflageMap` (`s15`, or `mask_tex` when `camo == 0`) and upload pixel constant `c190` (`0x2360`) = `vec4(mask_width / camo_width, mask_height / camo_height, 1.0, 1.0)` computed by `0x82461a98`. `Flver_ACColorColDifEmi.fpo` and `Coloring_Camouflage.fpo` sample `g_AC_CamouflageMap` (loaded as linear `Bc1RgbaUnorm` so its 4 quantized `R` levels `0.387, 0.516, 0.629, 0.758` map to `idx = 3..6` = `optional, aid2, aid, base`) at `uv * c190.xy` and mix `mask.r` with `camo.r` by `mask.b` when `mask.r >= 0.4375` (`c255.w`, keeping `idx <= 3` inner mechanical frame `kojima`/`optional` uncamouflaged).
  - **Glow / emissive channel (`0x82bd9210`, `0x82bd8198`, `0x82bd8958`, `0x82bd8ad8`, `0x82bd7bd8`, `Flver_ACColorColDifEmi.fpo`)**:
    `0x82bd9210` (`0x82bd950c`) calls `0x82bd8198`, which matches `"_glow"` (`0x820f2334`), `"_rim"` (`0x820f233c`), or `"_phong"` (`0x820f2344`) in the lowercased MTD stem (`ACParts_Glow`, `ACParts_Wep_Glow`, `ACParts_phong.mtd`) and constructs `0x82bd8958` (vtable `0x820f23f0`, or arm subclass `0x82bd8ad8` / vtable `0x820f2490` with `r_kata`/`l_kata` split via `0x82ca48e0`) with default intensity `20.0` (`0x8200b3c8` at `+0x4c`). When `AcColor` is applied (`0x82bd7ea8` / `0x82bd7d30` -> `0x82bd7cc8` -> `0x82bd7ae8`) or drawn (`0x82bd7bd8`), it converts `AcColor.groups[group].kojima` RGB by `* (1.0 / 255.0)` (`0x8201a21c`) × `20.0` and uploads `c138` (`0x2020`) via `0x82c89c78`. Note: on `_2` / `_03` glow submeshes, `<diffuse>_c` is uniform `R = 6/31 = 0.194 -> idx = 1 = kojima` (`c181`), so `kojima` both tints the lens diffuse via `[color_min, color_max]` and drives the additive emission `(emi.rgb * emi.a)^2 * c138.rgb` from `g_EmissionTexture` (`Flver_ACColorColDifEmi.fpo` instructions 23, 31–37, 54–58, 68).
  - **Runtime (`acvd-game::paint` + `ac_paint.wgsl`)**: `ExtendedMaterial<StandardMaterial, AcPaint>` binds `<diffuse>_c`, `ca0001..ca0011` (`camo_map`), and `g_EmissionTexture` (`emission_map`), computes `idx = clamp(i32(floor(r * 8.0 + 0.3)), 0, 6)` after the `r >= 0.4375` camouflage mix, applies the 360 overlay blend (`2*d*c` below `0.5`, `1 - 2*(1-d)*(1-c)` above, weighted by `mask.g`) in raw sRGB texel space before PBR lighting, and adds `(emi.rgb * emi.a)^2 * emission.rgb` on `_Glow` / `_phong` submeshes (`private/shots/paint_camo_5011_front.png`, `private/shots/paint_camo_5011.png`, `private/shots/paint_camo_6002.png`, `private/shots/paint_camo_6019.png`).

- **AC wall collision** (`collision.rs` `push_out`, `control.rs` step; leftovers under Open).
  Found with four unattended Xenia runs (AC TEST, walk forward into the start parapet; probe
  lists `private/xenia/wall_walk{,2,3,4}.txt`, recipe `private/xenia/wall_walk_recipe.txt`):
  - `0x829e7b68` / `0x829e7be0` (and wrappers `0x829e7d58` / `0x829e7d68`) never run in AC
    TEST. The AC's down rays come from the WorldRayCastManager (vtable `0x820c5538`, source
    `World\WorldRayCastManager.cpp`) batch `0x829e84f8` -> `0x829ea0d8`, five rays through the
    ray builder `0x82b9df28`: `…a194` 23 m up (filter 0x80100006), `…a27c` 1000 m down from
    +6.7 m (0x100002), `…a30c` 100 m down from +54.6 m (0x200002), `…a378` short down rays
    2.2-2.5 m out from the centre (0x100002), `…a3e8` long down (filter 0).
  - Body: FromSoftware's own hit library (`Source\Hit\`: `HitCheckerSphereWorld` `0x82ce6a48`,
    `HitCheckerSphereGeometry` `0x82cf4428`, `HitShape_Sphere` `0x82ceec98`), no Havok. The
    AC hit object (vtable `0x82095f54`: dtor `0x82866b20`, update `0x828651d0`, shape test
    `0x82866048`) is created by `0x82863ad0` from `0x82858460`, which reads tuning record 0
    (`0x82330c38(0x82332018(), 0)`) `+0x22c` / `+0x230` / `+0x234` (+ `+0x380`) and builds three
    spheres in `0x82863b60` via `0x82862df0` -> `0x82c44028`. Each frame `0x828651d0` sweeps
    the lower (`+0x8`, r 3.0, 3.0 m up) and upper (`+0xc`, r 5.5, 6.0 m up) spheres from the
    old to the new position through `0x82c441f8` (sphere +0x8 radius, +0x10 offset; r4 +0x0
    new, +0x30 old), up to three sweeps a frame.
  - Probe result: at the parapet the velocity turns from (-0.120, 0) to (-0.116, +0.031) at
    the same speed and the AC slides along it to (-393.15, 22.81, 668.63). The runtime walks
    from the start to (393.72, 22.81, 668.92) (x mirrored) along the same parapet; without the
    push it walked off the platform.
- Booster VFX groups (`acvd-game::sfx::boosters`, probe `private/xenia/boost_vfx.txt`,
  recipe `tools/xenia/recipes/boost_vfx.txt`). The sfx controller (`vtable 0x82098d10`)
  builds the lists in `0x82893ec0` from `MAP_SFXPARAM_ST` copied to object `+0x740`, and
  `0x828932a0` plays them through `0x82892f08`. Ground boost is movement slot `+0x10`
  (`0x828225e8`, mode 0, f2 = 0); air boost is slot `+0x2c` (`0x828234f0`, mode 0, f2 = 1);
  glide is slots `+0x34` / `+0x38` (mode 1, both intensities = stick length clamped to global
  param 0 `+0x498`, full stick = 0.6); non-boost air is slot `+0x14` (mode 2); slot `+0x44`
  is mode 4 with both intensities = param 0 `+0x4e0` (0.7). Flags: 1 and 2 play main 300 on
  dummies 31-34 (2 is the rear stick sectors 6/7/8 from `0x82825918`), 0x40 plays foot 301
  on those same nozzles, 4/8 play back 302 on side dummies 11/12 and 13/14 at scale |stick x|
  past 0.1. Mode 1 also plays 303 on leg points 21-24 and effect 1379 (`li 0x563`) on dummy
  31; every other mode plays 302 on 21-24. Light 299 rides 41 with the main/foot lists and
  42/43 with the side lists. Quick boost (`0x82822980`) does not call the dispatcher. The
  live FX scale is uniform `(f, f, f, 1)` (`0x830e8ef8`). Bevy's `AlphaMode::Add` shader already
  does `rgb * alpha` and writes alpha 0, so a second premultiply squared the cyan nozzle
  (alpha about 0.8) and left alpha-1 orange at full strength. f0000301's 2020 copies are model
  61 (s1020 / s1011), not cluster 108. UVs already sit in one atlas cell, so the frame index
  slides them. Each copy keeps the spawner's world matrix from birth (`private/shots/boost_glide_axis.png`:
  45 copies, span 160.8 m along velocity, axis about (-0.8, 0.5, -0.3) against vel (1, 0, 0)).
  The orange streak on that frame is f1379 on dummy 31: s1011 scale z peaks at 5 (mesh about
  4 m, so roughly 20 m) and the colour key is (1, 0.47, 0). f301's own meshes stay small
  (s1020 about 0.8 m, persistent s1011 scale z 0.1–0.25, effect scale 0.6), so they do not
  form the 360's blue-white beams (`private/xenia/shots/boost_glide.png`). Model builder
  `0x8311a928` reads action 61 through slot 34 (slot 21 is the draw-mode switch; 2 is the
  plain scaled model at `0x82c711d8`); slot 44 (4 on the spawned flame, 6 on the core) is not
  read. `param/FilterParamList.xml` has no bloom entry. Normal boost is still the small cyan
  nozzle plus f0000300 ground dust (`private/xenia/shots/boost_fwd.png`); it was not re-shot
  after the trail change. The glide shot still does not match.

- Map lighting, fog, sky dome, tone map, bloom, `Map_Diffuse_Multi`, `Water.spx`, and per-draw
  light-set ids (`sheets/map_env.csv`, `acvd-formats::env` / `flver` / `msb`, `acvd-render`,
  `acvd-game::env` / `map`): env light sets / fog / scene record read and matched to the 360's
  probed shader uploads; map pieces lit with `Flver_ColDif` / `Flver_ColDifMul` (`UV0`/`UV1` +
  big-endian `D3DCOLOR` vertex alpha blend on `Map_Diffuse_Multi` mountains `m9000..m9017`) then
  `c31 = 0.5` and `m_ToneMapFilter` (m4000 Divide: `1.1 * x / (1 + x)`, `x = col * 0.5 / 0.3`);
  HDR additive `Bloom` wired from type-101 sub-block 2 (`+0x19..+0x3c`, `0x82c18580`); `Water.spx`
  (`m9100`/`m9101`, `Flver_Water.fpo`, `Water.mtd`) rendered with 3 scrolled `water_n` normal
  layers, Fresnel reflection, and `c11`/`c12` specular in `env_water.wgsl`; `o9705` sky dome
  behind everything, centred under the camera at the env sky scale when `MODEL_PARAM_ST +0x14 +1`
  bit 7 is set (`0x829911f0`); light-set id from that same sub-record `+0` (part `+0x4c +0`
  override). Open leftovers under Map look.
- Map piece LOD (`sheets/map_lod.csv`, `acvd-game::map`). m4000's piece binder has 57 full
  FLVERs and 39 `{model}_l1.flv` (54 of 72 kind-0 parts; no `_l2`; face flags 0). The model
  LOD metric is `max(distance - radius, 0.1) / (2 * radius)` (`0x82d13e70`, radius at model
  +0x5c, 2 at `0x820b005c`). `0x82d10640` (vtable `0x8210c7c8` +0x68) maps that to level 0
  below 0.1, 1 below 0.4, 2 below 2, 3 through 15 (floats `0x82013518`, `0x82052b18`,
  `0x820b005c`, `0x82039608`). Levels 1-3 share `_l1`. Above 15 the function returns 0
  before picking a level; using that as a world-distance cull hid 481 of 550 parts at the
  actest camera, so those parts stay up and use `_l1` too. At that camera the run is
  232 full and 318 `_l1` (`private/shots/m4000_lod.png`). The distance is the VMX length
  stored as distance±radius at the LOD object +4/+8; that block is not instruction-decoded.
  The model-pointer fetch `0x82d134b0` is `li r3, 0; blr`. Loader `0x82d0eee0`.

- Air glide clips (`sheets/ac_states.csv` `glide_air_in` / `glide_air`, `acvd-game` `glide_clip`).
  Off the ground a glide plays the directional 30-frame entry (acmotion 223-230, builder
  `0x82883c80`) and then the air-float wheel (row 222, anim 62, booster clip 3, builder
  `0x82883d30`). Landing returns to the ground wheel.

- Booster nozzles pivot with the move (`acvd-game::boost`, `sheets/booster_anim.csv`).
  The four roots stay on the core sockets (`sheets/assembly_slots.csv`, flag 3). The bank
  `model/ac/motion/boost/boost_a.bnd.dcx` is `model_ac:\motion\%s%s\%s` with `""` and `boost`
  (360 `0x8284e078`, call `0x8284ec18`). `BoosterAnimID` (acmotion byte +8, read at `0x82882d80`)
  selects `a00_<id>.ani`. Clips of 360 frames are direction wheels and use the body lean's eased
  heading (or the velocity heading, for high boost); the others hold one pose. `Booster_Frame`
  (hokan +0x10) is the blend, read as milliseconds. The mounted rotation stays, plus the
  parent-space change from the clip's rest pose. `boost_la` is the light set and is not played.

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
  socket (`l_boost` 8 and `r_boost` 9 on the back, `l2_boost` 25 and `r2_boost` 26 on the
  front). 31 cores have no 25/26 (cr0230, cr04xx-06xx, cr1010, cr2230, cr24x0, cr25x0,
  cr2620, cr3010, cr4010). There no record attaches, and `0x8288db60` (booster category 6
  only) calls model vtable `0x82044bec` slot 8 = `0x8258aed8` with `(bone, 0, children)`;
  `0x82c92688` clears the draw bit of each mesh on that bone, so the front pair is not
  drawn. `acvd-game` lists those roots in `Placement::hidden` and zeroes their joint scale
  (`boost::Folded`). Vtable from `private/xenia/boost_front.txt` (AC TEST recipe); the hide
  branch itself did not fire there (that AC's core has 25/26). Hanger racks are the fixed models `hgl0001` / `hgr0001` on arm
  dummies 80/81; the hanger weapon then sits on rack dummies 85/87. Flag 4
  (`0x8288a058` → `0x827ce358`) points the weapon's local -Z along the socket forward, with
  world +Y as the up hint (±90° about X when they are parallel, `0x82d4ee50`). Rack dummies
  85/87 point along +Y. The rack's own clip then pitches that bone (`sheets/hanger_anim.csv`,
  `acvd-game::hanger`). The stow clip is `a00_(30 + 6*hanger_pose)` held on its last
  frame; `hanger_pose` is category-10 byte +0x0a (`sheets/ac_part_fields.csv`).

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

- Hand-weapon fire (`acvd-game::weapons`): `acvparts.bin` category 10 fields `magazine` (+0x176,
  total ammo `総弾数`, switch `0x8247f208` case 5 → `0x82478af0`), `damp_range` (+0x138, case 4 →
  `0x82478910`, previously mislabelled `magazine`), `missile_lock_time` (+0x13a, case 11 →
  `0x8247cf48`, 60 Hz ticks), `bullet_id` (+0x13c, `0x82475bd8`), `hit_id` (+0x140, `0x82475c18`),
  `init_speed` (+0x148, case 6 → `0x8247cbf8`, m/s), `en_drain` (+0x14c, case 8 → `0x8247cec0`),
  `reload_time` (+0x158, case 7 → `0x8247cb80`, 60 Hz ticks), `no_decay_range` (+0x17c, case 3 →
  `0x8247d350`, metres), `guarantee_rate` (+0x17e, `0x82478ca8`, % at `damp_range`),
  `min_power_rate` (+0x17f, `0x82478cf8`, % at 2000 m), `damage_power` (+0x18c, case 0 →
  `0x8247d4d8`) and `damage_recoil` (+0x190, case 1 → `0x8247d4d8`) (`sheets/ac_part_fields.csv`).
  Bullet units & ballistics: `init_speed` is in m/s (`0x8247dfc8`, `0x828a3294`, `0x829c0764`
  multiply by `DAT_82004408` = 1/60 for m/tick), while `BulletRigidSt` / `BulletEnergySt`
  `max_speed_km_h` (+0x1a) and `min_speed_km_h` (+0x1c) are in km/h (`0x828dd460` / `0x828dd49c`
  multiply by `DAT_8200d118` = 1/216 for m/tick). `BulletRigidSt.gravity` (+0x2c) is in m/s per
  60 Hz tick (`0x8247dfc8` and `0x828dd580` multiply by 1/60 for m/tick²). Both bullet ticks
  (`0x828dd528` rigid, `0x828dd708` energy) check `0x8298bdf8` (`distance > damp_range` at
  `bullet + 0x294`, copied from `fire_spec + 0x3e` at `0x829c0748`) and step velocity via
  `0x828dd2e0` → `0x82830120` → `0x82830020` → `0x828dd218`: subtracting `(speed * cp + brake)`
  along velocity, subtracting `gravity` from `vel.y`, and clamping speed to `[min, max]`. Past
  `damp_range`, rigid bullets switch `air_cp` → `stalled_air_cp_h`, apply `brake`, and integrate
  extra vertical drop velocity with `stalled_air_cp_v`; energy bullets switch `air_cp` →
  `stalled_air_cp`, apply `brake`, and vanish once `stall_frames > stall_to_vanish_frame`.
  Range decay (`0x8298be98` → `0x827ca448` with `max_dist` = 2000.0 m at `0x820466cc`, applied
  by `0x827cf1c0` → `0x827cee08` / `0x827cee18`) interpolates power/recoil multiplier from 1.0
  at `no_decay_range` to `guarantee_rate * 0.01` at `damp_range` and `min_power_rate * 0.01` at
  2000 m (`RangeDecay::multiplier`, `Gun::damage_at`, `Projectile::effective_damage`).
  F / left mouse / R2 fire the right arm, C / right
  mouse / L2 the left (button defaults from `manual.fmg`). Tracers leave the weapon-root joint
  along the follow camera's pitched forward, fly with the `bulletrigid` / `bulletenergy` row
  of `bullet_id`, and despawn on the ground, on energy stall vanish, or after 5 s. `weapon_kind` (+0x0f) and
  `ready_position` (+0x153, `0x82475ac0`) are on the same sheet. A hand `_a` binder's `a00_000`/`001`/`002`/`003`
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
  overlay. DRB: `acvd-formats::drb` decodes dialogs (GLD), objects (OGLD), shapes (Sprite, MonoRect/Frame, GouraudRect/Frame, Text, Dialog, Null, AlphaAnimSprite) and textures on all 69 layouts; the module doc has every record layout. Sprite texture ids >= 1000 are runtime slots (emblems 10000+, movies 101xx, maps 102xx). `acvd-render::menu` + the `acvd-menu` viewer (`cargo run -p acvd-viewer --bin acvd-menu -- staffroll`) draw a dialog tree with its sibling `.tpf.dcx` textures; verified on staffroll (rotated strip seamless) and vssortie timer / Data_Rule (flipped corners and arrows). Text (360 `DrbShape_createText` 0x824ac270): byte +0x15 font (`fontdef.xml` ID, read by `acvd-formats::fontdef`), +0x16 align (low 2 bits left/right/center, 0x8 vertical center), +0x17 mode: 0 static RTS string at +0x1c, 1 message (bank +0x1c, id +0x20; 36-byte record; bank 1 = `menu.fmg`, e.g. PauseLabel 0x109a = PAUSE; `TextMgr_getMessage` 0x82b1a8b0, bank 2 code-filled), 2 runtime (capacity +0x1c, filled via `MenuText`), 3 special classes. `acvd-render::menu` draws them; fonts load the exact `CcmFile` (e10 ships a .ccm and the .ccf fontdef names, with different advances). Sprite blend byte: 1 alpha, 2 additive (`menu::AdditiveSprite`; `piece1/2` and `rocksight_insight` are art on black that only works added). A Dialog shape's color is applied as a multiplicative tint of its sub-dialog: inferred from `ACV_FE_LockSightCenter`, whose digit plates are the white `FE_font_base` nine-slice under `000000ff` Dialogs (360 Dialog factory 0x824ac1f8 is shared with FormSprite; the tint is not traced). `AlphaAnimSprite` (factory `0x824abaf8`, ctor `0x824ced28`, draw `0x824cedd0`, `Sprite_AlphaRef.fpo`): sprite plus mask uv / index / mirror; fill 0..1 at object +0x44 (vtable slot 0x3c `0x824cecf8`); a texel shows where mask alpha ≥ 1 − fill (`menu::MenuGauge`). In-game HUD (`acvd-game::hud`): `sortie.drb.dcx` `ACV_LockSight_base` + `ACV_FE_LockSightCenter` (laid out around (0,0), placed at screen center), letterboxed 1280x720. `Top_outline` is not drawn (a faint frame around the screen). Setup `0x829d0290`, per-frame `0x829d0b30` (vtable `0x820c3010` slot 0x5c). AP digits are the frame-part `ap` sum (`sheets/ac_part_fields.csv` +0x138; `0x8286a600`; design 5001 = 34695); EN digits are `int(EN/ENmax × 10000)` as EN1..EN3 . EN4 EN5 (`0x82596b48`), 100.00 at full. Gauges fill AP / EN at 1.0 and L/R remaining/magazine, times a show scale over StaticDrawParam `gauge_show_time` (+0x42c); below `ap_red_zone` / `en_red_zone` / `ammo_red_zone` they fade to `gauge_low_*` over `gauge_color_time` (`0x82599090`). Side `Weapon` panels at `ACV_FE_Normal` LeftArm / RightArm (`0x82598b30`) show `CurAmmo` (`0x825970f0`, `%d` ≤ 9999). Digit sprites are authored '0' at (317,135)-(330,151) of `ACV_FE_Locksight_02`, 0-9 in 13-texel steps (`0x82598218`). AP and energy stay full until damage and energy drain exist. The filled quarters in `ACV_LockSight_base` (white, alpha 10) are not drawn: stacked on the arcs their edge is a white disc. `Locksight_bar` is not drawn: it is a solid band across the crosshair. The arcs and the gauge tracks (`AP_ENBar_base`, `WeaponBar_base`) use the lock-sight digit color `0x00ffa5ff`. Player coloring and gauge shading (the options swatch; 100 is opaque) are not applied, so the ring is that green rather than the selected swatch.

- Animation timing: clips play at 60 fps (was 30, too slow) and acanimhokan blend values are read as milliseconds (was 60 Hz frames, 2-20 s fades). User-confirmed walking looks right; still unverified against the 360 code.

- Dash / air-move lean: acmotion rows 17-40 (dash) and 68+ (air move) all use anim 55 / 56 (a01_055/056, 360 frames, a key every 45 = the 8 directions, wrap at 360); the frame is a heading in degrees clockwise from forward, not time. acvd-game poses it from the velocity heading (Motion.wheel, eased at an assumed 540 deg/s). Those rows are non-looping (`b_loop` 0), so a direction change used to call `play` again and snap the frame back to 0; a wheel clip that is already showing is not restarted. A stick that passes through zero for a frame while still moving stays on the dash / air-move wheel instead of cutting to idle. `center` on a01_055 is identity rotation and a translation that swings ~1.8 m around the wheel; that slide is limited to 2.5 m/s so a strafe change does not shove the whole body. User confirmed rapid strafing no longer twitches (2026-10-06). 360 code that picks the frame and its rate not found yet; dash-jump charge (anim 57) is also a wheel clip, not wired.

- Lean command RE (360): ground dash start builder 0x82883870 (called from vtable fn 0x8284b210; turn arg 0/1/2 adds 0/1/2 to the row id, ids 0x11,0x14,... = acmotion rows 17,20,..), quick-boost 360 builder 0x828844f0 (row 218), air-float 0x82883d30 (row 222), dash-jump charge 0x828839d0 (rows 49-56). Each takes the move vec2, angle = atan2 (0x823a5d20) wrapped to +-pi, fraction = angle/2pi (+1 if negative), start time = 1 - fraction (mirrored left/right, matching the by-eye swap), sent via 0x82883370 -> cmd struct (+4 start, +8) -> 0x82854060 -> clip time at 0x8289a750. Not yet found: whether the command is re-issued each frame, which vec2 it is (stick or velocity), and any smoothing.

- Armored Core V (retail 360, no Verdict Day extras) unpacked to private/x360/v/ACV.pe (xdvdfs + xexunpack from armoredcoredumps), Ghidra project private/ghidraV (ACV, PowerPC:BE:64:A2ALT-32addr base 0x82000000, .pdata 0x82215c00 size 0x8f2b0, 88,788 functions; start the bridge with --project <abs>/private/ghidraV/ACV --program ACV.pe, pass both flags on every command). The lean command builders are identical there: dash 0x827fbba0 (called by 0x827bc190, which also stores the move vec2 at this+8/+0xc), quick-boost 0x827fc938, air-float 0x827fbfa8, jump/charge tables; helper atan 0x823433d8, command 0x827fb5f8. Callers that pass the vec2 not found yet.
