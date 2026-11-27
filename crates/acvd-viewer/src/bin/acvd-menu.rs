//! Menu layout viewer: draws one DRB dialog with its disc textures.
//!
//! `acvd-menu [layout] [dialog] [--lang en] [--disc <360 ISO>] [--shot <png>] [--placeholders] [--atlas <texture>] [--kit]`
//! `layout` is a `lang/<lang>/menu/` name (`staffroll`) or a full `.drb.dcx` asset path; `dialog`
//! is a dialog name, defaulting to the first one no other dialog nests. `--placeholders` shows
//! texts the game fills at runtime as their object names. `--atlas` draws one of the layout's
//! textures at native size instead (to read texel rects). `--kit` previews [`acvd_render::ui_kit::AcvdUiKit`].
//! Left/Right: previous/next dialog, Up/Down: previous/next layout in the folder.

use std::path::PathBuf;

use acvd_formats::vfs::{self, Disc};
use acvd_render::app::{take_shot, Shot};
use acvd_render::menu::{self, Layout, MenuRoot};
use acvd_render::ui_kit::{
    palette, AcvdUiKit, ArrowDir, ButtonState, FontRole, PanelStyle, StatDelta, TraceStyle,
};
use bevy::prelude::*;
use bevy::ui::px;
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
    kit_mode: bool,
    kit: Option<AcvdUiKit>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut positional, mut lang, mut disc, mut shot, mut placeholders, mut atlas, mut kit_mode) =
        (Vec::new(), "en".to_string(), None, None, false, None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lang" => lang = args.next().unwrap_or(lang),
            "--disc" => disc = args.next().map(PathBuf::from),
            "--shot" => shot = args.next().map(|p| Shot::new(PathBuf::from(p))),
            "--placeholders" => placeholders = true,
            "--atlas" => atlas = args.next(),
            "--kit" => kit_mode = true,
            _ => positional.push(a),
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let disc =
        Disc::open(&disc.unwrap_or_else(|| vfs::default_disc(&root))).expect("opening the disc");
    let folder = format!("lang/{lang}/menu");
    let mut layouts: Vec<String> = disc
        .list(&folder)
        .into_iter()
        .filter(|n| n.ends_with(".drb.dcx"))
        .collect();
    let wanted = positional
        .first()
        .cloned()
        .unwrap_or_else(|| "staffroll".into());
    let path = if wanted.ends_with(".drb.dcx") {
        wanted.clone()
    } else {
        format!("{folder}/{wanted}.drb.dcx")
    };
    let current = layouts
        .iter()
        .position(|l| l.eq_ignore_ascii_case(&path))
        .unwrap_or_else(|| {
            layouts.insert(0, path);
            0
        });

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "acvd-menu".into(),
            resolution: WindowResolution::new(1280, 720),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(menu::MenuPlugin)
    .insert_resource(ClearColor(Color::srgb(0.05, 0.06, 0.07)))
    .insert_resource(Viewer {
        disc,
        layouts,
        current,
        dialog: 0,
        wanted_dialog: positional.get(1).cloned(),
        shown: None,
        layout: None,
        placeholders,
        atlas,
        kit_mode,
        kit: None,
    })
    .add_systems(Startup, |mut commands: Commands| {
        commands.spawn(Camera2d);
    })
    .add_systems(Update, (browse, show).chain());
    if let Some(shot) = shot {
        app.insert_resource(shot)
            .add_systems(Update, take_shot.after(show));
    }
    app.run();
}

fn browse(keys: Res<ButtonInput<KeyCode>>, mut viewer: ResMut<Viewer>) {
    let step =
        |a: KeyCode, b: KeyCode| keys.just_pressed(b) as isize - keys.just_pressed(a) as isize;
    let layout_step = step(KeyCode::ArrowUp, KeyCode::ArrowDown);
    if layout_step != 0 {
        let n = viewer.layouts.len() as isize;
        viewer.current = (viewer.current as isize + layout_step).rem_euclid(n) as usize;
        viewer.dialog = 0;
        viewer.layout = None;
    }
    let dialog_step = step(KeyCode::ArrowLeft, KeyCode::ArrowRight);
    if dialog_step != 0 {
        let n = viewer
            .layout
            .as_ref()
            .map_or(1, |l| l.drb.dialogs.len().max(1)) as isize;
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
    let Ok(mut window) = window.single_mut() else {
        return;
    };
    let size = Vec2::new(window.width(), window.height());
    if viewer.kit_mode {
        let key = Some((usize::MAX, 0, [size.x as u32, size.y as u32]));
        if viewer.shown == key {
            return;
        }
        viewer.shown = key;
        if viewer.kit.is_none() {
            viewer.kit = Some(AcvdUiKit::load(&viewer.disc, &mut images).expect("load AcvdUiKit"));
        }
        for e in &roots {
            commands.entity(e).despawn();
        }
        let kit = viewer.kit.as_ref().unwrap();
        let scale = (size.x / 1280.0).min(size.y / 720.0).max(0.5);
        spawn_kit_showcase(&mut commands, kit, scale);
        window.title = "acvd-menu --kit (AcvdUiKit scalable showcase)".into();
        if let Some(mut shot) = shot {
            shot.ready = true;
        }
        return;
    }
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
    let key = Some((
        viewer.current,
        viewer.dialog,
        [size.x as u32, size.y as u32],
    ));
    if viewer.shown == key {
        return;
    }
    viewer.shown = key;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let layout = viewer.layout.as_ref().unwrap();
    if let Some(name) = &viewer.atlas {
        let found = layout
            .drb
            .textures
            .iter()
            .position(|t| &t.name == name)
            .and_then(|i| layout.textures[i].clone());
        let Some(image) = found else {
            eprintln!(
                "no texture `{name}`; layout has {:?}",
                layout
                    .drb
                    .textures
                    .iter()
                    .map(|t| &t.name)
                    .collect::<Vec<_>>()
            );
            return;
        };
        commands.spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                ..default()
            },
            ImageNode::new(image),
        ));
        window.title = format!("{} - texture {name}", viewer.layouts[viewer.current]);
        if let Some(mut shot) = shot {
            shot.ready = true;
        }
        return;
    }
    let Some(dialog) = layout.drb.dialogs.get(viewer.dialog) else {
        return;
    };
    let scale = (size.x / 1280.0).min(size.y / 720.0);
    let dsize = Vec2::new(dialog.size[0] as f32, dialog.size[1] as f32) * scale;
    menu::spawn(
        &mut commands,
        layout,
        viewer.dialog,
        ((size - dsize) / 2.0).max(Vec2::ZERO),
        scale,
    );
    window.title = format!(
        "{} - [{}/{}] {} ({}x{}, {} objects)",
        viewer.layouts[viewer.current],
        viewer.dialog + 1,
        layout.drb.dialogs.len(),
        dialog.name,
        dialog.size[0],
        dialog.size[1],
        dialog.objects.len()
    );
    info!("{}", window.title);
    if let Some(mut shot) = shot {
        shot.ready = true;
    }
}

fn spawn_kit_showcase(commands: &mut Commands, kit: &AcvdUiKit, scale: f32) {
    let root = commands
        .spawn((
            MenuRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
        ))
        .id();

    // Top bar + stepped trace
    let top_wrap = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    let top_bar = kit.spawn_panel(
        commands,
        Some(top_wrap),
        PanelStyle::Bar,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((20.0 * scale).round()), px((8.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        },
        scale,
    );
    kit.spawn_text(
        commands,
        top_bar,
        FontRole::Header,
        "ACVD // AC DESIGN PRESETS",
        scale,
        palette::TEXT_PRIMARY,
    );
    let top_right = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((10.0 * scale).round()),
                ..default()
            },
            ChildOf(top_bar),
        ))
        .id();
    kit.spawn_badge(
        commands,
        top_right,
        "ALL TYPES",
        palette::ACCENT_TEAL,
        true,
        scale,
    );
    kit.spawn_badge(commands, top_right, "KE", palette::TEXT_MUTED, false, scale);
    kit.spawn_badge(commands, top_right, "CE", palette::TEXT_MUTED, false, scale);
    kit.spawn_badge(commands, top_right, "TE", palette::TEXT_MUTED, false, scale);
    kit.spawn_trace(commands, top_wrap, TraceStyle::Stepped, scale);

    // Main middle row: Left Tactical Presets Panel | Right Framed AC Specs + Gauges Panel
    let mid = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                padding: UiRect::all(px((22.0 * scale).round())),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::FlexStart,
                column_gap: px((24.0 * scale).round()),
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    // Left: Tactical Panel with corner reticles, header, all 4 button states, paging arrows
    let left_panel = kit.spawn_panel(
        commands,
        Some(mid),
        PanelStyle::Tactical,
        Node {
            width: px((360.0 * scale).round()),
            padding: UiRect::all(px((14.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((6.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_header(
        commands,
        left_panel,
        "PRESET DESIGNS",
        Some("67 / 118"),
        scale,
    );

    let rows = [
        ("6260 NTAC_AC", Some("CLR 6260"), ButtonState::Normal),
        ("6301 ch0101", Some("CLR 6301"), ButtonState::Normal),
        ("6303 ch0204", Some("ACTIVE"), ButtonState::Active),
        ("6306 ch0402_A", Some("CLR 6306"), ButtonState::Normal),
        ("6312 ch0601", Some("CLR 6312"), ButtonState::Selected),
        ("6315 LOCKED_UNIT", Some("LOCKED"), ButtonState::Disabled),
    ];
    for (name, tag, st) in rows {
        kit.spawn_button(commands, left_panel, name, tag, st, scale);
    }

    kit.spawn_trace(commands, left_panel, TraceStyle::Simple, scale);

    let pager = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
            ChildOf(left_panel),
        ))
        .id();
    let prev_box = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((6.0 * scale).round()),
                ..default()
            },
            ChildOf(pager),
        ))
        .id();
    kit.spawn_arrow(
        commands,
        prev_box,
        ArrowDir::Left,
        scale,
        palette::TEXT_PRIMARY,
    );
    kit.spawn_text(
        commands,
        prev_box,
        FontRole::Small,
        "PREV",
        scale,
        palette::TEXT_PRIMARY,
    );
    kit.spawn_text(
        commands,
        pager,
        FontRole::Small,
        "55-67 / 118",
        scale,
        palette::TEXT_MUTED,
    );
    let next_box = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((6.0 * scale).round()),
                ..default()
            },
            ChildOf(pager),
        ))
        .id();
    kit.spawn_text(
        commands,
        next_box,
        FontRole::Small,
        "NEXT",
        scale,
        palette::TEXT_PRIMARY,
    );
    kit.spawn_arrow(
        commands,
        next_box,
        ArrowDir::Right,
        scale,
        palette::TEXT_PRIMARY,
    );

    // Right: Framed Panel (`Shop_Base1` corner brackets) with StatRows + Gauges
    let right_panel = kit.spawn_panel(
        commands,
        Some(mid),
        PanelStyle::Framed,
        Node {
            width: px((420.0 * scale).round()),
            padding: UiRect::all(px((14.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((4.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_header(
        commands,
        right_panel,
        "AC SPECS",
        Some("AcCtrlParam"),
        scale,
    );

    let stats: [(&str, &str, Option<StatDelta<'_>>); 8] = [
        (
            "AP",
            "35,582",
            Some(StatDelta {
                text: "38,140",
                cmp: 1,
            }),
        ),
        (
            "KE Defense",
            "1,659",
            Some(StatDelta {
                text: "1,860",
                cmp: 1,
            }),
        ),
        (
            "CE Defense",
            "685",
            Some(StatDelta {
                text: "505",
                cmp: -1,
            }),
        ),
        (
            "TE Defense",
            "496",
            Some(StatDelta {
                text: "496",
                cmp: 0,
            }),
        ),
        (
            "Boost Speed",
            "177 km/h",
            Some(StatDelta {
                text: "192 km/h",
                cmp: 1,
            }),
        ),
        (
            "HB Speed",
            "371 km/h",
            Some(StatDelta {
                text: "358 km/h",
                cmp: -1,
            }),
        ),
        (
            "EN Recovery",
            "10,218",
            Some(StatDelta {
                text: "12,170",
                cmp: 1,
            }),
        ),
        (
            "Stability",
            "918",
            Some(StatDelta {
                text: "918",
                cmp: 0,
            }),
        ),
    ];
    for (i, (label, val, delta)) in stats.into_iter().enumerate() {
        kit.spawn_stat_row(commands, right_panel, label, val, delta, i % 2 == 0, scale);
    }

    kit.spawn_trace(commands, right_panel, TraceStyle::Simple, scale);

    // Subtle inset card for retail Load / Energy gauges
    let gauge_card = kit.spawn_panel(
        commands,
        Some(right_panel),
        PanelStyle::Subtle,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::all(px((10.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((8.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_gauge(commands, gauge_card, "Load", 0.72, Some(0.85), false, scale);
    kit.spawn_gauge(
        commands,
        gauge_card,
        "Energy",
        0.94,
        Some(0.80),
        true,
        scale,
    );

    // Bottom helpline bar
    let bot_bar = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Bar,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((20.0 * scale).round()), px((8.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        },
        scale,
    );
    kit.spawn_text(
        commands,
        bot_bar,
        FontRole::Body,
        "W/S: Navigate   |   Enter: Select   |   Esc: Back   |   Right-Drag: Orbit",
        scale,
        palette::TEXT_PRIMARY,
    );
    kit.spawn_badge(
        commands,
        bot_bar,
        "UI KIT PHASE 1",
        palette::SQUARE_MARK,
        false,
        scale,
    );
}
