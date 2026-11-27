//! Front-end boot flow: the 360 press-start screen (`lang/jp/menu/gameboot.drb.dcx`, localized
//! through `lang/<lang>/text/menu/menu.fmg`), then the opening movie, then the 3D garage.
//!
//! Xenia boot capture (`tools/xenia/recipes/boot_passive.txt`): logos for ~18 s, the press-start
//! screen at ~20 s, and the scene fires no `AppMenuMoviePlayer` call. `menu.fmg` id 204 is
//! `PRESS START`, 205 `MAIN SYSTEM IS READY TO ACTIVATE`, 5 the title, and the `Data` runtime
//! text is the console clock (`HH:MM:SS MM/DD/YYYY`). The rewrite plays `prologue` after START
//! instead of the 360's Xbox LIVE caution.

use std::path::PathBuf;

use acvd_formats::vfs::Disc;
use acvd_render::app::Shot;
use acvd_render::menu::{self, MenuObject, MenuRoot, MenuText};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::movie;
use crate::sound;

pub const PRESS_START_LAYOUT: &str = "lang/jp/menu/gameboot.drb.dcx";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootStage {
    Title,
    Movie,
    Done,
}

#[derive(Resource)]
pub struct BootConfig {
    pub enabled: bool,
    pub disc: Disc,
    /// `menu.fmg` language (`en` by default; the press-start layout only ships as `jp`).
    pub lang: String,
    /// Movie cache name under `movies_dir` (`prologue`).
    pub movie: String,
    pub movies_dir: PathBuf,
    /// `--boot-stage movie` / `--movie-frame` start directly on the movie.
    pub start: BootStage,
    /// `--movie-frame`: seek to this frame, pause, no audio (deterministic `--shot`).
    pub frame: Option<u32>,
    /// `--no-movie`: START goes straight to the garage.
    pub skip_movie: bool,
    /// `--boot-auto <s>`: press START automatically (unattended verification).
    pub auto_start: Option<f32>,
}

/// Set when the deferred 3D world may spawn. Always true when the boot flow is off.
#[derive(Resource)]
pub struct WorldReady(pub bool);

#[derive(Resource)]
pub struct Boot {
    stage: Option<BootStage>,
    timer: f32,
    timestamp_timer: f32,
}

impl Default for Boot {
    fn default() -> Self {
        Self {
            stage: None,
            timer: 0.0,
            timestamp_timer: 0.0,
        }
    }
}

pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Boot>()
            .add_systems(Startup, spawn_frontend.run_if(boot_on))
            .add_systems(
                Update,
                (movie::update, tick, finish).chain().run_if(boot_on),
            );
    }
}

fn boot_on(cfg: Res<BootConfig>) -> bool {
    cfg.enabled
}

fn spawn_frontend(
    mut commands: Commands,
    cfg: Res<BootConfig>,
    mut boot: ResMut<Boot>,
    mut images: ResMut<Assets<Image>>,
    mut audio: ResMut<Assets<AudioSource>>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    commands.spawn((
        movie::FrontendCamera,
        Camera2d,
        Camera {
            order: 10,
            clear_color: bevy::camera::ClearColorConfig::Custom(Color::BLACK),
            ..default()
        },
    ));
    let stage = if cfg.frame.is_some() {
        BootStage::Movie
    } else {
        cfg.start
    };
    boot.stage = Some(stage);
    match stage {
        BootStage::Movie => start_movie(&mut commands, &cfg, &mut boot, &mut images, &mut audio),
        BootStage::Done => boot.stage = Some(BootStage::Done),
        BootStage::Title => show_title(&mut commands, &cfg, &mut boot, &mut images, &windows),
    }
}

fn show_title(
    commands: &mut Commands,
    cfg: &BootConfig,
    boot: &mut Boot,
    images: &mut Assets<Image>,
    windows: &Query<&Window, With<PrimaryWindow>>,
) {
    let layout = match menu::load_in_lang(&cfg.disc, PRESS_START_LAYOUT, &cfg.lang, images) {
        Ok(layout) => layout,
        Err(e) => {
            eprintln!("boot: {e:#}");
            boot.stage = Some(BootStage::Done);
            return;
        }
    };
    let Ok(window) = windows.single() else {
        boot.stage = Some(BootStage::Done);
        return;
    };
    let scale = (window.width() / 1280.0)
        .min(window.height() / 720.0)
        .max(0.1);
    let origin = ((Vec2::new(window.width(), window.height()) - Vec2::new(1280.0, 720.0) * scale)
        / 2.0)
        .max(Vec2::ZERO);
    for name in ["@BackImage", "@PressStart"] {
        match layout.drb.dialogs.iter().position(|d| d.name == name) {
            Some(i) => {
                menu::spawn(commands, &layout, i, origin, scale);
            }
            None => eprintln!("boot: {PRESS_START_LAYOUT} has no dialog `{name}`"),
        }
    }
    boot.stage = Some(BootStage::Title);
    boot.timer = 0.0;
}

fn start_movie(
    commands: &mut Commands,
    cfg: &BootConfig,
    boot: &mut Boot,
    images: &mut Assets<Image>,
    audio: &mut Assets<AudioSource>,
) {
    match movie::start(
        commands,
        images,
        audio,
        &cfg.movies_dir,
        &cfg.movie,
        cfg.frame,
    ) {
        Ok(player) => commands.insert_resource(player),
        Err(e) => {
            eprintln!("boot: {e:#}");
            let cached = movie::cached_movies(&cfg.movies_dir);
            if !cached.is_empty() {
                eprintln!("boot: cached movies: {cached:?}");
            }
            boot.stage = Some(BootStage::Done);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tick(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    cfg: Res<BootConfig>,
    mut boot: ResMut<Boot>,
    roots: Query<Entity, With<MenuRoot>>,
    mut texts: Query<(&MenuObject, &mut MenuText)>,
    player: Option<Res<movie::Player>>,
    shot: Option<ResMut<Shot>>,
    mut images: ResMut<Assets<Image>>,
    mut audio: ResMut<Assets<AudioSource>>,
) {
    let Some(stage) = boot.stage else { return };
    boot.timer += time.delta_secs();
    let start_button = keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::NumpadEnter)
        || gamepads.iter().any(|g| {
            g.just_pressed(bevy::input::gamepad::GamepadButton::Start)
                || g.just_pressed(bevy::input::gamepad::GamepadButton::South)
        })
        || cfg.auto_start.is_some_and(|t| boot.timer >= t);
    match stage {
        BootStage::Title => {
            boot.timestamp_timer += time.delta_secs();
            if boot.timestamp_timer >= 0.5 {
                boot.timestamp_timer = 0.0;
                let stamp = timestamp();
                for (object, mut text) in &mut texts {
                    if object.0 == "Data" && text.text != stamp {
                        text.text = stamp.clone();
                    }
                }
            }
            if let Some(mut shot) = shot {
                if boot.timer >= 0.6 && !shot.ready {
                    shot.ready = true;
                }
            }
            if start_button {
                for e in &roots {
                    commands.entity(e).despawn();
                }
                if cfg.skip_movie {
                    boot.stage = Some(BootStage::Done);
                } else {
                    boot.stage = Some(BootStage::Movie);
                    boot.timer = 0.0;
                    start_movie(&mut commands, &cfg, &mut boot, &mut images, &mut audio);
                }
            }
        }
        BootStage::Movie => {
            if boot.timer >= 0.6 {
                if let Some(mut shot) = shot {
                    shot.ready = true;
                }
            }
            let finished = player.as_ref().is_some_and(|p| p.finished());
            if finished {
                if let Some(p) = player.as_ref() {
                    movie::despawn(&mut commands, p);
                }
                commands.remove_resource::<movie::Player>();
                boot.stage = Some(BootStage::Done);
            }
        }
        BootStage::Done => {}
    }
}

/// Tears the front end down exactly once and releases the deferred 3D world.
fn finish(
    mut commands: Commands,
    cfg: Res<BootConfig>,
    mut boot: ResMut<Boot>,
    roots: Query<Entity, With<MenuRoot>>,
    cameras: Query<Entity, With<movie::FrontendCamera>>,
    mut world: ResMut<WorldReady>,
    mut bgm: ResMut<sound::BgmState>,
    bgm_players: Query<Entity, With<sound::BgmPlayer>>,
    mut clear: ResMut<ClearColor>,
    shot: Option<Res<Shot>>,
) {
    if boot.stage != Some(BootStage::Done) {
        return;
    }
    for e in &roots {
        commands.entity(e).despawn();
    }
    for e in &cameras {
        commands.entity(e).despawn();
    }
    commands.remove_resource::<movie::Player>();
    *clear = ClearColor(Color::srgb(0.32, 0.36, 0.42));
    if shot.is_none() {
        bgm.enable();
        sound::switch_bgm(&mut commands, &mut bgm, &bgm_players, &cfg.disc, None);
    }
    world.0 = true;
    boot.stage = None;
}

fn timestamp() -> String {
    let Some((y, mo, d, h, mi, s)) = local_now() else {
        return "00:00:00 00/00/0000".into();
    };
    format!("{h:02}:{mi:02}:{s:02} {mo:02}/{d:02}/{y:04}")
}

#[cfg(windows)]
fn local_now() -> Option<(u16, u16, u16, u16, u16, u16)> {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }
    let mut t = SystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    // SAFETY: `GetLocalTime` writes a `SYSTEMTIME` into the storage we pass.
    unsafe { GetLocalTime(&mut t) };
    Some((t.year, t.month, t.day, t.hour, t.minute, t.second))
}

#[cfg(not(windows))]
fn local_now() -> Option<(u16, u16, u16, u16, u16, u16)> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (days, rem) = (secs / 86400, secs % 86400);
    let (mut year, mut day) = (1970i64, days as i64);
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let len = if leap { 366 } else { 365 };
        if day < len {
            break;
        }
        day -= len;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0usize;
    while day >= months[month] {
        day -= months[month];
        month += 1;
    }
    Some((
        year as u16,
        month as u16 + 1,
        day as u16 + 1,
        (rem / 3600) as u16,
        ((rem % 3600) / 60) as u16,
        (rem % 60) as u16,
    ))
}
