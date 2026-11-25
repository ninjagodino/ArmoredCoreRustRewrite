# Status and task list

Each chat takes **one** open task, works it, and updates this file before ending: move it to
`docs/archive.md` with a one-line summary, record findings (360 addresses, file layouts, dead ends)
under the task, and add any new tasks it uncovered. Per-system data/runtime status also lives in
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
      set 3), moves them to `WATER_RENDER_LAYER`, spawns a half-res mirrored `WaterReflectionCamera`
      (`y_refl = 2 * water_y - y_cam` with oblique clip plane at `y = water_y - 0.15` via
      `PerspectiveProjection::near_clip_plane`), and renders `WaterMaterial` (`env_water.wgsl`):
      3 world-XZ scrolled `water_n` normal layers (`g_TileScale` `0.0019 / 0.00268 / 0.0462`,
      `g_TexScroll`, `g_BumpMapSmoose = 2.0`), planar reflection sampled at mirrored screen UV
      distorted by `n.xz * g_ReflectBand (0.2)` and tinted by `g_FresnelColor (0.4784314)` (fixing
      the old flat light-gray look caused by `ID03`'s white `(1,1,1)` hemisphere ambient), Fresnel
      blend (`g_FresnelPow = 70`, `g_FresnelBias = 0.5`, `g_FresnelScale = 0.9`) over squared
      `g_WaterColor (0.15, 0.10, 0.05)`, directional specular lobes from `colors_b` (`c11`/`c12`,
      `g_SpecularPower = 10`), distance/height fog, and tonemap.
  - Done (360 tone-map sRGB parity, directional shadows, & map decal/alpha MTDs):
    - **`m_ToneMapFilter` display sRGB decode (`zoom_blur.wgsl`, `0x82c18628`, verified against
      Xenia `private/xenia/shots/ac_test_start.png` & `private/xenia/m4000_lit.jsonl`)**: the 360
      `ToneMap_{Exp,Divide,Reinhard}ToneMapPS` shaders write display-referred sRGB directly to an
      8-bit framebuffer without hardware sRGB encoding, whereas Bevy's `ZoomBlur` post-process
      writes to a linear HDR texture before the sRGB swapchain encodes it (`c^(1/2.2)`). Decoding
      the tonemap output via `srgb_decode` eliminates the double-gamma fullbright washout across
      `m4000` and the garage.
    - **Directional shadows on map pieces (`env_lit.wgsl`)**: `env_lit.wgsl` now samples Bevy's
      primary shadow-casting `DirectionalLight` cascade map (`shadows::fetch_directional_shadow`)
      with a `0.30` skylight floor (`mix(0.30, 1.0, dir_shadow)`), matching the shadowed bridge
      deck and AC ground shadow in `ac_test_start.png`.
    - **Crater decals & alpha/additive/glow map MTDs (`map.rs` `mtd_alpha_mode`, `env.rs::relight`)**:
      `m4000_map.msb` uses `Map_Diffuse_Alpha` (33 meshes, `m0200_decal02`/`m0200_decal03` ground
      craters), `Map_Speculer_Alpha` (11 meshes, `m0100_decal04`/`m0100_decal05`),
      `Map_Diffuse_alpha` (3), `Map_Diffuse_AlphaMask` (1, `net`), `Map_Diffuse_Add` (41,
      `m0100_03`), `Map_Diffuse _Grow` (2), and `Map_NoLight` (2). `map.rs` now keys its material
      cache by alpha kind and sets `AlphaMode::{Blend, Mask(0.5), Add}` + `cull_mode: None`, and
      `env::relight` routes `_Add`/`NoLight` to unlit `SkyMaterial`, `_Grow` to `emissive_boost =
      1.6`, and marks `Blend`/`Add` decals `NotShadowCaster` (removing the opaque black quad
      background on crater decals).
  - Open, in order: (1) sky leftovers: which pixel shader the sky pass binds (NoLightNoFog vs
    Flver_Sky.fpo with fog; the colour already matches the 360 shot). (2) c0 / c16 / c19 / c136
    values (taken as 1, no SSAO).
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
- **AC test targets**: spawning, rendering, hit shapes, AP/defense damage, destruction (`_b.flv` skinned gib physics via `debris.rs` using `PARAM_DESTROYPARAM_BIN` / `PARAM_DESTROYFALL_PARAM` for both enemy corpses and destructible map props), AC TEST enemy & test-AC AI (`script/enemy/*_actest.lc`, `script/arena/actest_*.lc`), and player AC damage/stagger/respawn are in (see Done "AC TEST Enemy AI & Player Damage / AP"); still open: enemy skeletal animations, navmesh pathfinding (`HNAV`/`HTR`), and vs-UNAC (`actest7`).
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
  `weapons.rs` / `damage.rs`. Follow-camera wall clip (`CamFx_stateFollow` `0x82bfb238` ->
  `0x82bfaef0`: camera ray `0x82c4d680` filter `8`, sphere probe `0x82bf9a28`, distance blend
  `0x82bfa740`) is `FollowCam::clip` (`RayFilter::Camera`, `Hit::blocks_camera`), using a 0.3 m
  margin and a 0.5 m minimum instead of the 360's 4 m minimum so a wall in the AC's back cannot
  clip. Remaining
  open points: `dash_inv` (msg `0x66` `0x8285cf48`),
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
- **Assembly gaps**: shoulder attach flag 5 (VMX `0x8288a058`; the parallel-to-+Y case
  rotates the up hint by ±90° and that axis is still unread), recon mounts, character LOD
  (`_l2` / `_l3`; map pieces are done, see Done);
  8 pending FLVERs (`0x20007` and FLVER0).
- **Maps leftovers**: rest of MSB (MODEL/EVENT/POINT/ROUTE/LAYER/TREE), `.smd`, map FLVER
  visuals, water-layer materials (none named water on m3100), collision filters 0x100002 /
  0x200002. HNAV/HTR navigation readers.
- **FCS / lock-on leftovers**: terrain line-of-sight occlusion (`LockCheckRay` `0x829c5f20`). Core Lock-on / FCS, secondary lock-on timer, lead prediction, multi-target missile lock (`missile_lock_max`, `missileLockEffectiveRate`), target lock-on HUD cursor (`MissileLock1..6`), and Scan Mode / Target Scan / Recon Units (`ACV_ScanModeLockSight`, `ACV_Scan_Cur_EnemyParamInfo`, `ACV_MapScan_ReconInfo*`) are now in (see Done).
- **Weapons leftovers** (hand-weapon tracers, bullet units, ballistics, range-decay, shoulder weapons & homing missiles, melee blades / heat piles / Boost Charge Kick, arm firing-stability reload scaling, weapon arms, sub-munition split missiles, Overed Weapons, and hand-weapon firing modes done; see Done):
  - Bay shift & purge are up to 360 parity (`acvd-game::bay`, `hanger`, `pose`, `sfx`, `sound`, `sheets/ac_states.csv`, `sheets/hanger_anim.csv`, `sheets/sound_cues.csv`, see Done): per-arm overlay (`r_kata`/`l_kata`), explicit TAE `2000..2003` / `2010..2013` (sparks FFX `4109`, steam puff FFX `98` at scale `1.5`, sounds `c00000047` / `c00000045`, `kind=101` Reattach at f40/f150, `kind=102` Purge at f10), dual-prong rack carousel (`a00_011`/`a00_021` + `r_hg_f`/`r_hg_b`), and dynamic `hanger_pose` updates.
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
  - Not drawn: distortion (43), radial blur, tracer/line actions, actions 2118/3000.

  Gameplay gaps:
  - Booster routing, QB shortage/large (`304`/`305`) + `HighBoostState` TAE (`0xC1D..0xC22`),
    ground/landing/slide/wall-kick dust (`groundsfxparam.bin`), water splashes & near-water
    spray (`envsfxparam.bin`), shell cartridges (`cartridge_sfx_id`), real 360 muzzle scale
    (`1.0` + `action 107` radial blur), Circle/Sphere emitters (`30`/`31`), and particle
    material sharing are done (see Done). Still open: heat-haze distortion (action 43) draws
    as a refractive quad; the 360 shot's blown-out white core is the additive stack plus the
    engine bloom, which Bevy's tonemap does not match.
  - Hits always use `default2`: bullet/missile wall-hit SFX do not yet map HMD surface material
    to `bullethitsfxparam.bin` columns (`hit_sfx_type` mapping is still assumed).
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

