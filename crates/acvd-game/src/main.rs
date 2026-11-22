//! Runtime skeleton: assembles ACs from the preset designs in `param/acassemblydrawing.bin`,
//! entirely from generated data plus geometry read off the owned disc.
//!
//! `acvd-game [design id] [--disc <360 ISO>] [--shot <png>] [--flat] [--yaw <degrees>]
//! [--clip <entry>] [--frame <n>] [--hold <keys>] [--wait <seconds>] [--map <id>] [--plane]
//! [--water <y>]`
//! Piloting (see `control`): WASD move, Q/E turn, Up/Down pitch, Shift boost mode, Space jump,
//! V high boost (quick boost) / hold V (or K, gamepad □ + HOLD) Boost Charge Kick, Ctrl glide boost while boosting on the ground
//! (air clips once it leaves the ground);
//! F / left mouse / R2 fire the right arm weapon (guns or melee blades/piles), C / right mouse / L2 the left,
//! G / X / Tab / R1 fire the shoulder weapon / locked missile salvo (see `weapons`).
//! Z / Select / R3 toggles Scan Mode (`AC + 0xdb0`, `scan`): reduces passive EN drain for faster
//! recharge, disables weapon fire, switches the HUD lock sight to `ACV_ScanModeLockSight`, and
//! turns F / left mouse / R2 into Target Scan (populating `ACV_Scan_Cur_EnemyParamInfo` with
//! AP and KE/CE/TE defenses). B / East (○) deploys a Recon unit from the design's category-13
//! `ob` slot (`bullet/bulletrecon.bin`), marking targets inside its `effective_range`.
//! Hold R (△ / gamepad North) and press that fire button to bay-shift: the hand weapon trades
//! places with the one on the rack (see `bay`). A ready-position weapon is purged instead,
//! because it cannot sit on the bay.
//! If the design equips an Overed Weapon (`back_r`, category 11: designs `9002`, `9003`, `9004`,
//! `9999`), hold U (or hold R / △ without pressing fire for 1 s) to arm it and purge the left-arm
//! weapon, then hold F / left mouse / R2 to charge and release at full charge to fire (auto-purges
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
//! P switches to the clip browser: Up/Down previous/next clip, Space pause. Left/Right: previous/next design,
//! drag left mouse: orbit, wheel: zoom, R: reframe. `--clip`/`--frame` start in the browser,
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

mod ai;
mod assemble;
mod bay;
mod blur;
mod boost;
mod collision;
mod control;
mod damage;
mod env;
mod fcs;
mod hanger;
mod hud;
mod map;
mod paint;
mod pose;
mod scan;
mod sfx;
mod sound;
mod targets;
mod weapons;

use std::path::PathBuf;

use acvd_data::generated::ac_unit::{AcAssemblyDesignSt, AC_ASSEMBLY_DESIGN_ST_FILES};
use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::Row;
use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::{self, Disc};
use acvd_render::app::{orbit, take_shot, Orbit, Shot};
use acvd_render::text::{self, Font};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use collision::{Collision, Layer, Plane, RAY_HALF};

#[derive(Resource)]
struct Garage {
    disc: Disc,
    designs: Vec<&'static Row<AcAssemblyDesignSt>>,
    current: usize,
    shown: Option<usize>,
    flat: bool,
    bounds: (Vec3, Vec3),
    /// Clip entry and paused frame to start the first design on.
    clip: Option<String>,
    frame: Option<f32>,
    status: String,
}

/// The assembled AC; its parts are children.
#[derive(Component)]
struct Ac;

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

/// The map whose models are drawn (`None`: draw the hit meshes or the floor plane).
#[derive(Resource)]
struct Scene {
    map: Option<String>,
    layout: Option<String>,
    test: targets::TestMode,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut wanted, mut disc, mut shot, mut flat, mut yaw) = (None, None, None, false, None);
    let (mut clip, mut frame, mut held, mut wait) = (None, None, Vec::new(), 0.0);
    let (mut map, mut plane, mut water) = (Some("m4000".to_string()), false, None);
    let (mut layout, mut hits) = (Some("actest".to_string()), false);
    let mut test = targets::TestMode::default();
    let (mut preview, mut burst, mut no_hud) = (None, 1, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--disc" => disc = args.next().map(PathBuf::from),
            "--shot" => shot = args.next().map(|p| Shot::new(PathBuf::from(p))),
            "--flat" => flat = true,
            "--yaw" => {
                yaw = args
                    .next()
                    .and_then(|d| d.parse::<f32>().ok())
                    .map(f32::to_radians)
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
                }
            }
            "--hits" => hits = true,
            "--plane" => plane = true,
            "--water" => water = args.next().and_then(|s| s.parse::<f32>().ok()),
            "--sfx" => preview = args.next().and_then(|s| s.parse::<i32>().ok()),
            "--burst" => burst = args.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1),
            "--no-hud" => no_hud = true,
            _ => wanted = a.parse::<u32>().ok(),
        }
    }
    let piloting = clip.is_none() && frame.is_none();
    // Piloting starts behind the AC, the browser in front of it.
    let yaw = yaw.unwrap_or(if piloting { 0.25 } else { 2.6 });
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let disc =
        Disc::open(&disc.unwrap_or_else(|| vfs::default_disc(&root))).expect("opening the disc");
    let mut collision = if plane {
        Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        )
    } else if let Some(id) = &map {
        match collision::load_map(&disc, &id) {
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
    let start = match map.as_deref().filter(|_| !plane) {
        Some(id) => map::start(&disc, id, layout.as_deref()).unwrap_or_else(|e| {
            eprintln!("map {id} start point: {e:#}");
            None
        }),
        None => None,
    };
    let start = start.unwrap_or(map::Start {
        position: Vec3::ZERO,
        yaw: 0.0,
    });
    let area_bounds = match map.as_deref().filter(|_| !plane) {
        Some(id) => map::AreaBounds::load(&disc, id, layout.as_deref()).unwrap_or_else(|e| {
            eprintln!("map {id} area bounds: {e:#}");
            map::AreaBounds::default()
        }),
        None => map::AreaBounds::default(),
    };
    let map_nav = match map.as_deref().filter(|_| !plane) {
        Some(id) => match map::MapNav::load(&disc, id) {
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
    let water_return = match map.as_deref().filter(|_| !plane) {
        Some(id) => match map::WaterReturnPoints::load(&disc, id, layout.as_deref(), water) {
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
        .unwrap_or(0);
    if let Some(id) = wanted.filter(|&id| designs.get(current).is_none_or(|r| r.id != id)) {
        eprintln!("no design {id}; starting at the first");
    }

    let enemy_parts = weapons::EnemyParts::load(&disc);
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "acvd-game".into(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins((blur::BlurPlugin, paint::PaintPlugin, acvd_render::menu::MenuPlugin))
    .add_plugins(sfx::SfxPlugin { disc: disc.clone() })
    .add_plugins(sound::SoundPlugin { disc: disc.clone() })
    .add_plugins(env::EnvPlugin { disc: disc.clone() })
    .insert_resource(ClearColor(Color::srgb(0.32, 0.36, 0.42)))
    .insert_resource(enemy_parts)
    .insert_resource(Garage {
        disc,
        designs,
        current,
        shown: None,
        flat,
        bounds: (Vec3::ZERO, Vec3::ONE),
        clip,
        frame,
        status: String::new(),
    })
    .insert_resource(StartYaw(yaw))
    .insert_resource(start)
    .insert_resource(area_bounds)
    .insert_resource(map_nav)
    .insert_resource(water_return)
    .insert_resource(Scene {
        map: map.filter(|_| !plane && !hits),
        layout: layout.filter(|_| !plane),
        test,
    })
    .insert_resource(collision)
    .insert_resource(control::Piloting(piloting))
    .insert_resource(control::Held(held))
    .insert_resource(ShowHud(!no_hud))
    .add_systems(Startup, (setup, weapons::setup))
    .add_systems(
        Update,
        (
            (
                browse,
                show,
                clips,
                control::pilot,
                scan::update,
                fcs::update,
                map::lod,
                pose::animate,
                boost::pose,
                boost::hide,
                hanger::pose,
            )
                .chain(),
            (
                bay::shift,
                weapons::fire,
                weapons::step_overed_weapon,
                weapons::step_melee,
                ai::update,
                targets::animate_enemies,
                pose::animate_npcs,
                weapons::step_projectiles,
                weapons::step_missiles,
                weapons::tick_player_health,
                targets::tick,
                map::step_destructibles,
                orbit,
                reframe,
            )
                .chain(),
        )
            .chain(),
    )
    .add_systems(
        Update,
        (
            hud::layout,
            hud::tick,
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
            .after(weapons::tick_player_health),
    );
    if let Some(id) = preview {
        app.insert_resource(sfx::Preview(id));
    }
    if let Some(mut shot) = shot {
        shot.burst = burst;
        app.insert_resource(ShotDelay(wait, false))
            .insert_resource(shot)
            .insert_resource(acvd_render::app::ShotMark::default())
            .add_systems(Update, (delay_shot, take_shot).chain().after(pose::animate));
    }
    app.run();
}

/// Seconds `--shot` still waits after the AC is shown (`--wait`), and whether it was shown.
#[derive(Resource)]
struct ShotDelay(f32, bool);

fn delay_shot(time: Res<Time>, mut delay: ResMut<ShotDelay>, mut shot: ResMut<Shot>) {
    delay.1 |= shot.ready;
    if delay.1 && delay.0 > 0.0 {
        delay.0 -= time.delta_secs();
        shot.ready = delay.0 <= 0.0;
    }
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

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut paint_materials: ResMut<Assets<paint::AcPaintMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    garage: Res<Garage>,
    enemy_parts: Res<weapons::EnemyParts>,
    yaw: Res<StartYaw>,
    collision: Res<Collision>,
    scene: Res<Scene>,
    show_hud: Res<ShowHud>,
) {
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            far: VIEW_FAR,
            ..default()
        }),
        Transform::default(),
        Orbit::new(yaw.0, 0.25),
        blur::ZoomBlur::default(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 10.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 2500.0,
            ..default()
        },
        Transform::from_xyz(-6.0, 3.0, -4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let drawn = scene.map.as_deref().map_or(0, |id| {
        match map::spawn(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &garage.disc,
            id,
        ) {
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
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut paint_materials,
            &mut images,
            &mut bindposes,
            &garage.disc,
            &enemy_parts,
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
            commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(materials.add(mat))));
        }
    }
    let water = materials.add(StandardMaterial {
        base_color: Color::srgba(0.15, 0.35, 0.6, 0.5),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    for p in collision.planes.iter().filter(|p| p.layer == Layer::Water) {
        commands.spawn((
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
    if show_hud.0 {
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
    mut orbit: Query<&mut Orbit>,
) {
    if piloting.0 || !keys.just_pressed(KeyCode::KeyR) {
        return;
    }
    if let Ok(mut o) = orbit.single_mut() {
        o.frame(garage.bounds.0, garage.bounds.1);
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
        garage.current = (garage.current as isize + step).rem_euclid(n) as usize;
    }
}

#[allow(clippy::too_many_arguments)]
fn show(
    mut commands: Commands,
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
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut paint_materials: ResMut<Assets<paint::AcPaintMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut orbit: Query<&mut Orbit>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    labels: Query<Entity, With<text::Label>>,
    hud: Option<Res<Hud>>,
    collision: Res<Collision>,
    start: Res<map::Start>,
    shot: Option<ResMut<Shot>>,
) {
    if garage.shown == Some(garage.current) {
        return;
    }
    let Some(&design) = garage.designs.get(garage.current) else {
        return;
    };
    garage.shown = Some(garage.current);
    for e in acs.iter().chain(shots.iter()).chain(labels.iter()) {
        commands.entity(e).despawn();
    }
    commands.remove_resource::<weapons::WeaponAnims>();
    commands.remove_resource::<weapons::Shift>();
    commands.remove_resource::<hanger::HangerPose>();
    commands.remove_resource::<boost::BoostPose>();

    let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design.data);
    let accolor = (!garage.flat)
        .then(|| paint::load_accolor(&garage.disc, design.data.coloring_id))
        .flatten();
    let spawn_y = collision.ground_below(start.position + Vec3::Y * RAY_HALF);
    let ac = commands
        .spawn((
            Ac,
            Transform::from_translation(start.position.with_y(spawn_y.unwrap_or(start.position.y)))
                .with_rotation(Quat::from_rotation_y(start.yaw)),
            Visibility::default(),
        ))
        .id();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut packs, mut missing) = (acvd_render::Packs::default(), 0);
    let mut problems = built.problems;
    let mut loaded = Vec::new();
    let mut known = vec![None; built.placements.len()];
    for (index, p) in built.placements.iter().enumerate() {
        let places = assemble::root_places(p, &built.placements, &known);
        debug!("{} {}", p.column, p.model.path);
        match acvd_render::rigged_model(&garage.disc, p.model.path, &|root| {
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
    let mut rack_bones: [Option<(usize, Vec<Entity>, usize)>; 2] = [None, None];
    let mut rack_named: [Option<Vec<(String, Entity)>>; 2] = [None, None];
    let mut rack_pose = [0u8; 2];
    let mut arms_dummy60: Option<(usize, usize, &assemble::Placement, usize, Entity)> = None;
    let mut ow_arm_attach: Option<weapons::OwArmAttach> = None;
    for (loaded_i, (part, (joints, binds))) in loaded.into_iter().zip(skins).enumerate() {
        boost::note(&part.placement, &part.rig, &joints, &mut nozzles);
        boost::fold(&mut commands, part.placement, &part.rig, &joints);
        let inverse_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds));
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
                    arms_dummy60 = Some((loaded_i, part.index, part.placement, arm_bone, arm_entity));
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
                            let stowed_parent = mounted
                                .0
                                .map(|p| joint_entities[p])
                                .unwrap_or(ac);
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
            let prong = if right { "r_hg_f" } else { "l_hg_f" };
            if let Some(bone) = part.rig.bones.iter().position(|b| b.name == prong) {
                let side = usize::from(!right);
                rack_bones[side] = Some((loaded_i, joints.clone(), bone));
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
            if on_bay {
                rack_pose[hand as usize] =
                    acvd_data::part_field(part.placement.part as i64, 10, "hanger_pose") as u8;
            }
            let on_prong = if on_bay {
                rack_bones[hand as usize]
                    .as_ref()
                    .and_then(|(rack_i, rack_joints, prong)| {
                        let local = rig.local_against(loaded_i, root_bone, *rack_i, *prong)?;
                        let parent = *rack_joints.get(*prong)?;
                        Some((parent, local))
                    })
            } else {
                None
            };
            let (parent, local) = on_prong.unwrap_or_else(|| {
                let mounted = rig.mount_local(loaded_i, root_bone);
                (
                    mounted
                        .and_then(|(p, _)| p)
                        .map(|p| joint_entities[p])
                        .unwrap_or(ac),
                    mounted.map(|(_, l)| l).unwrap_or(Transform::IDENTITY),
                )
            });
            if on_bay {
                if let Some(root) = root {
                    commands.entity(root).insert((ChildOf(parent), local));
                }
            }
            if root.is_some() {
                loadout.set(
                    hand,
                    on_bay,
                    bay::Mounted {
                        weapon: root,
                        parent,
                        local,
                    },
                );
            }
        }
        weapon_anims.load(&garage.disc, part.placement, &part.rig, &joints);
        sfx::effect_points(&mut commands, &part.rig, &joints, part.placement.column);
        for mesh in part.meshes {
            min = min.min(Vec3::from(mesh.min));
            max = max.max(Vec3::from(mesh.max));
            let base = acvd_render::material(
                &garage.disc,
                &mesh,
                &mut packs,
                &mut images,
                garage.flat,
                &mut missing,
            );
            let painted = accolor.as_ref().and_then(|col| {
                paint::material(
                    &garage.disc,
                    &base,
                    &mesh,
                    part.placement.column,
                    col,
                    &mut packs,
                    &mut images,
                )
            });
            let mut mesh_entity = commands.spawn((
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
                    mesh_entity.insert(MeshMaterial3d(paint_materials.add(mat)));
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

    let mut status = format!(
        "[{}/{}] design {} {} - {} models",
        garage.current + 1,
        garage.designs.len(),
        design.id,
        design.name,
        built.placements.len()
    );
    if missing > 0 {
        status.push_str(&format!(", {missing} textures missing"));
    }
    if !problems.is_empty() {
        status.push_str(&format!(", {} slots unplaced", problems.len()));
    }
    for p in &problems {
        warn!("design {}: {p}", design.id);
    }
    let ctrl = AcCtrlParam::calculate(&design.data);
    let fcs = fcs::Fcs::from_ctrl(&ctrl);
    let legs_motion_id = acvd_data::part_field(design.data.legs as i64, 3, "legs_motion_id") as u8;
    let mut pilot = control::Pilot::new(ctrl, legs_motion_id);
    pilot.yaw = start.yaw;
    if spawn_y.is_none() && !collision.hits.is_empty() {
        pilot.airborne = true;
    }
    commands.entity(ac).insert((
        pilot,
        fcs,
        scan::ScanState::from_design(&design.data),
        weapons::Armament::from_design(&design.data),
        damage::PlayerHealth::from_design(&design.data),
        hud::Status::from_design(&design.data),
    ));
    if min.x <= max.x {
        garage.bounds = (min, max);
        if let Ok(mut o) = orbit.single_mut() {
            o.frame(min, max);
        }
    }
    info!("{status}");
    if let Ok(mut w) = window.single_mut() {
        w.title = status.clone();
    }
    garage.status = status;
    if let Some(hud) = hud.as_deref() {
        // Tint is not game data; DRB colours are still unread.
        text::spawn(
            &mut commands,
            &hud.font,
            &hud_lines(design, &hud.parts),
            Vec2::new(24.0, 20.0),
            2.0,
            Color::srgb(0.92, 0.95, 0.98),
        );
    }
    if let Some(mut shot) = shot {
        shot.ready = true;
    }
}

fn hud_lines(design: &Row<AcAssemblyDesignSt>, parts: &Fmg) -> String {
    let mut text = format!("{} {}", design.id, design.name);
    let names: Vec<&str> = [design.data.head, design.data.core, design.data.legs]
        .into_iter()
        .filter_map(|id| parts.get(id as i32))
        .collect();
    if !names.is_empty() {
        text.push('\n');
        text.push_str(&names.join("  "));
    }
    text
}

/// Up/Down: previous/next clip of the motion set, Space: pause.
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
    if keys.just_pressed(KeyCode::Space) {
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
