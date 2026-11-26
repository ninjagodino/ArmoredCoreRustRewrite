//! Runtime skeleton: assembles ACs from the preset designs in `param/acassemblydrawing.bin`,
//! entirely from generated data plus geometry read off the owned disc.
//!
//! `acvd-game [design id] [--disc <360 ISO>] [--shot <png>] [--flat] [--yaw <degrees>]
//! [--clip <entry>] [--frame <n>] [--hold <keys>] [--wait <seconds>] [--map <id>] [--plane]
//! [--water <y>] [--cam <x,y,z> --look <x,y,z>]`
//! Piloting (see `control`): WASD move, Q/E turn, Up/Down pitch, V boost mode, Space jump,
//! Shift high boost (quick boost) / hold Shift (or K, gamepad □ + HOLD) Boost Charge Kick, Ctrl glide boost while boosting on the ground
//! (air clips once it leaves the ground);
//! F / right mouse / R2 fire the right arm weapon (guns or melee blades/piles), C / left mouse / L2 the left,
//! G / X / Tab / R1 fire the shoulder weapon / locked missile salvo (see `weapons`).
//! Z / Select / R3 toggles Scan Mode (`AC + 0xdb0`, `scan`): reduces passive EN drain for faster
//! recharge, disables weapon fire, switches the HUD lock sight to `ACV_ScanModeLockSight`, and
//! turns F / right mouse / R2 into Target Scan (populating `ACV_Scan_Cur_EnemyParamInfo` with
//! AP and KE/CE/TE defenses). B / East (○) deploys a Recon unit from the design's category-13
//! `ob` slot (`bullet/bulletrecon.bin`), marking targets inside its `effective_range`.
//! Hold R (△ / gamepad North) and press that fire button to bay-shift: the hand weapon trades
//! places with the one on the rack (see `bay`). A ready-position weapon is purged instead,
//! because it cannot sit on the bay.
//! If the design equips an Overed Weapon (`back_r`, category 11: designs `9002`, `9003`, `9004`,
//! `9999`), hold U (or hold R / △ without pressing fire for 1 s) to arm it and purge the left-arm
//! weapon, then hold F / right mouse / R2 to charge and release at full charge to fire (auto-purges
//! into EN/FCS lockout when the limit gauge drains).
//! Hand weapons support full-auto, simultaneous multi-pellet spread (shotguns/pulse guns),
//! sequential multi-round bursts (burst rifles/handguns), and hold-to-charge / release-to-fire
//! (laser rifles, rail cannons, laser cannons).
//! A ready-position weapon (cannon, autocannon, and the other classes with `ready_position`)
//! plays its deploy clip while fire is held and does not shoot until that clip ends; releasing
//! fire plays the stow, and that side's arm plays its deploy clip over the walk. A ready weapon
//! also plays its body stance until that clip ends, then fires; a sniper rifle does not. Rifles kick
//! the arm on each shot. Other weapons fire on the press and play their fire clip when they have one.
//! M toggles mouselook (mouse turns and pitches, cursor grabbed);
//! Esc / Gamepad Start opens the sortie pause menu (`pause`, `--pause` / `--pause-options` to start
//! paused: Resume, Options for fullscreen/volume, and Return to Garage in the AC TEST arena);
//! P switches to the freecam / clip browser: WASD move, Space/Ctrl ascend/descend (Shift fast),
//! drag left mouse: orbit, wheel: zoom, R: reframe; Up/Down previous/next clip, Enter pause,
//! Left/Right previous/next design. `--clip`/`--frame` start in the browser,
//! `--frame` paused on that frame. `--hold w,shift,space,f` holds keys for the whole run, and
//! `--wait` delays `--shot` that long. The default scene is the garage AC test: map `m4000`
//! (drawn and collided from the disc, see `map`) with the AC at the start point of
//! `m4000_actest.msb`. `--map` picks another `model/map` folder, `--layout <name>` another
//! `{map}_{name}.msb` start point (`none`: the terrain MSB's), `--test <mode>` picks which
//! `LAYER_PARAM_ST` target set to spawn from `{map}_{layout}.msb` (`0`/`none`/`actest1` empty,
//! `1`/`light`/`actest2` tanks & helis [default], `2`/`mt`/`actest3` MTs, `3`..`5`/`7` test ACs
//! `a0000`..`a0003`, `all`), `--hits` draws the hit meshes
//! instead of the map models, `--plane` is the old infinite floor, `--water` adds a test water
//! plane at that height. The lock-sight HUD (see `hud`) is drawn over gameplay.
//! `--no-hud` leaves that HUD and the part-name overlay out, for looking at effects.
//! Boosters, muzzle flashes, tracers and hits play FFX effects (see `sfx`); `--sfx <id>` keeps
//! effect `id` playing in front of the AC. Shots, boost and jump play their FMOD cues (see
//! `sound`). `--burst <n>` makes `--shot` save `n` frames 0.05 s apart (`<stem>_<i>.png`).
//! Boot: with no world/`--shot` args the game starts on the retail press-start splash
//! (`lang/jp/menu/gameboot.drb.dcx`, text from `lang/<lang>/text/menu/menu.fmg`, console clock in
//! the runtime `Data` text), then plays the cached opening movie (`prologue` by default, see
//! `acvd-movies cache`), then drops into the garage. Start/Space/Enter/Esc skips the movie after
//! 0.6 s. `--boot`/`--no-boot` force it on/off, `--boot-stage title|movie|done|garage` starts at
//! one stage (also implied by `--movie-frame`, which pauses the movie on that frame for a shot),
//! `--boot-auto <seconds>` auto-presses Start, `--boot-lang <en|jp|..>` changes the text bank,
//! `--movie <name>`/`--no-movie` pick or skip the movie, and `--movies-dir <dir>` points at the
//! cache (default `private/movies`). World/garage/`--map`/`--shot` args imply `--boot garage`.

mod ai;
mod assemble;
mod bay;
mod blur;
mod boost;
mod collision;
mod control;
mod damage;
mod debris;
mod decal;
mod env;
mod fcs;
mod frontend;
mod garage;
mod garage_assemble;
mod hanger;
mod hints;
mod hud;
mod input;
mod lens_flare;
mod map;
mod movie;
mod paint;
mod pause;
mod pose;
mod scan;
mod settings;
mod sfx;
mod sound;
mod targets;
mod voice;
mod weapons;

use std::path::PathBuf;

use acvd_data::generated::ac_unit::{AcAssemblyDesignSt, AC_ASSEMBLY_DESIGN_ST_FILES};
use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::Row;
use acvd_formats::accolor::AcColor;
use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::{self, Disc};
use acvd_render::app::{orbit, take_shot, Orbit, Shot};
use acvd_render::menu::MenuRoot;
use acvd_render::text::{self, Font};
use bevy::audio::{AudioSource, GlobalVolume};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use collision::{Collision, Layer, Plane, RAY_HALF};

#[derive(Resource)]
pub struct Garage {
    pub disc: Disc,
    pub designs: Vec<&'static Row<AcAssemblyDesignSt>>,
    pub current: usize,
    pub active_design: AcAssemblyDesignSt,
    pub active_name: String,
    pub custom_color: Option<AcColor>,
    pub dirty: bool,
    pub flat: bool,
    pub bounds: (Vec3, Vec3),
    /// Clip entry and paused frame to start the first design on.
    pub clip: Option<String>,
    pub frame: Option<f32>,
    pub status: String,
}

/// Marker for non-`MapPart` sortie geometry (debug collision mesh, water plane, fallback pillars).
#[derive(Component)]
struct SortieDecor;

/// Persistent TPF texture pack + FLVER model cache shared across AC rebuilds and the garage 3D
/// part preview so swapping parts never re-reads, re-decompresses, or re-untiles already-decoded
/// AC part meshes or textures from the ISO.
#[derive(Resource, Default)]
pub struct AcTexturePacks {
    pub packs: acvd_render::Packs,
    pub flvers: acvd_render::FlverCache,
}

/// The assembled AC; its parts are children.
#[derive(Component)]
struct Ac;

/// Identifies which `AC_ASSEMBLY_DESIGN_ST_SLOTS` column (`"head"`, `"core"`, `"arms"`, `"legs"`,
/// `"booster"`, `"armwep_r"`, `"armwep_l"`, `"shoulder"`, `"hanger_r"`, `"hanger_l"`, `"back_r"`)
/// a spawned AC submesh belongs to.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcPartColumn(pub &'static str);

/// When false (`--no-hud`), the lock-sight and the part-name overlay are not drawn.
#[derive(Resource)]
struct ShowHud(bool);

/// Disc font (`fontdef.xml` ID 1) and the English part-name bank.
#[derive(Resource)]
struct Hud {
    font: Font,
    parts: Fmg,
}

/// Initial camera yaw in radians (`--yaw <degrees>`).
#[derive(Resource)]
struct StartYaw(f32);

/// Optional explicit camera overrides (`--yaw` / `--pitch` in radians).
#[derive(Resource, Default, Clone, Copy)]
pub struct StartAim {
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
}

/// `--cam <x,y,z> --look <x,y,z>`: pin the main camera (Bevy axes) for inspecting effects.
#[derive(Resource)]
struct CamOverride {
    eye: Vec3,
    look: Vec3,
}

fn pin_camera(
    over: Option<Res<CamOverride>>,
    mut cams: Query<&mut Transform, (With<Camera3d>, Without<env::WaterReflectionCamera>)>,
) {
    let Some(over) = over else { return };
    if let Ok(mut cam) = cams.single_mut() {
        *cam = Transform::from_translation(over.eye).looking_at(over.look, Vec3::Y);
    }
}

/// The map whose models are drawn (`None`: draw the hit meshes or the floor plane).
#[derive(Resource)]
pub struct Scene {
    pub map: Option<String>,
    pub layout: Option<String>,
    pub test: targets::TestMode,
    pub plane: bool,
    pub hits: bool,
    pub water: Option<f32>,
    /// True when the sortie was launched from the 3D Garage AC TEST flow.
    pub from_garage: bool,
}

impl Scene {
    /// Returns true when the current sortie is the AC TEST arena (either `m4000`/`actest`
    /// or a sortie launched from the Garage AC TEST launcher).
    pub fn is_test_arena(&self) -> bool {
        self.from_garage
            || self.layout.as_deref() == Some("actest")
            || self.map.as_deref() == Some("m4000")
    }
}

fn in_sortie(mode: Res<garage::Mode>) -> bool {
    *mode == garage::Mode::Sortie
}

fn in_active_sortie(mode: Res<garage::Mode>, pause: Res<pause::PauseState>) -> bool {
    *mode == garage::Mode::Sortie && !pause.open
}

fn not_paused_in_sortie(mode: Res<garage::Mode>, pause: Res<pause::PauseState>) -> bool {
    *mode != garage::Mode::Sortie || !pause.open
}

fn frontend_active(cfg: Res<frontend::BootConfig>) -> bool {
    cfg.enabled
}

fn world_ready(world: Res<frontend::WorldReady>) -> bool {
    world.0
}

/// The deferred 3D world spawns once: after the front end releases it and before `WorldSpawned`.
#[derive(Resource, Default)]
struct WorldSpawned(bool);

fn world_needs_setup(world: Res<frontend::WorldReady>, spawned: Res<WorldSpawned>) -> bool {
    world.0 && !spawned.0
}

fn mark_world_spawned(mut spawned: ResMut<WorldSpawned>) {
    spawned.0 = true;
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut wanted, mut disc, mut shot, mut flat, mut yaw, mut pitch, mut pos_override) =
        (None, None, None, false, None, None, None::<Vec3>);
    let (mut clip, mut frame, mut held, mut hold_delay, mut wait) =
        (None, None, Vec::new(), 0.0f32, 0.0);
    let mut cam_eye: Option<Vec3> = None;
    let mut cam_look: Option<Vec3> = None;
    let (mut map, mut plane, mut water) = (Some("m4000".to_string()), false, None);
    let (mut layout, mut hits) = (Some("actest".to_string()), false);
    let mut test = targets::TestMode::default();
    let mut test_arg = "actest2".to_string();
    let (mut preview, mut burst, mut no_hud) = (None, 1, false);
    let (
        mut start_garage,
        mut garage_id,
        mut garage_screen,
        mut garage_paint_tab,
        mut garage_paint_picker_mode,
        mut cli_decal,
        mut cli_emblem,
    ) = (
        false,
        None,
        garage::GarageScreen::Top,
        garage::PaintTab::Presets,
        garage::PaintPickerMode::Hsv,
        None::<PathBuf>,
        None::<String>,
    );
    let mut start_pause: Option<pause::PauseScreen> = None;
    let mut start_input = input::InputDevice::KeyboardMouse;
    let mut open_com_voice_editor = false;
    let mut start_options_tab = settings::OptionsTab::Graphics;
    let mut shadow_dist_override: Option<settings::ShadowDistance> = None;
    let mut hanger_r_override: Option<i16> = None;
    let mut hanger_l_override: Option<i16> = None;
    let mut start_view_action: Option<garage::GarageViewAction> = None;
    let mut boot_flag: Option<bool> = None;
    let mut boot_movie = "prologue".to_string();
    let mut boot_lang = "en".to_string();
    let mut boot_stage = frontend::BootStage::Title;
    let mut movie_frame: Option<u32> = None;
    let mut skip_movie = false;
    let mut boot_auto: Option<f32> = None;
    let mut movies_dir: Option<PathBuf> = None;
    let mut world_args = false;
    while let Some(a) = args.next() {
        if matches!(
            a.as_str(),
            "--map"
                | "--layout"
                | "--test"
                | "--hits"
                | "--plane"
                | "--water"
                | "--sfx"
                | "--clip"
                | "--frame"
                | "--garage"
                | "--garage-id"
                | "--garage-screen"
                | "--view-action"
                | "--pause"
                | "--pause-options"
                | "--pause-options-sound"
                | "--cam"
                | "--pos"
                | "--hold"
                | "--hanger-r"
                | "--hanger-l"
                | "--decal"
                | "--emblem"
                | "--shadow-dist"
        ) {
            world_args = true;
        }
        match a.as_str() {
            "--boot" => boot_flag = Some(true),
            "--no-boot" => boot_flag = Some(false),
            "--movie" => boot_movie = args.next().unwrap_or(boot_movie),
            "--boot-lang" => boot_lang = args.next().unwrap_or(boot_lang),
            "--boot-stage" => {
                if let Some(raw) = args.next() {
                    boot_stage = if raw.eq_ignore_ascii_case("movie") {
                        frontend::BootStage::Movie
                    } else if raw.eq_ignore_ascii_case("done") || raw.eq_ignore_ascii_case("garage")
                    {
                        frontend::BootStage::Done
                    } else {
                        frontend::BootStage::Title
                    };
                }
            }
            "--movie-frame" => {
                movie_frame = args.next().and_then(|s| s.parse().ok());
                if movie_frame.is_some() {
                    boot_stage = frontend::BootStage::Movie;
                }
            }
            "--no-movie" => skip_movie = true,
            "--boot-auto" => boot_auto = args.next().and_then(|s| s.parse().ok()),
            "--movies-dir" => movies_dir = args.next().map(PathBuf::from),
            "--disc" => disc = args.next().map(PathBuf::from),
            "--shot" => shot = args.next().map(|p| Shot::new(PathBuf::from(p))),
            "--flat" => flat = true,
            "--yaw" => {
                yaw = args
                    .next()
                    .and_then(|d| d.parse::<f32>().ok())
                    .map(f32::to_radians)
            }
            "--pitch" => {
                pitch = args
                    .next()
                    .and_then(|d| d.parse::<f32>().ok())
                    .map(f32::to_radians)
            }
            "--pos" => {
                pos_override = args.next().and_then(|s| {
                    let v: Vec<f32> = s.split(',').filter_map(|p| p.parse().ok()).collect();
                    (v.len() == 3).then(|| Vec3::new(v[0], v[1], v[2]))
                });
            }
            "--cam" => {
                cam_eye = args.next().and_then(|s| {
                    let v: Vec<f32> = s.split(',').filter_map(|p| p.parse().ok()).collect();
                    (v.len() == 3).then(|| Vec3::new(v[0], v[1], v[2]))
                });
            }
            "--look" => {
                cam_look = args.next().and_then(|s| {
                    let v: Vec<f32> = s.split(',').filter_map(|p| p.parse().ok()).collect();
                    (v.len() == 3).then(|| Vec3::new(v[0], v[1], v[2]))
                });
            }
            "--clip" => clip = args.next(),
            "--frame" => frame = args.next().and_then(|f| f.parse::<f32>().ok()),
            "--hold" => {
                held = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(key)
                    .collect()
            }
            "--hold-delay" => {
                hold_delay = args
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0)
            }
            "--wait" => {
                wait = args
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0)
            }
            "--map" => map = args.next().filter(|s| s != "none"),
            "--layout" => layout = args.next().filter(|s| s != "none"),
            "--test" => {
                if let Some(m) = args.next() {
                    test = targets::TestMode::parse(&m);
                    test_arg = m;
                }
            }
            "--hits" => hits = true,
            "--plane" => plane = true,
            "--water" => water = args.next().and_then(|s| s.parse::<f32>().ok()),
            "--sfx" => preview = args.next().and_then(|s| s.parse::<i32>().ok()),
            "--burst" => burst = args.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1),
            "--no-hud" => no_hud = true,
            "--input" => {
                if let Some(raw) = args.next() {
                    start_input = if raw.eq_ignore_ascii_case("gamepad")
                        || raw.eq_ignore_ascii_case("pad")
                        || raw.eq_ignore_ascii_case("controller")
                    {
                        input::InputDevice::Gamepad
                    } else {
                        input::InputDevice::KeyboardMouse
                    };
                }
            }
            "--hanger-r" => {
                hanger_r_override = args.next().and_then(|s| s.parse::<i16>().ok());
            }
            "--hanger-l" => {
                hanger_l_override = args.next().and_then(|s| s.parse::<i16>().ok());
            }
            "--garage" => start_garage = true,
            "--garage-id" => {
                start_garage = true;
                garage_id = args.next().and_then(|s| s.parse::<u32>().ok());
            }
            "--garage-screen" => {
                start_garage = true;
                if let Some(raw) = args.next() {
                    if raw.trim().eq_ignore_ascii_case("decals") {
                        garage_screen = garage::GarageScreen::Paint;
                        garage_paint_tab = garage::PaintTab::Decals;
                    } else if raw.trim().eq_ignore_ascii_case("emblem") {
                        garage_screen = garage::GarageScreen::Paint;
                        garage_paint_tab = garage::PaintTab::Emblem;
                    } else if raw.trim().eq_ignore_ascii_case("palette")
                        || raw.trim().eq_ignore_ascii_case("palette-hsv")
                    {
                        garage_screen = garage::GarageScreen::Paint;
                        garage_paint_tab = garage::PaintTab::Custom;
                        garage_paint_picker_mode = garage::PaintPickerMode::Hsv;
                    } else if raw.trim().eq_ignore_ascii_case("palette-rgb")
                        || raw.trim().eq_ignore_ascii_case("palette_rgb")
                    {
                        garage_screen = garage::GarageScreen::Paint;
                        garage_paint_tab = garage::PaintTab::Custom;
                        garage_paint_picker_mode = garage::PaintPickerMode::Rgb;
                    } else if raw.trim().eq_ignore_ascii_case("options-graphics")
                        || raw.trim().eq_ignore_ascii_case("options_graphics")
                    {
                        garage_screen = garage::GarageScreen::Options;
                        start_options_tab = settings::OptionsTab::Graphics;
                    } else if raw.trim().eq_ignore_ascii_case("options-sound")
                        || raw.trim().eq_ignore_ascii_case("options_sound")
                    {
                        garage_screen = garage::GarageScreen::Options;
                        start_options_tab = settings::OptionsTab::Sound;
                    } else if let Some(s) = garage::GarageScreen::from_cli(&raw) {
                        garage_screen = s;
                    }
                }
            // Deterministic screenshot/debug entry into Options > Sound > CUSTOM COM VOICE.
            "--com-voice-editor" => {
                start_garage = true;
                garage_screen = garage::GarageScreen::Options;
                start_options_tab = settings::OptionsTab::Sound;
                open_com_voice_editor = true;
            }
            }
            "--decal" => {
                cli_decal = args.next().map(PathBuf::from);
            }
            "--view-action" => {
                if let Some(raw) = args.next() {
                    start_view_action = garage::GarageViewAction::from_cli(&raw);
                    if start_view_action.is_some() {
                        start_garage = true;
                        garage_screen = garage::GarageScreen::ViewMode;
                    }
                }
            }
            "--emblem" => {
                cli_emblem = args.next();
            }
            "--pause" => {
                start_pause = Some(pause::PauseScreen::Main);
            }
            "--pause-options" => {
                start_pause = Some(pause::PauseScreen::Options);
            }
            "--pause-options-sound" => {
                start_pause = Some(pause::PauseScreen::Options);
                start_options_tab = settings::OptionsTab::Sound;
            }
            "--shadow-dist" => {
                shadow_dist_override = args
                    .next()
                    .and_then(|s| settings::ShadowDistance::from_cli(&s));
            }
            _ => {
                wanted = a.parse::<u32>().ok();
                if wanted.is_some() {
                    world_args = true;
                }
            }
        }
    }
    let boot_enabled = boot_flag.unwrap_or(!world_args && shot.is_none());
    let initial_mode = if start_garage || boot_enabled {
        garage::Mode::Garage
    } else {
        garage::Mode::Sortie
    };
    let cam_override = cam_eye.map(|eye| CamOverride {
        eye,
        look: cam_look.unwrap_or(eye + Vec3::X * 100.0),
    });
    let piloting = !start_garage
        && !boot_enabled
        && clip.is_none()
        && frame.is_none()
        && cam_override.is_none();
    // Piloting starts behind the AC, the browser in front of it.
    let start_aim = StartAim { yaw, pitch };
    let yaw = yaw.unwrap_or(if piloting { 0.25 } else { 2.6 });
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let disc =
        Disc::open(&disc.unwrap_or_else(|| vfs::default_disc(&root))).expect("opening the disc");
    let (collision, mut start, area_bounds, map_nav, water_return) =
        if initial_mode == garage::Mode::Sortie {
            load_sortie_data(&disc, map.as_deref(), layout.as_deref(), plane, water)
        } else {
            (
                Collision::with_hits(
                    vec![Plane {
                        y: 0.0,
                        layer: Layer::Ground,
                    }],
                    Vec::new(),
                ),
                map::Start {
                    position: Vec3::ZERO,
                    yaw: 0.0,
                },
                map::AreaBounds::default(),
                map::MapNav::default(),
                map::WaterReturnPoints {
                    points: Vec::new(),
                    water_height: water,
                },
            )
        };
    if let Some(p) = pos_override {
        start.position = p;
    }
    let designs: Vec<&'static Row<AcAssemblyDesignSt>> = AC_ASSEMBLY_DESIGN_ST_FILES
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .filter(|r| {
            AC_ASSEMBLY_DESIGN_ST_SLOTS
                .iter()
                .any(|s| (s.part)(&r.data) > 0)
                && AcCtrlParam::buildable(&r.data)
        })
        .collect();
    let current = wanted
        .and_then(|id| designs.iter().position(|r| r.id == id))
        .unwrap_or_else(|| {
            if initial_mode == garage::Mode::Garage {
                // Default the 3D Garage to a clean, non-overweight, non-weapon-arm showcase AC
                // (preferring 6007 ARENA ATK-6 or 6008 ARENA ATK-2 if available).
                for pref in [6007u32, 6008, 6004, 6003] {
                    if let Some(pos) = designs.iter().position(|r| r.id == pref) {
                        let s = garage::AcSpecs::calculate(&designs[pos].data);
                        if !s.overweight
                            && !s.en_shortage
                            && !garage::has_weapon_arms(&designs[pos].data)
                        {
                            return pos;
                        }
                    }
                }
                designs
                    .iter()
                    .position(|r| {
                        let s = garage::AcSpecs::calculate(&r.data);
                        !s.overweight && !s.en_shortage && !garage::has_weapon_arms(&r.data)
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        });
    if let Some(id) = wanted.filter(|&id| designs.get(current).is_none_or(|r| r.id != id)) {
        eprintln!("no design {id}; starting at the first");
    }
    let initial_row = designs[current];
    let mut initial_design = initial_row.data;
    if let Some(id) = hanger_r_override {
        initial_design.hanger_r = id;
    }
    if let Some(id) = hanger_l_override {
        initial_design.hanger_l = id;
    }
    let initial_name = garage::display_design_name(initial_row.id, initial_row.name);
    let initial_color = (!flat)
        .then(|| paint::load_accolor(&disc, initial_design.coloring_id))
        .flatten();
    let mut garage_state = garage::GarageState::init(
        &disc,
        garage_id,
        garage_screen,
        map.as_deref().unwrap_or("m4000"),
        &test_arg,
    );
    garage_state.paint_tab = garage_paint_tab;
    garage_state.paint_picker_mode = garage_paint_picker_mode;
    if open_com_voice_editor {
        garage_state.com_voice_editor = Some(garage::ComVoiceEditor::new(
            voice::VoiceLibrary::load().next_name(),
        ));
    }
    garage_state.options_tab = start_options_tab;
    if shot.is_none() {
        garage_state.view_anim.pending = start_view_action;
    }

    let enemy_parts = weapons::EnemyParts::load(&disc);
    let mut pause_state = pause::PauseState::default();
    if initial_mode == garage::Mode::Sortie {
        if let Some(screen) = start_pause {
            pause_state.open_menu(screen);
            pause_state.options_tab = start_options_tab;
        }
    }
    let mut game_settings = settings::GameSettings::load();
    if let Some(sd) = shadow_dist_override {
        game_settings.shadow_distance = sd;
    }
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "acvd-game".into(),
            mode: if game_settings.fullscreen {
                bevy::window::WindowMode::BorderlessFullscreen(
                    bevy::window::MonitorSelection::Current,
                )
            } else {
                bevy::window::WindowMode::Windowed
            },
            ..default()
        }),
        ..default()
    }))
    .add_plugins((
        blur::BlurPlugin,
        paint::PaintPlugin,
        acvd_render::menu::MenuPlugin,
        frontend::FrontendPlugin,
    ))
    .add_plugins(sfx::SfxPlugin { disc: disc.clone() })
    .add_plugins(sound::SoundPlugin {
        disc: disc.clone(),
        map: if initial_mode == garage::Mode::Garage {
            None
        } else {
            map.clone().filter(|_| !plane && !hits)
        },
        play_bgm: shot.is_none() && !boot_enabled,
    })
    .add_plugins(voice::ComVoicePlugin)
    .add_plugins(env::EnvPlugin { disc: disc.clone() })
    .add_plugins(lens_flare::LensFlarePlugin { disc: disc.clone() })
    .insert_resource(ClearColor(Color::srgb(0.32, 0.36, 0.42)))
    .insert_resource(GlobalVolume::new(bevy::audio::Volume::Linear(
        game_settings.volume.clamp(0.0, 1.0),
    )))
    .insert_resource(game_settings)
    .insert_resource(enemy_parts)
    .insert_resource(initial_mode)
    .insert_resource(garage::ModeSwitch::default())
    .insert_resource(garage_state)
    .insert_resource(frontend::WorldReady(!boot_enabled))
    .insert_resource(WorldSpawned(!boot_enabled))
    .insert_resource(frontend::BootConfig {
        enabled: boot_enabled,
        disc: disc.clone(),
        lang: boot_lang,
        movie: boot_movie,
        movies_dir: movies_dir.unwrap_or_else(|| root.join("private/movies")),
        start: boot_stage,
        frame: movie_frame,
        skip_movie,
        auto_start: boot_auto,
    })
    .insert_resource(pause_state)
    .init_resource::<AcTexturePacks>()
    .init_resource::<garage_assemble::AssembleUiAssets>()
    .init_resource::<garage::PaintPickerAssets>()
    .init_resource::<paint::CustomCamoLibrary>()
    .insert_resource(decal::DecalState::new(cli_decal, cli_emblem))
    .insert_resource(decal::DecalMeshCache::default())
    .insert_resource(Garage {
        disc,
        designs,
        current,
        active_design: initial_design,
        active_name: initial_name,
        custom_color: initial_color,
        dirty: true,
        flat,
        bounds: (Vec3::ZERO, Vec3::ONE),
        clip,
        frame,
        status: String::new(),
    })
    .insert_resource(StartYaw(yaw))
    .insert_resource(start_aim)
    .insert_resource(start)
    .insert_resource(area_bounds)
    .insert_resource(map_nav)
    .insert_resource(water_return)
    .insert_resource(Scene {
        map: map.filter(|_| !plane && !hits),
        layout: layout.filter(|_| !plane),
        test,
        plane,
        hits,
        water,
        from_garage: false,
    })
    .insert_resource(collision)
    .insert_resource(control::Piloting(piloting))
    .insert_resource(control::Held(if hold_delay > 0.0 {
        Vec::new()
    } else {
        held.clone()
    }))
    .insert_resource(PendingHold {
        delay: hold_delay,
        keys: if hold_delay > 0.0 { held } else { Vec::new() },
    })
    .insert_resource(ShowHud(!no_hud))
    .insert_resource(input::ActiveInput(start_input))
    .add_systems(
        Startup,
        (setup, weapons::setup).run_if(not(frontend_active)),
    )
    .add_systems(
        PreUpdate,
        (setup, weapons::setup, mark_world_spawned)
            .chain()
            .run_if(world_needs_setup),
    )
    .add_systems(
        Update,
        (
            input::track_device,
            sortie_garage_hotkey,
            pause::handle_input,
            process_mode_switch,
            pause::sync_ui,
            garage::sync_room,
            decal::handle_imports,
            garage::handle_input,
            garage::handle_paint_color_drag,
            garage::handle_slider_track_drag,
            garage::sync_paint_history,
            garage::sync_ui,
            garage::update_custom_thumb_delete_hover,
            garage_assemble::populate_drb_dialogs,
            garage_assemble::sync_part_preview,
            garage_assemble::rotate_part_preview,
            garage_assemble::sync_ac_preview,
            garage_assemble::rotate_ac_preview,
            garage::update_camera,
            settings::sync_shadow_distance,
        )
            .chain()
            .before(show)
            .run_if(world_ready),
    )
    .add_systems(
        Update,
        (
            (
                browse.run_if(in_active_sortie),
                show,
                garage_assemble::sync_part_hologram,
                clips.run_if(in_active_sortie),
                control::pilot.run_if(in_active_sortie),
                scan::update.run_if(in_active_sortie),
                fcs::update.run_if(in_active_sortie),
                map::lod.run_if(in_sortie),
                pose::animate.run_if(not_paused_in_sortie),
                boost::pose.run_if(not_paused_in_sortie),
                boost::hide.run_if(not_paused_in_sortie),
                hanger::pose.run_if(not_paused_in_sortie),
            )
                .chain(),
            (
                decal::interact_and_gizmo,
                decal::sync_bakes,
                garage::step_view_actions,
                garage::step_view_bay_shifts,
            )
                .chain()
                .after(hanger::pose),
            (
                tick_pending_hold.run_if(in_active_sortie),
                bay::shift.run_if(in_active_sortie),
                bay::probe_shot.run_if(in_active_sortie),
                weapons::fire.run_if(in_active_sortie),
                weapons::probe_shot.run_if(in_active_sortie),
                weapons::step_overed_weapon.run_if(in_active_sortie),
                weapons::step_melee.run_if(in_active_sortie),
                ai::update.run_if(in_active_sortie),
                targets::animate_enemies.run_if(in_active_sortie),
                pose::animate_npcs.run_if(in_active_sortie),
                weapons::step_projectiles.run_if(in_active_sortie),
                weapons::step_missiles.run_if(in_active_sortie),
                weapons::tick_player_health.run_if(in_active_sortie),
                targets::tick.run_if(in_active_sortie),
                map::step_destructibles.run_if(in_active_sortie),
                debris::step.run_if(in_active_sortie),
                debris::probe_shot.run_if(in_sortie),
                orbit.run_if(in_active_sortie),
                reframe.run_if(in_active_sortie),
                pin_camera.run_if(in_active_sortie),
            )
                .chain(),
        )
            .chain()
            .run_if(world_ready),
    )
    .add_systems(
        Update,
        (
            hud::layout.run_if(in_sortie),
            (
                hud::tick,
                hud::track_lock_sight,
                hud::readouts,
                hud::panels,
                hud::lock_cursor,
                hud::scan_hud,
                hud::recon_hud,
                hud::stagger_hud,
                hud::ow_hud,
                hud::area_warning_hud,
            )
                .chain()
                .run_if(in_active_sortie),
        )
            .chain()
            .after(reframe)
            .run_if(world_ready),
    );
    if let Some(id) = preview {
        app.insert_resource(sfx::Preview(id));
    }
    if let Some(mut shot) = shot {
        shot.burst = burst;
        app.insert_resource(ShotDelay(wait, false, 0.0, start_view_action))
            .insert_resource(shot)
            .insert_resource(acvd_render::app::ShotMark::default())
            .init_resource::<FramePerf>()
            .add_systems(
                Update,
                (sample_frame_perf, delay_shot, take_shot, dump_frame_perf)
                    .chain()
                    .after(pose::animate),
            );
    }
    if let Some(o) = cam_override {
        app.insert_resource(o);
    }
    app.run();
}

#[derive(Resource)]
struct PendingHold {
    delay: f32,
    keys: Vec<KeyCode>,
}

fn tick_pending_hold(
    time: Res<Time>,
    mut pending: ResMut<PendingHold>,
    mut held: ResMut<control::Held>,
) {
    if pending.keys.is_empty() {
        return;
    }
    pending.delay -= time.delta_secs();
    if pending.delay <= 0.0 {
        held.0.append(&mut pending.keys);
    }
}

/// Seconds `--shot` still waits after the AC is shown (`--wait`), and whether it was shown.
#[derive(Resource)]
struct ShotDelay(f32, bool, f32, Option<garage::GarageViewAction>);

/// Rolling frame-time samples for `--shot` PERF dumps (ms).
#[derive(Resource, Default)]
struct FramePerf {
    samples: Vec<f32>,
    warmed: bool,
}

fn delay_shot(
    time: Res<Time>,
    mut delay: ResMut<ShotDelay>,
    mut shot: ResMut<Shot>,
    mut garage_state: ResMut<garage::GarageState>,
) {
    delay.1 |= shot.ready;
    if delay.1 && delay.0 > 0.0 {
        delay.0 -= time.delta_secs();
        shot.ready = delay.0 <= 0.0;
    }
    if shot.ready {
        delay.2 += time.delta_secs();
        if delay.2 >= 2.2 {
            if let Some(action) = delay.3.take() {
                garage_state.view_anim.pending = Some(action);
                garage_state.ui_dirty = true;
            }
        }
    }
}

fn sample_frame_perf(time: Res<Time>, mut perf: ResMut<FramePerf>, delay: Option<Res<ShotDelay>>) {
    let dt_ms = time.delta_secs() * 1000.0;
    // Skip the first couple of frames (shader compile / asset upload spikes).
    if !perf.warmed {
        if delay.is_some_and(|d| d.1) && dt_ms < 100.0 {
            perf.warmed = true;
        }
        return;
    }
    if dt_ms > 0.0 && dt_ms < 500.0 {
        perf.samples.push(dt_ms);
    }
}

fn dump_frame_perf(
    mark: Option<Res<acvd_render::app::ShotMark>>,
    perf: Res<FramePerf>,
    collision: Res<Collision>,
    meshes: Query<(), With<Mesh3d>>,
    map_parts: Query<(), With<map::MapPart>>,
    water_cams: Query<(), With<env::WaterReflectionCamera>>,
    targets: Query<(), With<targets::Target>>,
    sfx_q: Query<(), With<sfx::Sfx>>,
    mut dumped: Local<bool>,
) {
    let Some(mark) = mark else { return };
    if mark.path.is_none() || *dumped {
        return;
    }
    *dumped = true;
    let mut samples = perf.samples.clone();
    if samples.is_empty() {
        eprintln!("PERF no frame samples yet");
        return;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    let sum: f32 = samples.iter().sum();
    let avg = sum / n as f32;
    let p50 = samples[n / 2];
    let p95 = samples[((n as f32) * 0.95) as usize].min(samples[n - 1]);
    let max = samples[n - 1];
    eprintln!(
        "PERF frames={n} avg={avg:.1}ms ({:.0} fps) p50={p50:.1}ms p95={p95:.1}ms max={max:.1}ms \
         hits={} mesh3d={} map_parts={} water_cams={} targets={} sfx={}",
        1000.0 / avg.max(0.001),
        collision.hits.len(),
        meshes.iter().count(),
        map_parts.iter().count(),
        water_cams.iter().count(),
        targets.iter().count(),
        sfx_q.iter().count(),
    );
}

fn key(name: &str) -> Option<KeyCode> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "w" => KeyCode::KeyW,
        "a" => KeyCode::KeyA,
        "s" => KeyCode::KeyS,
        "d" => KeyCode::KeyD,
        "q" => KeyCode::KeyQ,
        "e" => KeyCode::KeyE,
        "shift" => KeyCode::ShiftLeft,
        "space" => KeyCode::Space,
        "ctrl" => KeyCode::ControlLeft,
        "v" => KeyCode::KeyV,
        "k" => KeyCode::KeyK,
        "f" | "fire" => KeyCode::KeyF,
        "c" => KeyCode::KeyC,
        "g" | "shoulder" => KeyCode::KeyG,
        "x" => KeyCode::KeyX,
        "tab" => KeyCode::Tab,
        "r" => KeyCode::KeyR,
        "u" | "ow" => KeyCode::KeyU,
        "z" | "scan" => KeyCode::KeyZ,
        "b" | "recon" => KeyCode::KeyB,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        other => {
            eprintln!("--hold: unknown key `{other}`");
            return None;
        }
    })
}

fn load_sortie_data(
    disc: &Disc,
    map: Option<&str>,
    layout: Option<&str>,
    plane: bool,
    water: Option<f32>,
) -> (
    Collision,
    map::Start,
    map::AreaBounds,
    map::MapNav,
    map::WaterReturnPoints,
) {
    let mut collision = if plane {
        Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        )
    } else if let Some(id) = map {
        match collision::load_map(disc, id) {
            Ok(c) => {
                eprintln!("map {id}: {} hit triangles", c.hits.len());
                c
            }
            Err(e) => {
                eprintln!("map {id}: {e:#}; using a flat plane");
                Collision::with_hits(
                    vec![Plane {
                        y: 0.0,
                        layer: Layer::Ground,
                    }],
                    Vec::new(),
                )
            }
        }
    } else {
        Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        )
    };
    let start = match map.filter(|_| !plane) {
        Some(id) => map::start(disc, id, layout).unwrap_or_else(|e| {
            eprintln!("map {id} start point: {e:#}");
            None
        }),
        None => None,
    }
    .unwrap_or(map::Start {
        position: Vec3::ZERO,
        yaw: 0.0,
    });
    let area_bounds = match map.filter(|_| !plane) {
        Some(id) => map::AreaBounds::load(disc, id, layout).unwrap_or_else(|e| {
            eprintln!("map {id} area bounds: {e:#}");
            map::AreaBounds::default()
        }),
        None => map::AreaBounds::default(),
    };
    let map_nav = match map.filter(|_| !plane) {
        Some(id) => match map::MapNav::load(disc, id) {
            Ok(n) => {
                if n.is_loaded() {
                    eprintln!("map {id}: {} navmesh regions", n.region_count());
                }
                n
            }
            Err(e) => {
                eprintln!("map {id} navmesh: {e:#}");
                map::MapNav::default()
            }
        },
        None => map::MapNav::default(),
    };
    let water_return = match map.filter(|_| !plane) {
        Some(id) => match map::WaterReturnPoints::load(disc, id, layout, water) {
            Ok(wr) => {
                eprintln!(
                    "map {id}: {} water-return points (kind 50), water_height={:?}",
                    wr.points.len(),
                    wr.water_height
                );
                wr
            }
            Err(e) => {
                eprintln!("{id} water return: {e:#}");
                map::WaterReturnPoints {
                    points: Vec::new(),
                    water_height: water,
                }
            }
        },
        None => map::WaterReturnPoints {
            points: Vec::new(),
            water_height: water,
        },
    };
    if let Some(y) = water {
        collision.planes.push(Plane {
            y,
            layer: Layer::Water,
        });
    }
    (collision, start, area_bounds, map_nav, water_return)
}

#[allow(clippy::too_many_arguments)]
fn spawn_sortie_world(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    paint_materials: &mut Assets<paint::AcPaintMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    garage: &Garage,
    enemy_parts: &weapons::EnemyParts,
    collision: &Collision,
    scene: &Scene,
) {
    let drawn = scene.map.as_deref().map_or(0, |id| {
        match map::spawn(commands, meshes, materials, images, &garage.disc, id) {
            Ok(n) => {
                info!("map {id}: {n} parts drawn");
                n
            }
            Err(e) => {
                warn!("map {id}: {e:#}");
                0
            }
        }
    });
    if let (Some(map_id), Some(layout)) = (scene.map.as_deref(), scene.layout.as_deref()) {
        if let Err(e) = targets::spawn(
            commands,
            meshes,
            materials,
            paint_materials,
            images,
            bindposes,
            &garage.disc,
            enemy_parts,
            map_id,
            layout,
            &scene.test,
            garage.flat,
        ) {
            warn!("targets {map_id}_{layout}: {e:#}");
        }
    }
    if drawn == 0 && collision.hits.is_empty() {
        commands.spawn((
            SortieDecor,
            Mesh3d(meshes.add(Plane3d::default().mesh().size(FLOOR, FLOOR))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.22, 0.23, 0.25),
                perceptual_roughness: 0.95,
                ..default()
            })),
        ));
    } else if drawn == 0 {
        for (layer, color) in [
            (Layer::Ground, Color::srgb(0.22, 0.23, 0.25)),
            (Layer::Water, Color::srgba(0.15, 0.35, 0.6, 0.5)),
        ] {
            let Some(mesh) = debug_mesh(collision.hits.iter().filter(|h| h.layer == layer)) else {
                continue;
            };
            let mut mat = StandardMaterial {
                base_color: color,
                perceptual_roughness: 0.95,
                ..default()
            };
            if layer == Layer::Water {
                mat.alpha_mode = AlphaMode::Blend;
            }
            commands.spawn((
                SortieDecor,
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(materials.add(mat)),
            ));
        }
    }
    let water = materials.add(StandardMaterial {
        base_color: Color::srgba(0.15, 0.35, 0.6, 0.5),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    for p in collision.planes.iter().filter(|p| p.layer == Layer::Water) {
        commands.spawn((
            SortieDecor,
            Mesh3d(meshes.add(Plane3d::default().mesh().size(FLOOR, FLOOR))),
            MeshMaterial3d(water.clone()),
            Transform::from_xyz(0.0, p.y, 0.0),
        ));
    }
    if collision.hits.is_empty() {
        let (pillar, grey) = (
            meshes.add(Cuboid::new(1.0, 6.0, 1.0)),
            materials.add(Color::srgb(0.45, 0.47, 0.5)),
        );
        let n = (FLOOR / 2.0 / PILLAR_SPACING) as i32;
        for x in -n..=n {
            for z in -n..=n {
                if (x, z) != (0, 0) {
                    commands.spawn((
                        SortieDecor,
                        Mesh3d(pillar.clone()),
                        MeshMaterial3d(grey.clone()),
                        Transform::from_xyz(
                            x as f32 * PILLAR_SPACING,
                            3.0,
                            z as f32 * PILLAR_SPACING,
                        ),
                    ));
                }
            }
        }
    }
}

/// Press `F1` during a sortie to return straight to the 3D Garage with the current AC.
fn sortie_garage_hotkey(
    mode: Res<garage::Mode>,
    keys: Res<ButtonInput<KeyCode>>,
    mut pause: ResMut<pause::PauseState>,
    mut switch: ResMut<garage::ModeSwitch>,
) {
    if *mode == garage::Mode::Sortie && keys.just_pressed(KeyCode::F1) {
        pause.close();
        switch.target = Some(garage::Mode::Garage);
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct RenderAssets<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    paint_materials: ResMut<'w, Assets<paint::AcPaintMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
    ac_packs: ResMut<'w, AcTexturePacks>,
    custom_camos: Res<'w, paint::CustomCamoLibrary>,
}

#[derive(bevy::ecs::system::SystemParam)]
struct SortieResources<'w> {
    scene: ResMut<'w, Scene>,
    collision: ResMut<'w, Collision>,
    start: ResMut<'w, map::Start>,
    area_bounds: ResMut<'w, map::AreaBounds>,
    map_nav: ResMut<'w, map::MapNav>,
    water_return: ResMut<'w, map::WaterReturnPoints>,
    piloting: ResMut<'w, control::Piloting>,
    enemy_parts: Res<'w, weapons::EnemyParts>,
    sortie_hud: Option<ResMut<'w, hud::Sortie>>,
}

#[derive(bevy::ecs::system::SystemParam)]
struct AudioCtx<'w, 's> {
    cues: ResMut<'w, sound::Cues>,
    bgm: ResMut<'w, sound::BgmState>,
    bgm_players: Query<'w, 's, Entity, With<sound::BgmPlayer>>,
    audio_assets: ResMut<'w, Assets<AudioSource>>,
}

/// Executes queued transitions between the 3D Garage and live AC TEST sorties.
#[allow(clippy::too_many_arguments)]
fn process_mode_switch(
    mut commands: Commands,
    mut mode: ResMut<garage::Mode>,
    mut switch: ResMut<garage::ModeSwitch>,
    mut pause_state: ResMut<pause::PauseState>,
    mut garage_state: ResMut<garage::GarageState>,
    mut garage: ResMut<Garage>,
    mut sortie: SortieResources,
    mut audio: AudioCtx,
    mut assets: RenderAssets,
    mut dir_lights: Query<(&mut DirectionalLight, &mut Transform)>,
    mut cursor_opts: Query<&mut bevy::window::CursorOptions, With<bevy::window::PrimaryWindow>>,
    mut cam_proj: Query<
        &mut Projection,
        (
            With<Camera3d>,
            Without<env::WaterReflectionCamera>,
            Without<garage_assemble::PartPreviewCamera>,
        ),
    >,
    cameras: Query<
        Entity,
        (
            With<Camera3d>,
            Without<env::WaterReflectionCamera>,
            Without<garage_assemble::PartPreviewCamera>,
        ),
    >,
    sortie_entities: Query<
        Entity,
        Or<(
            With<map::MapPart>,
            With<SortieDecor>,
            With<targets::Target>,
            With<weapons::Projectile>,
            With<weapons::Missile>,
            With<scan::ReconUnit>,
            With<MenuRoot>,
            With<hud::AreaWarningHud>,
            With<pause::PauseUi>,
            With<text::Label>,
        )>,
    >,
    garage_entities: Query<Entity, Or<(With<garage::GarageWorld>, With<garage::GarageUi>)>>,
) {
    let Some(target) = switch.target.take() else {
        return;
    };
    if *mode == target {
        return;
    }
    pause_state.close();
    match target {
        garage::Mode::Garage => {
            for e in &sortie_entities {
                commands.entity(e).despawn();
            }
            if let Ok(mut c) = cursor_opts.single_mut() {
                c.grab_mode = bevy::window::CursorGrabMode::None;
                c.visible = true;
            }
            env::clear_map_env(&mut commands);
            sortie.piloting.0 = false;
            *mode = garage::Mode::Garage;
            garage_state.screen = garage::GarageScreen::Top;
            garage_state.cursor = 0;
            garage_state.scroll = 0;
            garage_state.spawned_workshop = None;
            garage_state.cam_initialized = false;
            garage_state.cam_manual_orbit = false;
            garage_state.ui_dirty = true;
            garage.dirty = true;
            sound::switch_bgm(
                &mut commands,
                &mut audio.bgm,
                &audio.bgm_players,
                &garage.disc,
                None,
            );
            sound::cue(
                &mut commands,
                &mut audio.cues,
                &mut audio.audio_assets,
                garage::CUE_GARAGE_IN,
            );
        }
        garage::Mode::Sortie => {
            for e in &garage_entities {
                commands.entity(e).despawn();
            }
            garage_state.spawned_workshop = None;
            sortie.scene.from_garage = true;
            if let Some(m) = switch.sortie_map.take() {
                sortie.scene.map = Some(m);
            } else if sortie.scene.map.is_none() && !sortie.scene.plane && !sortie.scene.hits {
                sortie.scene.map = Some("m4000".to_string());
            }
            if let Some(t) = switch.sortie_test.take() {
                sortie.scene.test = targets::TestMode::parse(&t);
            }
            if sortie.scene.map.as_deref() == Some("m4000") {
                sortie.scene.layout = Some("actest".to_string());
            } else {
                sortie.scene.layout = None;
            }
            let (new_col, new_start, new_bounds, new_nav, new_wr) = load_sortie_data(
                &garage.disc,
                sortie.scene.map.as_deref(),
                sortie.scene.layout.as_deref(),
                sortie.scene.plane,
                sortie.scene.water,
            );
            *sortie.collision = new_col;
            *sortie.start = new_start;
            *sortie.area_bounds = new_bounds;
            *sortie.map_nav = new_nav;
            *sortie.water_return = new_wr;

            spawn_sortie_world(
                &mut commands,
                &mut assets.meshes,
                &mut assets.materials,
                &mut assets.paint_materials,
                &mut assets.images,
                &mut assets.bindposes,
                &garage,
                &sortie.enemy_parts,
                &sortie.collision,
                &sortie.scene,
            );
            if let Some(map_id) = sortie.scene.map.as_deref() {
                env::load_map_env(
                    &mut commands,
                    &garage.disc,
                    map_id,
                    &mut dir_lights,
                    &cameras,
                    &mut assets.images,
                );
            }
            if let Ok(mut proj) = cam_proj.single_mut() {
                if let Projection::Perspective(ref mut p) = *proj {
                    p.fov = PerspectiveProjection::default().fov;
                }
            }
            if let Some(ref mut sh) = sortie.sortie_hud {
                sh.reset_layout();
            }
            sortie.piloting.0 = true;
            *mode = garage::Mode::Sortie;
            garage.dirty = true;
            sound::switch_bgm(
                &mut commands,
                &mut audio.bgm,
                &audio.bgm_players,
                &garage.disc,
                sortie.scene.map.as_deref(),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut paint_materials: ResMut<Assets<paint::AcPaintMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut decals: ResMut<decal::DecalState>,
    mode: Res<garage::Mode>,
    garage: Res<Garage>,
    enemy_parts: Res<weapons::EnemyParts>,
    yaw: Res<StartYaw>,
    collision: Res<Collision>,
    scene: Res<Scene>,
    show_hud: Res<ShowHud>,
    settings: Res<settings::GameSettings>,
) {
    commands.spawn((
        Camera3d::default(),
        RenderLayers::from_layers(&[0, env::WATER_RENDER_LAYER]),
        Projection::Perspective(PerspectiveProjection {
            far: VIEW_FAR,
            ..default()
        }),
        Transform::default(),
        Orbit::new(yaw.0, 0.25),
        blur::ZoomBlur::default(),
    ));
    commands.spawn((
        settings::PrimaryShadowLight,
        DirectionalLight {
            illuminance: 9000.0,
            shadow_maps_enabled: settings.shadow_distance.shadows_enabled(),
            ..default()
        },
        settings.shadow_distance.cascade_config(),
        Transform::from_xyz(4.0, 10.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 2500.0,
            ..default()
        },
        Transform::from_xyz(-6.0, 3.0, -4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    if *mode == garage::Mode::Sortie {
        spawn_sortie_world(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut paint_materials,
            &mut images,
            &mut bindposes,
            &garage,
            &enemy_parts,
            &collision,
            &scene,
        );
    }
    let show_hud_flag = show_hud.0;
    decals.init_assets(&mut images);
    if show_hud_flag {
        match load_hud(&garage.disc, &mut images) {
            Ok(hud) => commands.insert_resource(hud),
            Err(e) => warn!("hud: {e:#}"),
        }
        match hud::load(&garage.disc, &mut images) {
            Ok(sortie) => commands.insert_resource(sortie),
            Err(e) => warn!("sortie hud: {e:#}"),
        }
    }
}

fn load_hud(disc: &Disc, images: &mut Assets<Image>) -> anyhow::Result<Hud> {
    Ok(Hud {
        font: text::load(disc, "e1_ext", images)?,
        parts: acvd_formats::fmg::read(&disc.asset("lang/en/text/partsname_en.fmg")?)?,
    })
}

fn debug_mesh<'a>(hits: impl Iterator<Item = &'a collision::Hit>) -> Option<Mesh> {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    for h in hits {
        let i = positions.len() as u32;
        positions.extend([h.a, h.b, h.c].map(|p| [p.x, p.y, p.z]));
        indices.extend([i, i + 1, i + 2]);
    }
    if positions.is_empty() {
        return None;
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    mesh.compute_smooth_normals();
    Some(mesh)
}

const FLOOR: f32 = 1000.0;
/// Camera far plane in metres; m4000's MSB parts span about 1.6 km. Not game data.
const VIEW_FAR: f32 = 6000.0;
const PILLAR_SPACING: f32 = 40.0;

fn reframe(
    keys: Res<ButtonInput<KeyCode>>,
    piloting: Res<control::Piloting>,
    garage: Res<Garage>,
    ac: Query<&Transform, With<Ac>>,
    mut orbit: Query<&mut Orbit>,
) {
    if piloting.0 || !keys.just_pressed(KeyCode::KeyR) {
        return;
    }
    if let Ok(mut o) = orbit.single_mut() {
        let pos = ac.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
        o.frame(pos + garage.bounds.0, pos + garage.bounds.1);
    }
}

fn browse(keys: Res<ButtonInput<KeyCode>>, mut garage: ResMut<Garage>) {
    let n = garage.designs.len() as isize;
    let step = [(KeyCode::ArrowRight, 1), (KeyCode::ArrowLeft, -1)]
        .iter()
        .filter(|(k, _)| keys.just_pressed(*k))
        .map(|(_, s)| *s)
        .sum::<isize>();
    if step != 0 && n > 0 {
        let next = (garage.current as isize + step).rem_euclid(n) as usize;
        garage.current = next;
        if let Some(&row) = garage.designs.get(next) {
            garage.active_design = row.data;
            garage.active_name = garage::display_design_name(row.id, row.name);
            garage.custom_color = (!garage.flat)
                .then(|| paint::load_accolor(&garage.disc, row.data.coloring_id))
                .flatten();
            garage.dirty = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn show(
    mut commands: Commands,
    mode: Res<garage::Mode>,
    mut garage: ResMut<Garage>,
    acs: Query<Entity, With<Ac>>,
    shots: Query<
        Entity,
        Or<(
            With<weapons::Projectile>,
            With<weapons::Missile>,
            With<scan::ReconUnit>,
        )>,
    >,
    mut assets: RenderAssets,
    mut orbit: Query<&mut Orbit>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    labels: Query<Entity, With<text::Label>>,
    hud: Option<Res<Hud>>,
    collision: Res<Collision>,
    start: Res<map::Start>,
    start_aim: Res<StartAim>,
    shot: Option<ResMut<Shot>>,
    mut decals: ResMut<decal::DecalState>,
    mut decal_cache: ResMut<decal::DecalMeshCache>,
) {
    let meshes = &mut *assets.meshes;
    let materials = &mut *assets.materials;
    let paint_materials = &mut *assets.paint_materials;
    let images = &mut *assets.images;
    let bindposes = &mut *assets.bindposes;
    if !garage.dirty {
        return;
    }
    garage.dirty = false;
    decals.init_assets(images);
    decal_cache.meshes.clear();
    decal_cache.shoulder_emblem = None;
    let mut l_kata_rest: Option<Mat4> = None;
    let design_data = garage.active_design;
    let design_name = garage.active_name.clone();
    for e in acs.iter().chain(shots.iter()).chain(labels.iter()) {
        commands.entity(e).despawn();
    }
    commands.remove_resource::<weapons::WeaponAnims>();
    commands.remove_resource::<weapons::Shift>();
    commands.remove_resource::<hanger::HangerPose>();
    commands.remove_resource::<boost::BoostPose>();

    let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design_data);
    let accolor = if garage.flat {
        None
    } else {
        garage
            .custom_color
            .or_else(|| paint::load_accolor(&garage.disc, design_data.coloring_id))
    };
    let (spawn_pos, spawn_yaw, spawn_y) = if *mode == garage::Mode::Garage {
        // Garage workshops (`menu0300`..`menu1080`) place their emblem banner / backdrop at `-Z`
        // and open bay toward `+Z`, so rotate the AC 180° to face `+Z` (away from the backdrop).
        (Vec3::ZERO, std::f32::consts::PI, Some(0.0))
    } else {
        let y = collision.ground_below(start.position + Vec3::Y * RAY_HALF);
        (
            start.position.with_y(y.unwrap_or(start.position.y)),
            start.yaw,
            y,
        )
    };
    let ac = commands
        .spawn((
            Ac,
            Transform::from_translation(spawn_pos).with_rotation(Quat::from_rotation_y(spawn_yaw)),
            Visibility::default(),
        ))
        .id();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let ac_cache = &mut *assets.ac_packs;
    let (packs, flvers, mut missing) = (&mut ac_cache.packs, &mut ac_cache.flvers, 0);
    let mut problems = built.problems;
    let mut loaded = Vec::new();
    let mut known = vec![None; built.placements.len()];
    for (index, p) in built.placements.iter().enumerate() {
        let places = assemble::root_places(p, &built.placements, &known);
        debug!("{} {}", p.column, p.model.path);
        match flvers.rigged_model(&garage.disc, p.model.path, &|root| {
            assemble::place_of(&places, root)
        }) {
            Ok((meshes, rig)) => {
                known[index] = Some(assemble::Stored {
                    places,
                    frames: rig.frames.clone(),
                });
                loaded.push(pose::Loaded {
                    index,
                    placement: p,
                    meshes,
                    rig,
                });
            }
            Err(e) => problems.push(format!("{}: {e:#}", p.model.path)),
        }
    }
    let rig = match pose::build(&garage.disc, &loaded) {
        Ok(rig) => rig,
        Err(e) => {
            problems.push(format!("motion: {e:#}"));
            return;
        }
    };
    let (joint_entities, skins) = pose::spawn(&mut commands, ac, &rig, &loaded);
    let mut loadout = bay::Loadout::default();
    let mut weapon_anims = weapons::WeaponAnims::default();
    let mut nozzles = Vec::new();
    let mut rack_bones: [Option<(usize, Vec<Entity>, usize, Option<usize>)>; 2] = [None, None];
    let mut rack_named: [Option<Vec<(String, Entity)>>; 2] = [None, None];
    let mut rack_pose = [0u8; 2];
    let mut arms_dummy60: Option<(usize, usize, &assemble::Placement, usize, Entity)> = None;
    let mut ow_arm_attach: Option<weapons::OwArmAttach> = None;
    for (loaded_i, (part, (joints, binds))) in loaded.into_iter().zip(skins).enumerate() {
        boost::note(&part.placement, &part.rig, &joints, &mut nozzles);
        boost::fold(&mut commands, part.placement, &part.rig, &joints);
        let binds_vec = binds;
        let inverse_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds_vec.clone()));
        let root_bone = part
            .rig
            .bones
            .iter()
            .position(|b| b.parent.is_none())
            .unwrap_or(0);
        let root = joints.get(root_bone).copied();
        if let Some(h) = weapons::hardpoint(part.placement) {
            if let Some(root) = root {
                commands.entity(root).insert(h);
            }
        }
        if part.placement.column == "arms" {
            if let Some(arm_bone) = part
                .rig
                .sockets
                .iter()
                .find(|&&(id, _)| id == 60)
                .and_then(|&(_, b)| b)
            {
                if let Some(&arm_entity) = joints.get(arm_bone) {
                    arms_dummy60 =
                        Some((loaded_i, part.index, part.placement, arm_bone, arm_entity));
                }
            }
        }
        if part.placement.column == "back_r" {
            if let Some(root) = root {
                commands.entity(root).insert(weapons::OwHardpoint);
            }
            // Overed Weapons with secondary root `"60"` (`ow0110`, `ow0310`, `ow0610`, `ow6010`)
            // re-attach root `"60"` onto arm dummy `60` (`acattachinfo` record 1) on deployment.
            if let (Some(b60), Some((arms_li, arms_idx, arms_pl, arm_bone, arm_entity))) = (
                part.rig
                    .bones
                    .iter()
                    .position(|b| b.name == "60" && b.parent.is_none()),
                arms_dummy60,
            ) {
                if let (Some(ow_stored), Some(arms_stored), Some(&root_60_entity)) = (
                    known[part.index].as_ref(),
                    known[arms_idx].as_ref(),
                    joints.get(b60),
                ) {
                    let stowed_place = assemble::place_of(&ow_stored.places, Some("60"));
                    if let Some(deployed_place) =
                        assemble::ow_deployed_place(part.placement, arms_pl, arms_stored)
                    {
                        if let (Some(mounted), Some(deployed_local)) = (
                            rig.mount_local(loaded_i, b60),
                            rig.local_for_reattach(
                                loaded_i,
                                b60,
                                stowed_place,
                                deployed_place,
                                arms_li,
                                arm_bone,
                            ),
                        ) {
                            let stowed_parent = mounted.0.map(|p| joint_entities[p]).unwrap_or(ac);
                            ow_arm_attach = Some(weapons::OwArmAttach {
                                root_60: root_60_entity,
                                stowed_parent,
                                stowed_local: mounted.1,
                                deployed_parent: arm_entity,
                                deployed_local,
                                deployed: false,
                            });
                        }
                    }
                }
            }
        }
        if part.placement.column == "arms" {
            let is_wa =
                acvd_data::part_field(part.placement.part as i64, 2, "is_weapon_arm") != 0.0;
            for (bone_name, hand) in [
                ("r_kata", weapons::Hand::Right),
                ("l_kata", weapons::Hand::Left),
            ] {
                if let Some(idx) = part.rig.bones.iter().position(|b| b.name == bone_name) {
                    if let Some(&e) = joints.get(idx) {
                        commands.entity(e).insert(weapons::ShoulderHardpoint);
                    }
                }
                if is_wa {
                    let hp_bone = if hand == weapons::Hand::Right {
                        "r_armhand"
                    } else {
                        "l_armhand"
                    };
                    let idx = part
                        .rig
                        .bones
                        .iter()
                        .position(|b| b.name == hp_bone)
                        .or_else(|| part.rig.bones.iter().position(|b| b.name == bone_name));
                    if let Some(idx) = idx {
                        if let Some(&e) = joints.get(idx) {
                            commands.entity(e).insert(weapons::Hardpoint { hand });
                        }
                    }
                }
            }
        }
        if part.placement.column == "rack_r" || part.placement.column == "rack_l" {
            let right = part.placement.column == "rack_r";
            let front_name = if right { "r_hg_f" } else { "l_hg_f" };
            let back_name = if right { "r_hg_b" } else { "l_hg_b" };
            if let Some(front) = part.rig.bones.iter().position(|b| b.name == front_name) {
                let back = part.rig.bones.iter().position(|b| b.name == back_name);
                let side = usize::from(!right);
                rack_bones[side] = Some((loaded_i, joints.clone(), front, back));
                rack_named[side] = Some(
                    part.rig
                        .bones
                        .iter()
                        .enumerate()
                        .filter_map(|(i, b)| joints.get(i).copied().map(|e| (b.name.clone(), e)))
                        .collect(),
                );
            }
        }
        if let Some((hand, on_bay)) = match part.placement.column {
            "armwep_r" => Some((weapons::Hand::Right, false)),
            "armwep_l" => Some((weapons::Hand::Left, false)),
            "hanger_r" => Some((weapons::Hand::Right, true)),
            "hanger_l" => Some((weapons::Hand::Left, true)),
            _ => None,
        } {
            let weapon_hanger_pose =
                acvd_data::part_field(part.placement.part as i64, 10, "hanger_pose") as u8;
            if on_bay {
                rack_pose[hand as usize] = weapon_hanger_pose;
            }
            let on_prong = if on_bay {
                rack_bones[hand as usize]
                    .as_ref()
                    .and_then(|(rack_i, rack_joints, front, back)| {
                        let base = rig.local_against(loaded_i, root_bone, *rack_i, *front)?;
                        // Attach the bay weapon onto the prong joint (`*_hg_f` / `*_hg_b`) with
                        // the FLVER dummy 87/88/85/86 local translation (`[±0.0012, -0.5188, -0.8182]`
                        // in prong space, placing the peg at the weapon's top-middle rather than
                        // its grip) and flag-4 orientation (`prong_weapon_rotation()`, `0x8288a058`).
                        let local = Transform::from_translation(base.translation)
                            .with_rotation(hanger::prong_weapon_rotation());
                        let parent = *rack_joints.get(*front)?;
                        let alt_parent = back.and_then(|b| rack_joints.get(b).copied());
                        Some((parent, alt_parent, local))
                    })
            } else {
                None
            };
            let (parent, alt_parent, local) = on_prong.unwrap_or_else(|| {
                let mounted = rig.mount_local(loaded_i, root_bone);
                (
                    mounted
                        .and_then(|(p, _)| p)
                        .map(|p| joint_entities[p])
                        .unwrap_or(ac),
                    None,
                    mounted.map(|(_, l)| l).unwrap_or(Transform::IDENTITY),
                )
            });
            if on_bay {
                if let Some(root) = root {
                    // Phase 0 (`a00_011` f=0) starts with the stowed weapon on the back prong
                    // (`r_hg_b` / `l_hg_b`, dummy 88/86) while the front prong (`r_hg_f` / `l_hg_f`,
                    // dummy 87/85) is empty and swings forward at f=0..40 to receive the hand weapon.
                    let initial_prong = alt_parent.unwrap_or(parent);
                    commands
                        .entity(root)
                        .insert((ChildOf(initial_prong), local));
                }
            }
            if root.is_some() {
                loadout.set(
                    hand,
                    on_bay,
                    bay::Mounted {
                        weapon: root,
                        parent,
                        alt_parent,
                        local,
                        hanger_pose: weapon_hanger_pose,
                    },
                );
            }
        }
        let leg_type = acvd_data::part_field(design_data.legs as i64, 3, "leg_type") as u8;
        weapon_anims.load(&garage.disc, part.placement, &part.rig, &joints, leg_type);
        sfx::effect_points(&mut commands, &part.rig, &joints, part.placement.column);
        if part.placement.column == "arms" {
            l_kata_rest = part
                .rig
                .bones
                .iter()
                .position(|b| b.name.eq_ignore_ascii_case("l_kata"))
                .and_then(|i| binds_vec.get(i))
                .map(|m| m.inverse());
        }
        for mesh in part.meshes {
            min = min.min(Vec3::from(mesh.min));
            max = max.max(Vec3::from(mesh.max));
            let center_x = 0.5 * (mesh.min[0] + mesh.max[0]);
            let decal_slot = decal::DecalSlot::from_mesh_bounds(
                part.placement.column,
                mesh.root.as_deref(),
                Some(center_x),
            );
            if let Some(slot) = decal_slot {
                if let Some(cached) = decal::CachedPartMesh::from_mesh(
                    slot,
                    &mesh.mesh,
                    joints.clone(),
                    binds_vec.clone(),
                ) {
                    decal_cache.meshes.push(cached);
                }
            }
            let base = acvd_render::material(
                &garage.disc,
                &mesh,
                packs,
                images,
                garage.flat,
                &mut missing,
            );
            let painted = accolor.as_ref().and_then(|col| {
                paint::material_with_custom_camo(
                    &garage.disc,
                    &base,
                    &mesh,
                    part.placement.column,
                    col,
                    decal_slot.map(|s| decals.slot_handle(s)),
                    assets.custom_camos.active_handle(),
                    assets.custom_camos.custom_scale_value(),
                    packs,
                    images,
                )
            });
            let mut mesh_entity = commands.spawn((
                AcPartColumn(part.placement.column),
                Mesh3d(meshes.add(mesh.mesh)),
                SkinnedMesh {
                    inverse_bindposes: inverse_bindposes.clone(),
                    joints: joints.clone(),
                },
                NoFrustumCulling,
                Transform::default(),
                ChildOf(ac),
            ));
            match painted {
                Some(mat) => {
                    let group = acvd_formats::accolor::slot_group(
                        part.placement.column,
                        mesh.root.as_deref(),
                    );
                    mesh_entity.insert((
                        MeshMaterial3d(paint_materials.add(mat)),
                        paint::AcPaintGroup(group),
                    ));
                }
                None => {
                    mesh_entity.insert(MeshMaterial3d(materials.add(base)));
                }
            }
            if let Some(root) = root {
                if matches!(
                    part.placement.column,
                    "armwep_r" | "armwep_l" | "hanger_r" | "hanger_l"
                ) {
                    mesh_entity.insert(bay::WeaponMesh { root });
                }
            }
            if part.placement.column == "back_r" {
                mesh_entity.insert(weapons::OwMesh);
            }
        }
    }
    decal_cache.shoulder_emblem = l_kata_rest.and_then(|rest| {
        let left_meshes: Vec<&decal::CachedPartMesh> = decal_cache
            .meshes
            .iter()
            .filter(|m| m.slot == decal::DecalSlot::LeftArm)
            .collect();
        decal::build_shoulder_emblem_projector(design_data.arms, rest, &left_meshes)
    });
    decals.mark_changed_frame_parts(
        design_data.head,
        design_data.core,
        design_data.arms,
        design_data.legs,
    );
    commands.insert_resource(loadout);
    if let Some(att) = ow_arm_attach {
        commands.insert_resource(att);
    } else {
        commands.remove_resource::<weapons::OwArmAttach>();
    }
    let racks = rack_named
        .into_iter()
        .enumerate()
        .filter_map(|(i, bones)| {
            Some((
                if i == 0 {
                    weapons::Hand::Right
                } else {
                    weapons::Hand::Left
                },
                rack_pose[i],
                bones?,
            ))
        })
        .collect();
    commands.insert_resource(hanger::HangerPose::load(&garage.disc, racks));
    for entity in weapon_anims.joints() {
        commands.entity(entity).insert(weapons::ClipJoint);
    }
    for (entity, transform) in weapon_anims.rest_pose() {
        commands.entity(entity).insert(transform);
    }
    commands.insert_resource(weapon_anims);
    match boost::BoostPose::load(&garage.disc, nozzles) {
        Some(boost) => commands.insert_resource(boost),
        None => warn!("booster motion bank did not load"),
    }
    match rig.motion {
        Some(mut motion) => {
            if let Some(name) = garage.clip.take() {
                match motion.clips.iter().position(|a| a.entry == name) {
                    Some(i) => {
                        if let Err(e) = motion.select(&garage.disc, i) {
                            problems.push(format!("clip {name}: {e:#}"));
                        }
                    }
                    None => problems.push(format!("no clip {name} in {}", motion.set)),
                }
            }
            if let Some(f) = garage.frame.take() {
                (motion.frame, motion.playing) = (f, false);
            }
            commands.insert_resource(motion);
        }
        None => commands.remove_resource::<pose::Motion>(),
    }

    let mut status = format!("{} - {} models", design_name, built.placements.len());
    if missing > 0 {
        status.push_str(&format!(", {missing} textures missing"));
    }
    if !problems.is_empty() {
        status.push_str(&format!(", {} slots unplaced", problems.len()));
    }
    for p in &problems {
        warn!("design {}: {p}", design_name);
    }
    let ctrl = AcCtrlParam::calculate(&design_data);
    let fcs = fcs::Fcs::from_ctrl(&ctrl);
    let legs_motion_id = acvd_data::part_field(design_data.legs as i64, 3, "legs_motion_id") as u8;
    let mut pilot = control::Pilot::new(ctrl, legs_motion_id);
    pilot.yaw = if *mode == garage::Mode::Sortie {
        start_aim.yaw.unwrap_or(spawn_yaw)
    } else {
        spawn_yaw
    };
    if *mode == garage::Mode::Sortie {
        if let Some(p) = start_aim.pitch {
            pilot.set_pitch(p);
        }
    }
    if *mode == garage::Mode::Sortie && spawn_y.is_none() && !collision.hits.is_empty() {
        pilot.airborne = true;
    }
    commands.entity(ac).insert((
        pilot,
        fcs,
        scan::ScanState::from_design(&design_data),
        weapons::Armament::from_design(&design_data),
        damage::PlayerHealth::from_design(&design_data),
        hud::Status::from_design(&design_data),
    ));
    if min.x <= max.x {
        garage.bounds = (min, max);
        if *mode == garage::Mode::Sortie {
            if let Ok(mut o) = orbit.single_mut() {
                o.frame(spawn_pos + min, spawn_pos + max);
            }
        }
    }
    info!("{status}");
    if let Ok(mut w) = window.single_mut() {
        w.title = if *mode == garage::Mode::Garage {
            format!("ACVD Garage — {design_name}")
        } else {
            status.clone()
        };
    }
    garage.status = status;
    if *mode == garage::Mode::Sortie {
        if let Some(hud) = hud.as_deref() {
            // Tint is not game data; DRB colours are still unread.
            text::spawn(
                &mut commands,
                &hud.font,
                &hud_lines(&design_name, &design_data, &hud.parts),
                Vec2::new(24.0, 20.0),
                2.0,
                Color::srgb(0.92, 0.95, 0.98),
            );
        }
    }
    if let Some(mut shot) = shot {
        shot.ready = true;
    }
}

fn hud_lines(name: &str, design: &AcAssemblyDesignSt, parts: &Fmg) -> String {
    let mut text = name.to_string();
    let names: Vec<&str> = [design.head, design.core, design.legs]
        .into_iter()
        .filter_map(|id| parts.get(id as i32))
        .collect();
    if !names.is_empty() {
        text.push('\n');
        text.push_str(&names.join("  "));
    }
    text
}

/// Up/Down: previous/next clip of the motion set, Enter: pause.
fn clips(
    keys: Res<ButtonInput<KeyCode>>,
    garage: Res<Garage>,
    piloting: Res<control::Piloting>,
    motion: Option<ResMut<pose::Motion>>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Some(mut m) = motion else { return };
    if piloting.0 {
        return;
    }
    if keys.just_pressed(KeyCode::Enter) {
        m.playing = !m.playing;
    }
    let step = [(KeyCode::ArrowDown, 1), (KeyCode::ArrowUp, -1)]
        .iter()
        .filter(|(k, _)| keys.just_pressed(*k))
        .map(|(_, s)| *s)
        .sum::<isize>();
    let n = m.clips.len() as isize;
    let mut index = m.index;
    for _ in 0..n.max(1) {
        if step == 0 || n == 0 {
            break;
        }
        index = (index as isize + step).rem_euclid(n) as usize;
        match m.select(&garage.disc, index) {
            Ok(()) => break,
            Err(e) => warn!("{e:#}"),
        }
    }
    if step != 0 || m.is_added() {
        let title = format!(
            "{} - {} ({} frames)",
            garage.status,
            m.name(),
            m.clip.frames
        );
        info!("{title}");
        if let Ok(mut w) = window.single_mut() {
            w.title = title;
        }
    }
}
