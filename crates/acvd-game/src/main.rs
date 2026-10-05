//! Runtime skeleton: assembles ACs from the preset designs in `param/acassemblydrawing.bin`,
//! entirely from generated data plus geometry read off the owned disc.
//!
//! `acvd-game [design id] [--disc <dump root>] [--shot <png>] [--flat] [--yaw <degrees>]
//! [--clip <entry>] [--frame <n>] [--hold <keys>] [--wait <seconds>] [--map <id>] [--plane]
//! [--water <y>]`
//! Piloting (see `control`): WASD move, Q/E turn, Up/Down pitch, Shift boost mode, Space jump;
//! F / left mouse / R2 fire the right arm weapon, C / right mouse / L2 the left (see `weapons`);
//! P switches to the clip browser: Up/Down previous/next clip, Space pause. Left/Right: previous/next design,
//! drag left mouse: orbit, wheel: zoom, R: reframe. `--clip`/`--frame` start in the browser,
//! `--frame` paused on that frame. `--hold w,shift,space,f` holds keys for the whole run, and
//! `--wait` delays `--shot` that long. Default collision is map `m3100` from the disc; `--map`
//! picks another `model/map` folder, `--plane` is the old infinite floor, `--water` adds a test
//! water plane at that height. The lock-sight HUD (see `hud`) is drawn over gameplay.
//! Boosters, muzzle flashes, tracers and hits play FFX effects (see `sfx`); `--sfx <id>` keeps
//! effect `id` playing in front of the AC. `--burst <n>` makes `--shot` save `n` frames 0.05 s
//! apart (`<stem>_<i>.png`).

mod assemble;
mod blur;
mod collision;
mod control;
mod hud;
mod pose;
mod sfx;
mod weapons;

use std::path::PathBuf;

use acvd_data::generated::ac_unit::{AcAssemblyDesignSt, AC_ASSEMBLY_DESIGN_ST_FILES};
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
use acvd_data::Row;
use acvd_formats::fmg::Fmg;
use acvd_render::app::{orbit, take_shot, Orbit, Shot};
use acvd_render::text::{self, Font};
use collision::{Collision, Layer, Plane, RAY_HALF};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

#[derive(Resource)]
struct Garage {
    usrdir: PathBuf,
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

/// Disc font (`fontdef.xml` ID 1) and the English part-name bank.
#[derive(Resource)]
struct Hud {
    font: Font,
    parts: Fmg,
}

/// Initial camera yaw in radians (`--yaw <degrees>`).
#[derive(Resource)]
struct StartYaw(f32);

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut wanted, mut disc, mut shot, mut flat, mut yaw) = (None, None, None, false, None);
    let (mut clip, mut frame, mut held, mut wait) = (None, None, Vec::new(), 0.0);
    let (mut map, mut plane, mut water) = (Some("m3100".to_string()), false, None);
    let (mut preview, mut burst) = (None, 1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--disc" => disc = args.next().map(PathBuf::from),
            "--shot" => shot = args.next().map(|p| Shot::new(PathBuf::from(p))),
            "--flat" => flat = true,
            "--yaw" => yaw = args.next().and_then(|d| d.parse::<f32>().ok()).map(f32::to_radians),
            "--clip" => clip = args.next(),
            "--frame" => frame = args.next().and_then(|f| f.parse::<f32>().ok()),
            "--hold" => held = args.next().unwrap_or_default().split(',').filter_map(key).collect(),
            "--wait" => wait = args.next().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
            "--map" => map = args.next().filter(|s| s != "none"),
            "--plane" => plane = true,
            "--water" => water = args.next().and_then(|s| s.parse::<f32>().ok()),
            "--sfx" => preview = args.next().and_then(|s| s.parse::<i32>().ok()),
            "--burst" => burst = args.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1),
            _ => wanted = a.parse::<u32>().ok(),
        }
    }
    let piloting = clip.is_none() && frame.is_none();
    // Piloting starts behind the AC, the browser in front of it.
    let yaw = yaw.unwrap_or(if piloting { 0.25 } else { 2.6 });
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let usrdir = acvd_formats::vfs::usrdir(&disc.unwrap_or_else(|| root.join("ACVD Unbound")));
    let mut collision = if plane {
        Collision { planes: vec![Plane { y: 0.0, layer: Layer::Ground }], hits: Vec::new() }
    } else if let Some(id) = map {
        match collision::load_map(&usrdir, &id) {
            Ok(c) => {
                eprintln!("map {id}: {} hit triangles", c.hits.len());
                c
            }
            Err(e) => {
                eprintln!("map {id}: {e:#}; using a flat plane");
                Collision { planes: vec![Plane { y: 0.0, layer: Layer::Ground }], hits: Vec::new() }
            }
        }
    } else {
        Collision { planes: vec![Plane { y: 0.0, layer: Layer::Ground }], hits: Vec::new() }
    };
    if let Some(y) = water {
        collision.planes.push(Plane { y, layer: Layer::Water });
    }
    let designs: Vec<&'static Row<AcAssemblyDesignSt>> = AC_ASSEMBLY_DESIGN_ST_FILES
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .filter(|r| AC_ASSEMBLY_DESIGN_ST_SLOTS.iter().any(|s| (s.part)(&r.data) > 0) && AcCtrlParam::buildable(&r.data))
        .collect();
    let current = wanted.and_then(|id| designs.iter().position(|r| r.id == id)).unwrap_or(0);
    if let Some(id) = wanted.filter(|&id| designs.get(current).is_none_or(|r| r.id != id)) {
        eprintln!("no design {id}; starting at the first");
    }

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: "acvd-game".into(), ..default() }),
        ..default()
    }))
    .add_plugins((blur::BlurPlugin, acvd_render::menu::MenuPlugin))
    .add_plugins(sfx::SfxPlugin { usrdir: usrdir.clone() })
    .insert_resource(ClearColor(Color::srgb(0.32, 0.36, 0.42)))
    .insert_resource(Garage { usrdir, designs, current, shown: None, flat, bounds: (Vec3::ZERO, Vec3::ONE), clip, frame, status: String::new() })
    .insert_resource(StartYaw(yaw))
    .insert_resource(collision)
    .insert_resource(control::Piloting(piloting))
    .insert_resource(control::Held(held))
    .add_systems(Startup, (setup, weapons::setup))
    .add_systems(Update, (browse, show, clips, control::pilot, pose::animate, weapons::fire, orbit).chain())
    .add_systems(Update, (hud::layout, hud::readouts).chain().after(weapons::fire));
    if let Some(id) = preview {
        app.insert_resource(sfx::Preview(id));
    }
    if let Some(mut shot) = shot {
        shot.burst = burst;
        app.insert_resource(ShotDelay(wait, false)).insert_resource(shot).add_systems(Update, (delay_shot, take_shot).chain().after(pose::animate));
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
        "f" | "fire" => KeyCode::KeyF,
        "c" => KeyCode::KeyC,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        other => {
            eprintln!("--hold: unknown key `{other}`");
            return None;
        }
    })
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    garage: Res<Garage>,
    yaw: Res<StartYaw>,
    collision: Res<Collision>,
) {
    commands.spawn((Camera3d::default(), Transform::default(), Orbit::new(yaw.0, 0.25), blur::ZoomBlur::default()));
    commands.spawn((DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: true, ..default() }, Transform::from_xyz(4.0, 10.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y)));
    commands.spawn((DirectionalLight { illuminance: 2500.0, ..default() }, Transform::from_xyz(-6.0, 3.0, -4.0).looking_at(Vec3::ZERO, Vec3::Y)));
    if collision.hits.is_empty() {
        commands.spawn((
            Mesh3d(meshes.add(Plane3d::default().mesh().size(FLOOR, FLOOR))),
            MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.22, 0.23, 0.25), perceptual_roughness: 0.95, ..default() })),
        ));
    } else {
        for (layer, color) in [(Layer::Ground, Color::srgb(0.22, 0.23, 0.25)), (Layer::Water, Color::srgba(0.15, 0.35, 0.6, 0.5))] {
            let Some(mesh) = debug_mesh(collision.hits.iter().filter(|h| h.layer == layer)) else { continue };
            let mut mat = StandardMaterial { base_color: color, perceptual_roughness: 0.95, ..default() };
            if layer == Layer::Water {
                mat.alpha_mode = AlphaMode::Blend;
            }
            commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(materials.add(mat))));
        }
    }
    let water = materials.add(StandardMaterial { base_color: Color::srgba(0.15, 0.35, 0.6, 0.5), alpha_mode: AlphaMode::Blend, ..default() });
    for p in collision.planes.iter().filter(|p| p.layer == Layer::Water) {
        commands.spawn((Mesh3d(meshes.add(Plane3d::default().mesh().size(FLOOR, FLOOR))), MeshMaterial3d(water.clone()), Transform::from_xyz(0.0, p.y, 0.0)));
    }
    if collision.hits.is_empty() {
        let (pillar, grey) = (meshes.add(Cuboid::new(1.0, 6.0, 1.0)), materials.add(Color::srgb(0.45, 0.47, 0.5)));
        let n = (FLOOR / 2.0 / PILLAR_SPACING) as i32;
        for x in -n..=n {
            for z in -n..=n {
                if (x, z) != (0, 0) {
                    commands.spawn((Mesh3d(pillar.clone()), MeshMaterial3d(grey.clone()), Transform::from_xyz(x as f32 * PILLAR_SPACING, 3.0, z as f32 * PILLAR_SPACING)));
                }
            }
        }
    }
    match load_hud(&garage.usrdir, &mut images) {
        Ok(hud) => commands.insert_resource(hud),
        Err(e) => warn!("hud: {e:#}"),
    }
    match hud::load(&garage.usrdir, &mut images) {
        Ok(sortie) => commands.insert_resource(sortie),
        Err(e) => warn!("sortie hud: {e:#}"),
    }
}

fn load_hud(usrdir: &std::path::Path, images: &mut Assets<Image>) -> anyhow::Result<Hud> {
    Ok(Hud {
        font: text::load(usrdir, "e1_ext", images)?,
        parts: acvd_formats::fmg::read(&acvd_formats::vfs::open(usrdir, "lang/en/text/partsname_en.fmg")?)?,
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
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    mesh.compute_smooth_normals();
    Some(mesh)
}

const FLOOR: f32 = 1000.0;
const PILLAR_SPACING: f32 = 40.0;

fn browse(keys: Res<ButtonInput<KeyCode>>, mut garage: ResMut<Garage>) {
    let n = garage.designs.len() as isize;
    let step = [(KeyCode::ArrowRight, 1), (KeyCode::ArrowLeft, -1)].iter().filter(|(k, _)| keys.just_pressed(*k)).map(|(_, s)| *s).sum::<isize>();
    if step != 0 && n > 0 {
        garage.current = (garage.current as isize + step).rem_euclid(n) as usize;
    }
}

#[allow(clippy::too_many_arguments)]
fn show(
    mut commands: Commands,
    mut garage: ResMut<Garage>,
    keys: Res<ButtonInput<KeyCode>>,
    acs: Query<Entity, With<Ac>>,
    shots: Query<Entity, With<weapons::Projectile>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut orbit: Query<&mut Orbit>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    labels: Query<Entity, With<text::Label>>,
    hud: Option<Res<Hud>>,
    collision: Res<Collision>,
    shot: Option<ResMut<Shot>>,
) {
    if garage.shown == Some(garage.current) {
        if keys.just_pressed(KeyCode::KeyR) {
            if let Ok(mut o) = orbit.single_mut() {
                o.frame(garage.bounds.0, garage.bounds.1);
            }
        }
        return;
    }
    let Some(&design) = garage.designs.get(garage.current) else { return };
    garage.shown = Some(garage.current);
    for e in acs.iter().chain(shots.iter()).chain(labels.iter()) {
        commands.entity(e).despawn();
    }

    let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design.data);
    let spawn_y = collision.ground_below(Vec3::new(0.0, RAY_HALF, 0.0));
    let ac = commands.spawn((Ac, Transform::from_xyz(0.0, spawn_y.unwrap_or(0.0), 0.0), Visibility::default())).id();
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut packs, mut missing) = (acvd_render::Packs::default(), 0);
    let mut problems = built.problems;
    let mut loaded = Vec::new();
    for (index, p) in built.placements.iter().enumerate() {
        debug!("{} {} {:?}", p.column, p.model.path, p.offsets);
        match acvd_render::rigged_model(&garage.usrdir, p.model.path, &|root| p.offset(root)) {
            Ok((meshes, rig)) => loaded.push(pose::Loaded { index, placement: p, meshes, rig }),
            Err(e) => problems.push(format!("{}: {e:#}", p.model.path)),
        }
    }
    let rig = match pose::build(&garage.usrdir, &loaded) {
        Ok(rig) => rig,
        Err(e) => {
            problems.push(format!("motion: {e:#}"));
            return;
        }
    };
    let skins = pose::spawn(&mut commands, ac, &rig, &loaded);
    for (part, (joints, binds)) in loaded.into_iter().zip(skins) {
        let inverse_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds));
        if let Some(h) = weapons::hardpoint(part.placement) {
            if let Some(&root) = joints.first() {
                commands.entity(root).insert(h);
            }
        }
        sfx::effect_points(&mut commands, &part.rig, &joints, part.placement.column);
        for mesh in part.meshes {
            min = min.min(Vec3::from(mesh.min));
            max = max.max(Vec3::from(mesh.max));
            let material = materials.add(acvd_render::material(&garage.usrdir, &mesh, &mut packs, &mut images, garage.flat, &mut missing));
            commands.spawn((
                Mesh3d(meshes.add(mesh.mesh)),
                MeshMaterial3d(material),
                SkinnedMesh { inverse_bindposes: inverse_bindposes.clone(), joints: joints.clone() },
                NoFrustumCulling,
                Transform::default(),
                ChildOf(ac),
            ));
        }
    }
    match rig.motion {
        Some(mut motion) => {
            if let Some(name) = garage.clip.take() {
                match motion.clips.iter().position(|a| a.entry == name) {
                    Some(i) => {
                        if let Err(e) = motion.select(&garage.usrdir, i) {
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

    let mut status = format!("[{}/{}] design {} {} - {} models", garage.current + 1, garage.designs.len(), design.id, design.name, built.placements.len());
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
    let legs_motion_id = acvd_data::part_field(design.data.legs as i64, 3, "legs_motion_id") as u8;
    let mut pilot = control::Pilot::new(ctrl, legs_motion_id);
    if spawn_y.is_none() && !collision.hits.is_empty() {
        pilot.airborne = true;
    }
    commands.entity(ac).insert((pilot, weapons::Armament::from_design(&design.data)));
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
        text::spawn(&mut commands, &hud.font, &hud_lines(design, &hud.parts), Vec2::new(24.0, 20.0), 2.0, Color::srgb(0.92, 0.95, 0.98));
    }
    if let Some(mut shot) = shot {
        shot.ready = true;
    }
}

fn hud_lines(design: &Row<AcAssemblyDesignSt>, parts: &Fmg) -> String {
    let mut text = format!("{} {}", design.id, design.name);
    let names: Vec<&str> = [design.data.head, design.data.core, design.data.legs].into_iter().filter_map(|id| parts.get(id as i32)).collect();
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
    let step = [(KeyCode::ArrowDown, 1), (KeyCode::ArrowUp, -1)].iter().filter(|(k, _)| keys.just_pressed(*k)).map(|(_, s)| *s).sum::<isize>();
    let n = m.clips.len() as isize;
    let mut index = m.index;
    for _ in 0..n.max(1) {
        if step == 0 || n == 0 {
            break;
        }
        index = (index as isize + step).rem_euclid(n) as usize;
        match m.select(&garage.usrdir, index) {
            Ok(()) => break,
            Err(e) => warn!("{e:#}"),
        }
    }
    if step != 0 || m.is_added() {
        let title = format!("{} - {} ({} frames)", garage.status, m.name(), m.clip.frames);
        info!("{title}");
        if let Ok(mut w) = window.single_mut() {
            w.title = title;
        }
    }
}
