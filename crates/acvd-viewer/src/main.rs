//! Model viewer: browses every FLVER in the generated model index, read straight off the
//! owned disc.
//!
//! `acvd-viewer [model name or asset path] [--disc <360 ISO or dump root>] [--shot <png>] [--flat]`
//! Left/Right: previous/next model, PageUp/PageDown: jump 50, drag left mouse: orbit,
//! wheel: zoom, R: reframe. `--shot` saves one frame of the first model and exits.

use std::path::PathBuf;

use acvd_data::ModelRef;
use acvd_formats::vfs::{self, Disc};
use acvd_render::app::{orbit, take_shot, Orbit, Shot};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

#[derive(Resource)]
struct Viewer {
    disc: Disc,
    models: Vec<&'static ModelRef>,
    current: usize,
    shown: Option<usize>,
    flat: bool,
    bounds: (Vec3, Vec3),
}

#[derive(Component)]
struct ModelPart;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut wanted, mut disc, mut shot, mut flat) = (None, None, None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--disc" => disc = args.next().map(PathBuf::from),
            "--shot" => shot = args.next().map(|p| Shot::new(PathBuf::from(p))),
            "--flat" => flat = true,
            _ => wanted = Some(a),
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let disc = Disc::open(&disc.unwrap_or_else(|| vfs::default_disc(&root))).expect("opening the disc");
    let models: Vec<&'static ModelRef> = acvd_data::generated::models::ALL.iter().flat_map(|g| g.iter()).collect();
    let wanted = wanted.unwrap_or_else(|| "am0010.flv".into());
    let current = models
        .iter()
        .position(|m| m.path.eq_ignore_ascii_case(&wanted) || m.path.rsplit('|').next().is_some_and(|n| n.eq_ignore_ascii_case(&wanted)))
        .unwrap_or_else(|| {
            eprintln!("no model `{wanted}`; starting at the first");
            0
        });

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: "acvd-viewer".into(), ..default() }),
        ..default()
    }))
    .insert_resource(ClearColor(Color::srgb(0.11, 0.12, 0.14)))
    .insert_resource(Viewer { disc, models, current, shown: None, flat, bounds: (Vec3::ZERO, Vec3::ONE) })
    .add_systems(Startup, setup)
    .add_systems(Update, (browse, show, orbit).chain());
    if let Some(shot) = shot {
        app.insert_resource(shot).add_systems(Update, take_shot.after(show));
    }
    app.run();
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera3d::default(), Transform::default(), Orbit::new(0.6, 0.3)));
    commands.spawn((DirectionalLight { illuminance: 9000.0, ..default() }, Transform::from_xyz(4.0, 8.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y)));
    commands.spawn((DirectionalLight { illuminance: 3000.0, ..default() }, Transform::from_xyz(-6.0, 2.0, -4.0).looking_at(Vec3::ZERO, Vec3::Y)));
}

fn browse(keys: Res<ButtonInput<KeyCode>>, mut viewer: ResMut<Viewer>) {
    let n = viewer.models.len() as isize;
    let step = [(KeyCode::ArrowRight, 1), (KeyCode::ArrowLeft, -1), (KeyCode::PageDown, 50), (KeyCode::PageUp, -50)]
        .iter()
        .filter(|(k, _)| keys.just_pressed(*k))
        .map(|(_, s)| *s)
        .sum::<isize>();
    if step != 0 && n > 0 {
        viewer.current = (viewer.current as isize + step).rem_euclid(n) as usize;
    }
}

#[allow(clippy::too_many_arguments)]
fn show(
    mut commands: Commands,
    mut viewer: ResMut<Viewer>,
    keys: Res<ButtonInput<KeyCode>>,
    parts: Query<Entity, With<ModelPart>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut orbit: Query<&mut Orbit>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    shot: Option<ResMut<Shot>>,
) {
    if viewer.shown == Some(viewer.current) {
        if keys.just_pressed(KeyCode::KeyR) {
            if let Ok(mut o) = orbit.single_mut() {
                o.frame(viewer.bounds.0, viewer.bounds.1);
            }
        }
        return;
    }
    let Some(&model) = viewer.models.get(viewer.current) else { return };
    viewer.shown = Some(viewer.current);
    for e in &parts {
        commands.entity(e).despawn();
    }

    let mut status = format!("[{}/{}] {} - {} meshes, {} triangles", viewer.current + 1, viewer.models.len(), model.path, model.meshes, model.triangles);
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    match acvd_render::model(&viewer.disc, model.path) {
        Ok(list) => {
            let mut packs = acvd_render::Packs::default();
            let mut missing = 0;
            for part in list {
                min = min.min(Vec3::from(part.min));
                max = max.max(Vec3::from(part.max));
                let material = materials.add(acvd_render::material(&viewer.disc, &part, &mut packs, &mut images, viewer.flat, &mut missing));
                commands.spawn((Mesh3d(meshes.add(part.mesh)), MeshMaterial3d(material), Transform::default(), ModelPart));
            }
            if missing > 0 {
                status.push_str(&format!(" ({missing} textures missing)"));
            }
        }
        Err(e) => {
            status.push_str(&format!(" - failed: {e:#}"));
            eprintln!("{status}");
        }
    }
    if min.x <= max.x {
        viewer.bounds = (min, max);
        if let Ok(mut o) = orbit.single_mut() {
            o.frame(min, max);
        }
    }
    info!("{status}");
    if let Ok(mut w) = window.single_mut() {
        w.title = status;
    }
    if let Some(mut shot) = shot {
        shot.ready = true;
    }
}
