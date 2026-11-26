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

- **Sun / lens-flare look**: sprite maths matches the 360 (see Done "360 lens-flare parity"), and
  the sun is flare-only. The sprites draw as camera-facing additive HDR quads at `FLARE_DIST`
  0.5 m in the 3D transparent phase, so `Bloom`, the god-ray source pass and `m_ToneMapFilter` see
  them like the 360's sprite batch (`0x82b95c28`) instead of a post-tonemap UI overlay. The sprite
  RGB is **not** premultiplied by the per-slot alpha (`0x82b96490` packs colour and fade
  separately); that alpha only gates the draw, and `tint.a` carries the fade: the smoothed sun
  visibility for glares/ghosts/dust, the envelope for point flares/beams. Premultiplying was what
  made the sun look dim/washed (`sun_offset_hdr.png`, core 208); the solid sprite look is
  `sun_offset_fade.png` = `sun_offset_noalpha.png`, core 255 and 204 luma at 0.10H (2560x1440,
  `--cam 424.65,77.84,649.50 --look 424.65,700,1200 --no-hud`). The saved 360 crop
  (`private/shots/ref_sun_360.png`, "6007 ARENA ATK-6") is a different env with a hazy, bright sky
  (core 226 over ~114 luma), so absolute values are not comparable; sun pitch is the msb's 43 deg,
  so placement follows the 360. Still missing: a 360 m4000 capture of the sun (the 9 yaw sweep
  shots are dusk sky with no sun) and a runtime check that the sky dome's painted glow matches the
  msb sun angle.

- **Map look**: lighting / fog / sky / tone map / per-draw light-set ids, area bounds (POINT kinds 100-102), destructible map props (`_b.flv` / `_b_h.hmd`), and water return (`POINT` kind 50 + `env::Scene.water_height`, active only when the map has kind-50 points) are in (see below). The `o0006` `signboardGrow` screens' `movie` texture is a runtime render target, not an asset (see below).
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
      `env::relight` routes `_Add`/`NoLight` to unlit `SkyMaterial`, `_Grow`/`_Glow` to
      `emissive_boost = 32.0` with `cull_mode: None`, and marks `Blend`/`Add` decals
      `NotShadowCaster` (removing the opaque black quad background on crater decals). The two
      soft white diagonal dashes beside the first bridge in `ac_test_start.png` were initially
      read as world light beams, but they are **HUD gauge end-caps** (bit-identical at
      (460,365)/(530,365) and (189,364)/(273,364) in `ac_test_start`/`_move`/`_arena`, each pair
      joined by a dotted horizontal bar), so the pixel-derived `lens_flare::M4000_BEAMS` billboards
      and the `beam.m4000.*` sheet rows were **deleted** — nothing world-space sat on those pixels.
      Nearby `o2135` street-lamp
      heads (`Map_Diffuse _Grow` / `Simple_Grow` LODs) and `m0510` traffic strips share the
      `_Grow` emission path.
      `Flver_ColDifEmi.fpo` squares the emission texture; the display-referred boost is calibrated
      against the 360 shot because the lamp MTD keeps `g_EmissionCoef 0.3`, `signboardGrow.mtd`
      keeps `1.0`, and Bevy's bloom weights a bright pixel by screen area.
    - **Map beams / light shafts — no authored data (done, 2026-11-25)**: `m4000` has **no beam
      geometry and no light record**. Every `POINT_PARAM_ST` in `m4000.msb` / `_map.msb` /
      `_actest.msb` / `_aitest.msb` / `_env.msb` is kinds `50/100/101/102/200/850/1000/3000` — no
      `1300` and no `1310` spot lights; MODEL kinds are `0`/`1` (geometry), `2` (enemies, light
      set 20), `4` (AC); the only EVENT is the kind-101 tone-map block. All 1437 placed meshes
      (`private/tmp/m4000_all.txt`) carry only ground-glow `Map_Diffuse_Add` (`m0100_03`) and
      `m100_signboard_01` screens. Disc-wide `envlights --scan` finds point lights only in
      `m3430`/`m3750` (10 each) and **no spot lights in any map**; `--shafts-scan` finds nolight
      cards only in `m3230`/`m3231` (`o9331`) and `m3750` (`o4024`). The VMX128 trace of the
      spot-light path (`py tools\x360dis\x360dis.py --func 0x82be7d08 --refs`, which decodes
      Ghidra's "bad instruction data"): it reads a POINT `shape 3` record, stores pos at object
      `+0x20` and dir/FOV at `+0x30` in a 0x1c0-byte entity (ctor `0x82c03640`, vtable
      `0x820f59a8`); the vtable methods (`0x82c03930`, `0x82c039a0`, `0x82c031b8`, `0x82c03278`,
      `0x8269bcb0`) are light lifecycle only, **no beam draw**. `AppFxLensFlareDrawEntity`
      (`0x820ff920`, class meta `0x83ade720`, vtable `0x820ff994`, registrar `0x82c7b2b0`) is a
      reflection registration. Conclusion: m4000 and every map have **0 beams**; the last two
      in-tree fake billboards were removed. **Never re-derive placements from screenshots** (HUD
      end-caps at (460,365)/(530,365)/(189,364)/(273,364) in the three `ac_test_*` frames prove
      the trap; one view cannot recover depth).
    - **Map-object TAE FFX (done, 2026-11-25)**: map objects ship
      `tae/obj/<oXXXX>/own000100.tae` (176 files); 141 of them carry `KIND_FFX` (0) events that
      spawn an FFX id on a FLVER dummy (`dummy.color[1]` is the TAE dummy id, as on AC parts).
      `map::object_effects` now reads each placed `KIND_OBJECT` part's TAE, keeps the frame-0 FFX
      events, resolves the dummy through `Flver::bone_transforms`, and `map::spawn` attaches a
      persistent `sfx::Sfx` at the mirrored dummy socket. `m4000` logs **12 object FFX** — its six
      `o0006` signboards each fire `ffx 197` (texture 226, 3 m warm-white `(1.0,0.78,0.70)` additive
      billboard, node life -1/60 s = infinite, action 59) on dummies `200`/`201`, the two lamp arms
      at ±19.67, 18.47, -7.15 m. Test
      `o0006_sign_tae_spawns_ffx_197_on_its_two_lamp_dummies`; dump any TAE with
      `cargo run --release -p acvd-formats --example taedump -- tae/obj/o0006/own000100.tae`
      (all 176 in `private/tmp/obj_tae_dump.txt`). Other object models' glows ride the same path.
    - **Dynamic aircraft TAE FFX (done, 2026-11-26)**: an aircraft placement plays the `.tae`
      animation whose id equals its ANI clip id alongside the clip (the behavior timeline only
      selects the clip via kind 104). `map::step_aircraft` crosses that timeline with the clip
      frame and spawns each `KIND_FFX` event as `sfx::Sfx` on the model's `Rig::effects` socket
      (`dummy.color[1]`), parented to the dummy bone so body/rotor sockets follow the pose.
      `o9330` clip 210 fires 5709/5710/7510 on dummies 150-154/162/170-173 and clip 1000 fires
      4083 on 160-166; `o9340` clips 210/211/220 fire 4806/5105/5202/4085 (dummies 201/211-213/
      244-246). `o9341`/`o9344` TAE files carry **no** `KIND_FFX` events, so the flying-wing
      variants have no authored FX. Verified live on m3330 (`9 animated aircraft, 2 with TAE FFX`;
      shot `private/shots/bomber_fx_m3330b.png`); test
      `map::tests::bomber_tae_effects_name_model_dummies` pins ids, sockets and crossing frame.
    - **Per-model map texture pack (`map.rs` `texture_pack` / `Textures::load_tex`)**: object
      models (`KIND_OBJECT`, e.g. the road fence `o2076`) resolved every texture name through the
      disc-wide `best_texture_ref`, so `net`/`barbed_wire`/`o2036` came from unrelated packs
      (`net` exists nowhere in `m4000_htdcx.bnd`; the global pick was `m3200_htdcx.bnd`'s dense
      256x256 bright lattice, RGB 69-142) and the fence drew as thick white mesh/posts.
      `model/obj/o2076/o2076.tpf.dcx` holds its own thin 128x128 `net` (index 1, RGB 8-60) beside
      `barbed_wire` (0) and `o2036` (2); `map.rs` now opens `model/obj/{model}/{model}.tpf.dcx`
      first for object parts (map pieces keep the `{name}.tpf.dcx` members of `{map}_htdcx.bnd`)
      and keys the image/material caches by pack so same-named textures in different packs never
      collide. Global name lookup stays as the fallback for shared textures with no per-model
      entry. Verified with `private/shots/fence_fixed_b.png`; test
      `map::tests::object_textures_resolve_from_their_own_pack`.
      Broken `_b.flv` gibs (`debris.rs`) built their materials with the same bare-name
      `acvd_render::material`, so `e0010_01` / `o2070_01` / crate names resolved to an unrelated
      brighter pack and the chunks drew white/checkered. `acvd_render::material_in_pack` now tries
      the `model/obj|ene/{m}/{m}.tpf.dcx` derived from the `_m.bnd.dcx` asset first (name-only
      lookup as fallback), and gib materials reuse `map::mtd_alpha_mode` so
      `Map_Diffuse_Alpha`/`AlphaMask` scrap no longer renders as solid quads
      (`debris::tests::gib_textures_resolve_inside_the_model_pack`).
      Remaining gib look: chunks still use the generic `StandardMaterial` (diffuse + bump only)
      and sit outside any `MapPart`, so `env::relight` does not apply the map's fog/light set to
      them.
    - **`o0006` `signboardGrow` `movie` texture is a runtime render target, not a missing asset**:
      the FLVER material (`model/obj/o0006/o0006.flv` mat 4, `signboardGrow.mtd` ->
      `Phong_ColDifEmi.spx`) names texture path `movie` for both `g_DiffuseTexture` and
      `g_EmissionTexture`; `o0006.tpf.dcx` holds 7 textures and the disc-wide index (34,521 texture
      refs) has no `movie`, so it cannot be loaded. The 360 fills a scene named-target map
      (`FUN_82646580` `0x82646580` builds it at `this+0xa2010`; insert `FUN_82bd2c40`
      `0x82bd2c40`, lookup `FUN_82bd2b00` `0x82bd2b00`) with runtime surfaces: `g_Color` /
      `g_Decal` from the render-target table (`FUN_82bc0ec8` ids 1/6) and `movie` from the FMV
      player (`FUN_824a8890` `0x824a8890`, global fallback `0x83a91474`; the env class vtable
      `0x8208ce30` slot `0x28` = `FUN_82801168` sets/erases the same `movie` key). In AC TEST no
      FMV plays, so the sampler is unbound and the white `g_EmissionColor` (coef `1.0`) gives the
      white glowing screens the 360 shot shows; `map.rs` recognises `movie`/`g_Color`/`g_Decal`
      as runtime names so they are not reported as missing, and `env::relight` keeps the white
      fallback. Real screen content would need the `movie/jp/*.wmv` VC-1 decode (see Movies).
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
    passes a tick, skipping triangles with |normal.y| >= cos(55 degrees). Since the body spheres
    are floor-tangent, `push_out` also skips static contacts at or below `feet + STEP_UP` (1.0 m,
    not game data) and the grounded ray probes `travel x tan(55)` above the feet, so kerbs are
    stepped over and 45-55 degree ramps are climbed; steeper than 55 degrees still acts as a wall
    (see Done "AC wall collision"). Airborne movement uses the same upward probe and lands on
    ground that rises through the feet, so a glide boost or a plain airborne boost into an incline
    lands instead of clipping; the 360's own step-up/step-down height and its exact
    walkable-slope limit are still unknown (45 degrees is only its wall-jump floor/wall split).
  - Walk speed: the 360 AC TEST AC walks at 0.12 m per tick at entity `+0x130`
    (`private/xenia/wall_walk3.jsonl`), the runtime's default design 5001 at 0.249; check
    whether that is the design (AC TEST may use another AC) or a 2x step.
  - Slopes too steep to stand on (the runtime treats >55 degrees as a wall rather than sliding
    down; unverified against the 360), AC vs objects that move, AC vs AC (wall kick /
    `ackickrhit` filter is now in, see Done "Wall Kick / Wall Jump").
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
  - Glide: states 4/5 take the separate mover path `0x82822ea0` -> `0x8281e408` with the working
    block `+0x288` (9 words copied from object `+0x8c..0xac`), max `block+8` = object `+0x94` x
    scale `+0x270`, accel at `+0x2cc` (built by `0x8281deb8`: `rot(stick) x speed x 60 x scale`).
    Below max it is `v = normalize(v + a) * min(|v + a|, max)`; at/above max it is
    `normalize(v + a) * |v|`, so the glide **holds** its entry speed (no brake) and only steers.
    Probe `private/xenia/glide_move.jsonl`: 0.4630 m/tick = 100.0 km/h held through states 4 and 5.
    Done: the vertical update `0x82820168` skips the gravity loop
    (`0x8281f960`) in states 4/5 (`0x8281f3d0` at `0x828201b4`), so a glide holds its height
    and carries on over a drop. `GlideState` (vtable `0x820926c8`, ctor `0x8283ddc8`,
    activation `0x82844fd8`, update `0x82841b38`) runs the ground move (slot `+0x34` =
    `0x8281f540`, state 4) for `glide_ground_phase` = **0.5 s** (`fRam83713a94`) and then the
    air move (slot `+0x38` = `0x82823618` -> `0x8281f8c0`, state 5) until the glide ends
    (`sheets/ac_ctrl_calc.csv`). The activation ascends by a fixed amount: `0x8281dd28`
    clamps the rise at glide block (+0x288) word +0x0 = **5.0 m** (`glide_lift_height`, built
    from the param table `+0x468` x f30 by `0x82821af0`) above the activation height (block
    +0x2c = object `+0x2b4`, seeded by `0x8281f8c0`), stepping at `0.1 m/tick`. The ascent
    spans both phases: Xenia probe `private/xenia/glide_cap.jsonl` stays in state 4 for 0.5 s
    (23.230 -> ~25.7 m) and state 5 finishes the climb to 28.230 m at 0.1007 m/tick. The
    runtime now switches phase at 0.5 s, marks the AC airborne then (the start clip hands over
    to the air entry while still rising), and caps the absolute lift at activation + 5.0 m;
    leaving a ledge after the ascent adds no further rise. On the ground: `glide_start` (acmotion row 217)
    then `glide` (row 218, 360-frame wheel, booster clip 1). In the air: one directional entry
    (rows 223-230, anims 63-70, 30 frames, sector `0x82825918` / builder `0x82883c80`) then
    `glide_air` (row 222 空中浮遊, anim 62, 360-frame wheel, booster clip 3, builder
    `0x82883d30`). Camera action 26, cue b00000006 (taken as the start). Glide EN drain and
    depletion end are in (see Done "Generator Energy (EN) drain & recharge"). The start/keep
    heading gate is in: `0x8281ea30` (state 4) and `0x8281e898` (state 5, via `0x8281f8c0`)
    return 0 and end the glide when `cos(stick - velocity) < -sin(block +0x20)`; the block angle
    is a 10 deg allowance (Xenia probe `private/xenia/glide_gate.jsonl`: 0.17453292 rad at both
    entries), so the stick may sit up to 100 deg off the current velocity
    (`sheets/ac_ctrl_calc.csv` `glide_turn_cone`). The air entry also seeds the mover: horizontal
    acceleration from the thrust block `+0x2cc` (recomputed per tick here), while `+0x1e8` is
    copied from `+0x2dc`, the lift accumulator that `0x8281db00` advances and `0x8281dd28` stops
    at activation + 5.0 m (`glide_cap.jsonl` reads 0.00226 at the state-5 entry, then 0 at the
    ceiling); after the ascent the air glide holds a flat height. Done: the glide quick boost —
    a tapped Hi-Boost during a glide keeps the glide (the 360 glide top-level action vtable
    `0x82092318`, update `0x8283f240`, stays alive while `HighBoostState` replaces the movement
    substate; its `+0x1c` slot `0x8283f360` and `+0x24` slot `0x8283fcb8` start the glide
    sub-actions, and `FUN_82844c78` re-creates the `0x82092318` state). Holding Hi-Boost through
    `QbPhase::Phase2` remains the Boost Charge Kick (`0x8283fdb0`); the old immediate
    `qb_held` -> `start_kick` on the first glide tick was the bug that exited the glide on a tap.
    `Pilot::glide_qb` keeps `glide`/the phase timer/lift through the burst so the glide resumes;
    SHOT telemetry reports `glide_qb`. Done: the action-40 ribbon
    trails (f0000134, 3 starts from `0x828a8088`, dummies 153/155/156/213/218/223 in TAE anims
    63-70) render in `sfx.rs`. Left: the exact state-4 vertical rate (the runtime shares
    `glide_rise_tick` across both phases; the probe's state-4 leg is ~0.074 m/tick vs 0.1007 in
    state 5) and damage rows 239-242 / 246-247 (not played).
  - **Glide Boost VFX 1:1** (action-40 ribbon `f0000134`, `acvd-game::sfx`): action 40 is the
    Tracer, traced (see Done "Glide Boost start ascent & ribbon trail"): registered at
    `0x83198538`, def reader `0x835b3e10`, per-segment update `0x83197e90` -> `0x835b48f0` ->
    strip emitter `0x83110dd0` / `0x83118b68`; width/colour/UV are evaluated per emitted sample
    and alpha fades over slot 15 (`def+0x1c`, 0.5 s). `Look::Ribbon` now does this
    (`ribbon_batch`, tests `glide_ribbon_reads_f0000134_action_40` and
    `ribbon_batch_fades_and_sizes_each_sample_by_its_age`). Remaining: confirm the live 360
    values with `private/xenia/ribbon_seg.txt` (the `boost_glide` recipe did not reach the glide
    that run: `glide=0`, no `f134`), settle the cross-section orientation (the 360 crosses two
    tangents; the rewrite uses the camera) and compare to `private/xenia/shots/glide_{1,2,3}s6.png`.
    The CLI cannot stagger Ctrl after boost (`--hold v,w,ctrl` leaves `glide=0`), so the live
    capture needs a recipe or a staged input. `--sfx 134` preview is a dead end (spawns 20 m
    ahead, `total_active_sfx=0`).
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
  0x200002. HNAV/HTR navigation readers. The garage sortie launcher now lists the 33 retail
  battle-map folders from `param/battlemapinfo.bin` + `menu.fmg` names (see Done "Sortie map
  selection"); the mission variants without their own `{map}_m.dcx.bnd` / `{map}_htdcx.bnd`
  (m3104, m3121, m3201, m3202, ...) still need the shared base-binder lookup before they can be
  listed.
- **Armored Core V (base game) maps — started (2026-11-26)**: the ACV 360 disc reads through the
  same VFS (`vfs::X360_ISO_V`); `X360::open` now picks the single `bind/dvdbnd5.bhd` +
  `bind/dvdbnd.bdt` layer when Verdict Day's `bind/dvdbnd5_layer0.bhd` is absent
  (`vfs::tests::acv_disc_reads_single_layer_archive`). ACV has no committed BHD5 name list, so
  `files()` reports only the loose files, boot-bundle members and scripts (644 paths) and reads are
  by path hash. `cargo run -p acvd-formats --example
  acv_maps` surveys it: ACV ships map layout in **97** `bind/mission/{mission,BattleFeild}_XXXX.bnd`
  BND3 binders (BHD5 entries, not loose like VD); each holds a `mission/*.xml` `<AreaList>` naming
  the `<Chr>`/`<Map>` MSBs plus `<Env>`/`<Nav>`, while the map-piece FLVERs are individual
  top-level files `model/map/{map}/{model}.flv` (+ `_l1.flv`), with `{map}_m.mlb` and
  `{map}_l.tpf.dcx`, and no VD-style `{map}_m.dcx.bnd` / `{map}_htdcx.bnd`. All 174 referenced MSBs
  parse with the shared `msb` reader, 0 failures (35,722 MODEL, 182,409 PARTS, 7,292 POINT
  records); 20 top-level map folders have `_m.mlb` (m0050, m0100, m0299, m0300, m0400, m0500,
  m0600, m0650, m0700, m0750, m0800, m0900, m1000, m2000, m2100, m2200, m2300, m2500, m5000,
  m5001), 17 of them named by mission XMLs. ACV's `POINT_PARAM_ST` adds kinds 0, 1, 250, 260, 270,
  280, 400, 500, 650, 1100, 2000, 3100 to VD's set, and `PARTS_PARAM_ST` has kinds 0/1/2/4 plus
  0xFFFFFFFF (1,503 records). ACV `param/battlemapinfo.bin` (1,463 B, `BATTLE_MAP_INFO` PARAMDEF
  104, **0x54**-byte rows vs VD's 0x5c) has 13 maps with `MapID` 3000-3004 / 5000-5700, new
  `MapMinY`/`MapMaxY` (+0x3c/+0x40) and `BeginPlaygID` (+0x44), and no `practice_map_tex_id` /
  `map_name_id` / thumbnail fields. `system/acv.ini` in `bind/boot.bnd` sets
  `$(AcTestMap)=m720/m720.msb`, `$(TutorialMap)=m710/m710.msb`,
  `$(VersusDefaultMap)=mission/BattleFeild_5000.xml`; `mission_6170.bnd` is a fully commented-out
  arena XML that still names m0001. Model sharing is **not** a plain global folder search. Only the
  mission-specific MSBs (`m0300_mission1200/1201/1209/4023`) reference the 199 models
  m1012..m1210 missing from `m0300`; `m0300_map.msb` and mission_0003/4022/4024/4025 reference
  none, so the mission-0003 path is self-contained. Same-name FLVs are different assets: m1012 is
  a 4 KB/3-vertex dummy in m0500/m0750/m0800/m2100/m2200 and a real 2.1 MB FLVER only in m0600;
  of the 199, 151 have only dummy copies anywhere and 48 have exactly one real copy (46 in m2200,
  2 in m0600). `MODEL_PARAM_ST` has a name rel at `+0x00` (inline at `+0x1c` for m1012) and a SIB
  path rel at `+0x0c` (`N:\ACV\data\model\map\m0300\model_sib\m1012.sib`);
  `PARTS_PARAM_ST` has a SIB path rel at `+0x10` (`...\m0300\layout\layout.SIB`). Those SIB
  strings are build-source paths, not runtime archive paths. `_m.mlb` is the shipped link data:
  697 type-`0x80` sections in `m0300_m.mlb` (724 in `m0500_m.mlb`), each with source SIB,
  materials and model name; `m0300_m.mlb` has no m1012 section, m0500/m0600 do. ACV's
  `MapPartsLinkModelDataProxy` is vtable `0x820c60f4` (slot `+0x38` = `0x82988038`), ctor
  `0x82988a00`, factory `0x8298cc88`; `0x82988038` -> path builder `0x82a14540` -> loader
  `0x82a13010`, and `0x82a14540` builds `bnddev_%s` (`0x820d372c`) with `%s:` (`0x820d3708`).
  The ACV Xenia AC Test probe (recipe `private/xenia/acv_test.txt`, log
  `private/xenia/acv_model.jsonl`) built `bnddev_m0100_m_dcx:/m0102.flv` and
  `bnddev_m0100_m_dcx:/m0050.flv` and requested `m0012.flv` / `m0050.flv`, all of which exist
  in the current map folder m0100; boot also preloaded `m0299_m.mlb` / `m0100_m.mlb`. No
  global-name lookup was observed, so the rewrite rule is the current map's
  `model/map/{map}/{model}.flv`, skipping absent models; treat the 151 dummy-only mission-1200
  parts as unused/hidden; `vfs::tests::acv_map_models_resolve_in_the_current_map_folder` pins
  the current-folder/dummy/real copies. Mission 1200 itself was not probed. The ACV static index
  is usable after `acvd-index` was changed to locate the PE `.pdata` section (ACV `0x82215c00`,
  VD's fixed `0x8225c200`): `build --pe private/x360/v/ACV.pe --root private/tmp/acvroot`
  indexes 107,832 functions, and `q const 0x820d372c` finds only the key builder `0x82a14d60`.
  VD's equivalent is vtable
  `0x8210c624` (secondary vfptr at `+0x62c`), ctor `0x82d0f750`; slot 14 `0x82d0f680` ->
  `0x82c2d4d0`, which appends the model name to `context+0xc`; VD source path `0x8210c5c0`,
  ACV `0x820c6090` (`0x82d0eee0` was a thunk). ACV `ch_env/*_env.msb` use the **plain**
  `msb::MAGIC` (0x00989A6A), not VD's `MAGIC_ENV` (0x00989A6C). `env::read` now accepts both.
  ACV's type-100 handler `0x827626d8` gives two i16 angle pairs and six RGBA byte colours
  (`acv_light`; the m0300 test checks light 0 at 15/5 and 30/20), and the EVENT table/BGM parse
  (50 events, BGM 103/1001). Its compact fog block (`+0x30..+0x4d`) is now decoded: s32
  distance start/range (view-space z), RGB distance colour, u8 min/max alpha and a range-sign
  byte, then s32 height start/range, RGB height colour, u8 min/max alpha and a range-sign byte.
  The runtime writes the 0xa0-byte slot `+0/+4/+0x10/+0x20/+0x24/+0x28/+0x2c/+0x30/+0x40/+0x50/
  +0x54/+0x58`; `0x82b05428` scales the raw byte colours/alphas by 1/255 (`0x82017d6c`), and
  `0x8256d328` consumes the selected slot. ACV's map VS/PS use `c208`/`c215` and
  `c98`/`c61`/`c99` fog constants (`Normal/Flver_PNT02.xvu`, `Normal_PT6/Flver_ColDif.xpu`).
  `Fog` holds the shared fields; `Fog.acv` preserves the lower alpha bounds and both range-sign
  flags. m0300 ID01 (`slot 1`) has distance `-750/820`, height `0/24`, colours
  `(90,120,110)`/`(126,215,175)`, alpha maxima `90/255`/`100/255`. ACV's type-101 event bridge
  `0x82762160` (scene branch `0x827621ec`) keeps the core scene in the first sub-block
  (`data + u32(data)`): clear RGB `+0..2`, draw distance `+4`, water height `+8`, and sky scale
  `+0x1c` (used only when `> 1`). `acv_scene` decodes those; `Env::acv_scenes` keeps every type-101
  scene event in encounter order and `scene` is the last one (last-event-wins). m0300 is clear
  `(0x25,0x32,0x31)`, far `10000`, water `-1000`, sky scale `100`. m0300 and m0600
  have two type-101 events (`lightning filter` then `scene environment parameters`), both carrying
  the same 12-entry sub-block table; m0100 has only the scene event. m0300's first and last `+0x30`
  records are byte-identical (`3c/00/00`, `0.2`, `-0.13`, `0.06`), but m0600's first lightning
  filter (`kind 0x50`, `0.5/0.1/-0.02/0.005`) differs from its last scene filter (`kind 0xff`,
  `0.35`), so both events are retained. The type-101 lens flare at
  `data + u32(data + 0x14)` (`0x8275de18`) is decoded: same header as VD, with glare offsets at
  `+0x48`, god ray at `+0x90` and dust at `+0x94`. m0300's last scene has angles `17/29`, 4 glares,
  5 enabled ghosts (of 18 slots), god-ray `tex_id -1`, dust pos `(0,208)` size `(512,512)`.
  The type-101 `+0x1c` sub-block is not the type-100 fog: `0x8275e2c0` maps it into a 0x48-byte
  atmosphere/weather record and `0x82b12930` copies that record to `[0x838a99c0]+0x10+0xd130`
  (defaults distance `150/300`, `500/9999`, `-1/-0.1`, `-0.0001`, `400/1`; m0300's lightning
  event changes `60/2000`, `8/9999`, `0.0001`, `400/190/500`, its normal event `10/100`, `0.9/80`,
  `0.15`, `400/80/140`). Type-102/103 are separate wind/rain payloads; fog remains the type-100
  compact slot + `fog.bin`. The `+0x30` sub-block is the screen filter: `0x8275d4f0` copies
  `+1..+3`, the `+4..+0x30` floats and `+0x1c..+0x20` bytes into the filter singleton at
  `0x8387c700+0x55c0` via `0x82569e10`. The `+0x34` sub-block matches DOF: m0600's `100/1.57/7/6/4`
  matches the Dof filter object (`0x82541fb8`, execute `0x825422fc` reads `+0xf8..+0x104`), while
  m0300 is disabled (all zero except `+0x30=1`). The layouts are now parsed into
  `Scene.acv_filter` / `acv_dof`; neither is rendered yet. The disc tests
  `env::tests::acv_env_from_mission_binder` and
  `env::tests::acv_two_scene_events_keep_first_filter` cover core, lens flare, both filters and DOF.
  `acv_scene_core_layout`, `acv_lens_flare_layout`, `acv_filter_dof_layout` and
  `acv_light_fog_layout` pin the parsers without the disc. `m1000` is not a map with models:
  mission_0010 pairs `ch_env/m1000_env.msb` with m0300, and `script/event_include/m1000_*` exists.
  `m0650`/`m5000`/`m5001` are referenced by no mission XML, script, or executable string, so
  their `_m.mlb` files are unused/cut and need no loader. Still open: ACV type-101
  screen-filter/DOF rendering, the `+0x1c` atmosphere/tone-map parse and the remaining unnamed
  sub-blocks, HNAV/HTR and the `.mtd` binders.
- **FCS / lock-on leftovers**: terrain line-of-sight occlusion (`LockCheckRay` `0x829c5f20`). Core Lock-on / FCS, secondary lock-on timer, lead prediction, multi-target missile lock (`missile_lock_max`, `missileLockEffectiveRate`), the per-enemy target lock-on HUD cursor (one four-wing `_E`/`_T`/`_L`/`_R` group per enemy in the FCS cone, up to 8, each wing placing its authored 128×64 corner dialog on the distance-scaled 32×32 target rect; text/bars/pips only on the FCS-selected target, `MissileLock1..6`), and Scan Mode / Target Scan / Recon Units (`ACV_ScanModeLockSight`, `ACV_Scan_Cur_EnemyParamInfo`, `ACV_MapScan_ReconInfo*`) are now in (see Done). Retail lock-cursor appearance/rule confirmed from the build instead of a sortie capture: `sortie.drb` authors every element (`LockOnText` msg 2313, `MissileLock1..6`, `EnemyName`/`EnemyAP`, and the 32x32 `_Target` with `LockColor1st` `0x006680ff` / `LockColor2nd` `0xff4000ff`); `0x829d4a90` bakes those color defaults and `0x829d4b88` overrides them from the dialog's named colors. `0x829d4558` -> `0x829d30e0` draws each wing only when the candidate's `+0` lock flag or `i16 +2` missile-lock count is set and `+1` or `f32 +4` >= 1.0; `0x829d1a10` shows `MissileLock1..n` from `i16 +2`, `0x829d1ac0` fills `ReloadBar` from `f32 +4` while `CursorLine` stays at full fill. The Xenia runs drew no cursor because the tanks sat outside the FCS sight cone (aim), not because of a missing code path; the 32×32 `_Target` is only the `LockColor1st`/`LockColor2nd` source (never painted), and its 1st/2nd selection is read through that candidate state. `0x829d3b38` builds the box from `rectOf(group+4)`; `0x829d4dd8` -> `0x824ab278` instantiates `ACV_OutLine_LockCursor_Target` (dialog name at `0x820c35f0`, call site `0x829d50ec`), so that rect is the dialog's authored 32×32, scaled by `LockSight+0x2c` and centred on the projected point. The four corner arms are the 128×64 wings' `CursorLine` sprites anchored on that rect; the earlier "target-sized normal-mode frame" TODO is therefore closed (authored 32×32, no enemy AABB). The FCS remains live in Scan Mode: `AC + 0xdb0` is read by scan/control, not the `0x829d...` lock path, so `LOCK ON` and the enemy name stay visible while Scanning (matching the retail capture). `EnemyInfo` is a `Null` object in dialog 14 (no visible text), and the lock effect drawn over the enemy model remains open. Each wing now picks its own `LockColor1st`/`2nd` swatch from its candidate record (`0x829d19f0`) and the `ReloadBar` fills with clamp(`f32 +4`) (`0x829d1ac0`), not the weapon cooldown. The reference capture is the combat `ACV_LockSight_base` art (tick ring), not `ACV_ScanModeLockSight`. The two scan weapon panels are traced but not spawned: `ACV_Scan_Cur_EnemyWeaponInfo` (dialog 109 280×300, `0x829d3218` type 2 at `centerRect.x0 - 280s, centerRect.y1`; `EnemyWep0..4` are dialog 92 197×44 filled from the target struct `+8` count and `+0x20` array stride `0x1ac`: `WaponName +0`, `WeaponPlace +0x110`, `DamageType +0x15c`, `Damage +0x1a4`; `DamageType`/`Damage` hidden when damage `< 1`) and `ACV_Scan_Cur_WeaponInfo` (dialog 110 400×260, `0x829d3218` type 1 at `centerRect.x1, centerRect.y0 - 260s`; `ScanWeaponDlgItem0..4` are dialog 89 357×68, manager `0x825a60a0`/`0x825a5c90` over the five slots at `0x838c7fd0..0x838c7fe4`, 6×`0xc0` records, layout `0x825a49c0`). Normal-mode (Combat Mode) lock acquisition is now traced from the raw lock script: every candidate is accepted on the outer `sight + 2°` cone (bit 0 of `0x829d8850`; `0x829d8d58` accepts that bit alone and uses bit 1 only to mark the inner sight), `0x8287c560` records each candidate's 3D distance and `0x82876b40` keeps the current target while the slot tracker state is 2, re-feeding the candidate list only when it is not (`FUN_82877320`), so selection is nearest-distance with a sticky current target, and the old per-frame 0.85 keep-current bias is gone; the outer `+2°` cone is the hysteresis. Still open for exact 1:1: the shelter LOS sub-branch (`0x829d8d58` when lock-context `+0x124 != 0` and candidate `+0x18 == 0`, via `0x829e9380`/`0x829e87c8`/`0x829e8810`, with the range cap only when flag bit 2 is set).
- **Weapons leftovers** (hand-weapon tracers, bullet units, ballistics, range-decay, shoulder weapons & homing missiles, melee blades / heat piles / Boost Charge Kick, arm firing-stability reload scaling, weapon arms, sub-munition split missiles, Overed Weapons, and hand-weapon firing modes done; see Done):
  - Bay shift & purge are up to 360 parity (`acvd-game::bay`, `hanger`, `pose`, `sfx`, `sound`, `sheets/ac_states.csv`, `sheets/hanger_anim.csv`, `sheets/sound_cues.csv`, see Done): per-arm overlay (`r_kata`/`l_kata`), explicit TAE `2000..2003` / `2010..2013` (sparks FFX `4109`, steam puff FFX `98` at scale `1.5`, sounds `c00000047` / `c00000045`, `kind=101` Reattach at f40/f150, `kind=102` Purge at f10), dual-prong rack carousel (`a00_011`/`a00_021` + `r_hg_f`/`r_hg_b`), and dynamic `hanger_pose` updates.
  - Not done: core aim for weapon fire (shots follow the
    camera pitch, not a posed upper body). OW `motion_ow_equip` body animation bank switching (`0x8284e9d8`), dynamic re-attachment of OW root `60` onto arm dummy `60` (`acattachinfo` record 1) during deployment, Overed Weapons (`acvparts` cat-11 `110..6010`, `OwCtrl` `0x82885c40..0x82886948`, `ACV_OWLockSight` / `ACV_OW_ErrorLog`), hand-weapon firing modes (full-auto, simultaneous multi-pellet spread, sequential multi-round burst, hold-to-charge / release-to-fire `0x82475b00` / `0x8247d6e0`), arm firing-stability & attack-balance reload scaling (`0x8286bd00` / `0x8286adf8`), weapon arms (`acvparts` cat-2 `+0x13c == 1` / `+0xf4` -> cat-10 `9020..9620`), sub-munition split missiles (`close_act_id == 8` / `shoot_bullet_id` -> `BULLET_BULLETARISE_BIN`), energy weapon per-shot EN drain (`en_drain`,
    `0x828a1820`), shoulder weapons & homing missiles (`bulletmissile`), melee blades & heat piles (`bulletblade`), Boost Charge Kick (`KickState`), and target hit shapes / defense mitigation are now wired. The lock-sight HUD shows AP, live energy, the
    `AlphaAnimSprite` gauges, hand-weapon / weapon-arm ammo counts, shoulder-weapon ammo (`SWep*` / `Gauge_SWeapon`), and OW lock-sight / overload error banners (`acvd-game::hud`).
  - Firing clips: `ready_position` (+0x153) and `weapon_kind` (+0x0f) are on the sheet. The
    arm clip request is `AimArm` `0x8284a570` -> `0x8289e808` -> `0x82882ad0` (clip 1) for a held
    hand weapon, and the shoulder-slot branch of the part shoot handler `0x8289de30`
    (`0x82882b58`, clip 3, re-requested every frame the shoulder trigger is down; probe
    `private/xenia/shoulder_anim2.txt`, 0x82847e20 tail-calls it). Combat now ticks the arm's
    shoulder-piece clips from the shoulder trigger too, matching the garage action test. Open:
    joint-control bit 31 at
    `0x82456f68` selects `sniper_$(LR)` vs `gun_$(LR)` and is not tied back to +0x153.
    Shots wait until the deploy clip ends; that timing is the clip, not a TAE event.
    Ready weapons (`ready_position`: cannons, sniper cannons, the other heavy classes) play
    acmotion 78/75 while fire is held and 79/76 on release (`sheets/ac_states.csv`; the blend
    rows are キャノン構え / キャノン解除, b_apply_lower 1). Sniper rifles (kind 5) do not. The arm
    clip only overrides the bones it animates (the `r_shoul_p*` / `l_shoul_p*` pads), so it
    does not plant the rest of the arm back on the clip bind: those tracks are two identical
    identity keys, and writing them was erasing acmotion 78's `r_arm01` pose. The arm's
    `a00_001`/`a00_003` play on that side's bones while a ready weapon is deployed or the
    shoulder trigger is held.
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
    - **Restored 2026-11-25**: `bay` now keeps a persistent `Shift` resource, so the old
      `if body.shift.is_none()` gate skipped the whole aim/recoil block on every frame and the
      hand-arm raise (and its shot kick) never stepped. The block now runs each frame and skips
      only the hand whose bay shift is active (`Shift::is_active`). Trace with `--shot`: the new
      `weapons::probe_shot` logs `SHOT ... aim r=(hold weight frame limit turn err aim_y)` per
      hand every frame (`--hold f --pitch 35` on design 5001: `weight=1.00 frame=1 err=0.0`,
      `turn` stays 0 while Core Control already holds the barrel on the camera aim). Test
      `hand_arm_aim_notes_the_jcondata_shoulder` pins the `gun_$(LR)` `_arm01` shoulder, its
      85 deg LockMaxX, and that the shoulder is a recoil bone.
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
- **UI leftovers**: PINA keyframe values (the DRB clip names and lengths are decoded in
  `sheets/ui_anim.csv` and replayed by `acvd-render::ui_kit` `UiTween`/`UiClip`); runtime image slots (emblems,
  maps, movies); the other undecoded shape classes; Dialog color tint (inferred,
  see Notes). Open: Text word +0x0c (0, 502, 1000-1004, 1030, 1200-1204; not a scale), Text flags
  byte 1 / low half (0x0204000f ...; base ctor 0x824da1d8 passes them to 0x824d9450 / 0x824d95f0).
  HUD: the lock-sight show scale
  (StaticDrawParam +0x448 = 0.53, not applied), the weapon-gauge fill formula inside `0x829d0580`,
  and hanger panels (shoulder panels, live EN, `Gauge_APEffect` damage lag/blink, and `ErrorLog_Stagger`
  are now wired; ring/arc art is tinted with the sight color since 2026-11-25; Xenia probes
  `private/xenia/en_drain{,2}.jsonl` confirmed HUD status EN
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
  `formats.csv` `.wmv`); nothing decodes them in Rust. The boot intro now uses the FFmpeg cache
  (`tools/acvd-movies` -> `private/movies/`, played by `acvd-game::movie`); a pure-Rust VC-1
  decoder is still a large job, and the WMA Pro audio could reuse `acvd-formats::xma`'s WMA Pro
  core. The in-world `o0006` `signboardGrow` screens sample this player's runtime `movie` render
  target by name, so playing a movie there also needs the decoder. Untraced: the 360 flow past
  the offline CAUTION dialog's OK (main-menu NEW GAME / LOAD GAME lives in `title.drb`, not the
  press-start `gameboot.drb`), and the front-end BGM event (the `0x82800128` `env_bgm` hit in
  `private/xenia/TASK.jsonl` was not resolved to an id; the rewrite plays menu theme `grg`).
- **Mission events** (EVD), **AI** (decompiled Lua in `private/lua360` from the ISO, with the
  `LogAI` state names; no sheet yet).


- **Map ambient combat noise** done: placement-MSB `POINT_PARAM_ST` kind-1000 points now
  start looping positional `m%08d` events from the mission `<MapSe>` project
  (`acvd-game::sound::ambient_map`, `acvd-game::map::ambient_points`, `msb::Point::ambient`,
  see Done). Remaining: m00002011 exists only in projects 109/330/331/353/361 and
  m00004404 in none, so those authored points are silent; box-shape (kind 5) points play as
  point emitters because the 360's box branch only changes the FMOD area attributes.
- **COM / operator voice follow-ups** (`acvd-game::voice`, `garage`, `sheets/voice.csv`): the
  slider walks OFF, the on-disc Type 1..5 packs (`acv2_voice_com_00..04.fev`), then any user
  packs saved from Options > Sound > `+ ADD CUSTOM COM VOICE` (modal pack name + one
  .mp3/.wav/.ogg/.flac per callout, `custom_voices.json`, paint-style unsaved-exit prompt).
  Type 6..18 remain DLC (`com_05..17`, `dlc_sound:/%s_com_%02d.fev` `0x82001f94`) and need the
  DLC mount. The callout set is the
  reliable combat subset (energy/AP thresholds, scan/combat mode, AC disabled, beginning
  operation); the mission-only lines (beacon, objective, enemy AC weight class, operation
  pattern) need the mission/AI message dispatcher. x360dis confirms the sound path:
  `0x824492c0` computes `(base + 0x493e0) * 100 + voice offset`, `0x82449518` picks the base
  from a four-entry play-obj table and the offset from the sound-map row, and the sound
  singleton hands its `+0x94cb4` table to `0x82448fb8` at `0x82ad0764`. The AI-manager path
  fills them from scene data (`0x827e8d20` reads `[this+0x320]+0x2ea70`), so they are not
  hard-coded in the wrapper. AC TEST still did not fire `0x82449518` / `0x824493d0`
  (`private/xenia/voice_en.jsonl`) or `0x82448fb8` (new `private/xenia/voice_bases.jsonl`,
  empty), so offsets 38..44 remain the FEV event order rather than a live hit; the FEV also has
  `20211` and `20301..20305` past the 44 message events (fevdump). The sound-map rows are
  `lang/en/text/ai/aisys.tsmap`, not `param/aisys.bin`. Voice currently rides the SFX
  bus; a dedicated Voice Volume slider (the 360 `@VoiceVolumeSl`/`@VoiceVolumeValue` strings,
  `0x8204c8c4`/`0x8204c8d4`) is not exposed yet. Fired callouts now draw the retail
  `MissionText` subtitle window (`acvd-game::subtitle`, `sortie.drb` dialog 44 at
  `0x82519228`): every `ComLine` resolves its offset through `voice::text_id` to
  `lang/en/text/ai/aisys.fmg` and shows the line for the FSB clip length (`Cues::com_voice_seconds`).
