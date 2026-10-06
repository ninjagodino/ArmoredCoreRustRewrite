//! Menu layout viewer: draws one DRB dialog with its disc textures.
//!
//! `acvd-menu [layout] [dialog] [--lang en] [--disc <360 ISO or dump root>] [--shot <png>] [--placeholders] [--atlas <texture>]`
//! `layout` is a `lang/<lang>/menu/` name (`staffroll`) or a full `.drb.dcx` asset path; `dialog`
//! is a dialog name, defaulting to the first one no other dialog nests. `--placeholders` shows
//! texts the game fills at runtime as their object names. `--atlas` draws one of the layout's
//! textures at native size instead (to read texel rects).
//! Left/Right: previous/next dialog, Up/Down: previous/next layout in the folder.

use std::path::PathBuf;

use acvd_formats::vfs::{self, Disc};
use acvd_render::app::{take_shot, Shot};
use acvd_render::menu::{self, Layout, MenuRoot};
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowResolution};

#[derive(Resource)]
struct Viewer {
    disc: Disc,
    layouts: Vec<String>,
    current: usize,
    dialog: usize,
    wanted_dialog: Option<String>,
    shown: Option<(usize, usize, [u32; 2])>,
    layout: Option<Layout>,
    placeholders: bool,
    atlas: Option<String>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut positional, mut lang, mut disc, mut shot, mut placeholders, mut atlas) = (Vec::new(), "en".to_string(), None, None, false, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lang" => lang = args.next().unwrap_or(lang),
            "--disc" => disc = args.next().map(PathBuf::from),
            "--shot" => shot = args.next().map(|p| Shot::new(PathBuf::from(p))),
            "--placeholders" => placeholders = true,
            "--atlas" => atlas = args.next(),
            _ => positional.push(a),
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let disc = Disc::open(&disc.unwrap_or_else(|| vfs::default_disc(&root))).expect("opening the disc");
    let folder = format!("lang/{lang}/menu");
    let mut layouts: Vec<String> = disc.list(&folder).into_iter().filter(|n| n.ends_with(".drb.dcx")).collect();
    let wanted = positional.first().cloned().unwrap_or_else(|| "staffroll".into());
    let path = if wanted.ends_with(".drb.dcx") { wanted.clone() } else { format!("{folder}/{wanted}.drb.dcx") };
    let current = layouts.iter().position(|l| l.eq_ignore_ascii_case(&path)).unwrap_or_else(|| {
        layouts.insert(0, path);
        0
    });

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: "acvd-menu".into(), resolution: WindowResolution::new(1280, 720), ..default() }),
        ..default()
    }))
    .add_plugins(menu::MenuPlugin)
    .insert_resource(ClearColor(Color::srgb(0.05, 0.06, 0.07)))
    .insert_resource(Viewer { disc, layouts, current, dialog: 0, wanted_dialog: positional.get(1).cloned(), shown: None, layout: None, placeholders, atlas })
    .add_systems(Startup, |mut commands: Commands| {
        commands.spawn(Camera2d);
    })
    .add_systems(Update, (browse, show).chain());
    if let Some(shot) = shot {
        app.insert_resource(shot).add_systems(Update, take_shot.after(show));
    }
    app.run();
}

fn browse(keys: Res<ButtonInput<KeyCode>>, mut viewer: ResMut<Viewer>) {
    let step = |a: KeyCode, b: KeyCode| keys.just_pressed(b) as isize - keys.just_pressed(a) as isize;
    let layout_step = step(KeyCode::ArrowUp, KeyCode::ArrowDown);
    if layout_step != 0 {
        let n = viewer.layouts.len() as isize;
        viewer.current = (viewer.current as isize + layout_step).rem_euclid(n) as usize;
        viewer.dialog = 0;
        viewer.layout = None;
    }
    let dialog_step = step(KeyCode::ArrowLeft, KeyCode::ArrowRight);
    if dialog_step != 0 {
        let n = viewer.layout.as_ref().map_or(1, |l| l.drb.dialogs.len().max(1)) as isize;
        viewer.dialog = (viewer.dialog as isize + dialog_step).rem_euclid(n) as usize;
    }
}

fn show(
    mut commands: Commands,
    mut viewer: ResMut<Viewer>,
    roots: Query<Entity, With<MenuRoot>>,
    mut images: ResMut<Assets<Image>>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
    shot: Option<ResMut<Shot>>,
) {
    let Ok(mut window) = window.single_mut() else { return };
    let size = Vec2::new(window.width(), window.height());
    if viewer.layout.is_none() {
        let path = viewer.layouts[viewer.current].clone();
        match menu::load(&viewer.disc, &path, &mut images) {
            Ok(mut layout) => {
                layout.placeholders = viewer.placeholders;
                let wanted = viewer.wanted_dialog.take();
                viewer.dialog = wanted
                    .and_then(|w| layout.drb.dialogs.iter().position(|d| d.name == w))
                    .or_else(|| layout.drb.roots().next().map(|(i, _)| i))
                    .unwrap_or(0);
                viewer.layout = Some(layout);
            }
            Err(e) => {
                window.title = format!("{path}: {e:#}");
                eprintln!("{}", window.title);
                viewer.shown = Some((viewer.current, viewer.dialog, [0, 0]));
                return;
            }
        }
    }
    let key = Some((viewer.current, viewer.dialog, [size.x as u32, size.y as u32]));
    if viewer.shown == key {
        return;
    }
    viewer.shown = key;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let layout = viewer.layout.as_ref().unwrap();
    if let Some(name) = &viewer.atlas {
        let found = layout.drb.textures.iter().position(|t| &t.name == name).and_then(|i| layout.textures[i].clone());
        let Some(image) = found else {
            eprintln!("no texture `{name}`; layout has {:?}", layout.drb.textures.iter().map(|t| &t.name).collect::<Vec<_>>());
            return;
        };
        commands.spawn((MenuRoot, Node { position_type: PositionType::Absolute, ..default() }, ImageNode::new(image)));
        window.title = format!("{} - texture {name}", viewer.layouts[viewer.current]);
        if let Some(mut shot) = shot {
            shot.ready = true;
        }
        return;
    }
    let Some(dialog) = layout.drb.dialogs.get(viewer.dialog) else { return };
    let scale = (size.x / 1280.0).min(size.y / 720.0);
    let dsize = Vec2::new(dialog.size[0] as f32, dialog.size[1] as f32) * scale;
    menu::spawn(&mut commands, layout, viewer.dialog, ((size - dsize) / 2.0).max(Vec2::ZERO), scale);
    window.title = format!("{} - [{}/{}] {} ({}x{}, {} objects)", viewer.layouts[viewer.current], viewer.dialog + 1, layout.drb.dialogs.len(), dialog.name, dialog.size[0], dialog.size[1], dialog.objects.len());
    info!("{}", window.title);
    if let Some(mut shot) = shot {
        shot.ready = true;
    }
}
