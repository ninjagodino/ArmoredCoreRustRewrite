//! Bevy pieces the viewer and the game share: an orbit camera and a one-frame screenshot mode.

use std::path::PathBuf;

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

/// Drag the left mouse button to orbit `focus`, scroll to zoom.
#[derive(Component)]
pub struct Orbit {
    pub focus: Vec3,
    pub radius: f32,
    pub yaw: f32,
    pub pitch: f32,
    /// The game crate writes the camera from `sheets/camera_follow.csv`; skip mouse orbit.
    pub follow: bool,
}

impl Orbit {
    pub fn new(yaw: f32, pitch: f32) -> Self {
        Self { focus: Vec3::ZERO, radius: 5.0, yaw, pitch, follow: false }
    }

    /// Centres on the box and backs off far enough to see all of it.
    pub fn frame(&mut self, min: Vec3, max: Vec3) {
        self.focus = (min + max) / 2.0;
        self.radius = ((max - min).length() * 1.2).max(0.5);
    }
}

pub fn orbit(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut cams: Query<(&mut Orbit, &mut Transform)>,
) {
    for (mut o, mut t) in &mut cams {
        if o.follow {
            continue;
        }
        if buttons.pressed(MouseButton::Left) {
            o.yaw -= motion.delta.x * 0.005;
            o.pitch = (o.pitch + motion.delta.y * 0.005).clamp(-1.5, 1.5);
        }
        if scroll.delta.y != 0.0 {
            o.radius = (o.radius * (1.0 - scroll.delta.y * 0.1)).max(0.05);
        }
        let offset = Vec3::new(o.yaw.sin() * o.pitch.cos(), o.pitch.sin(), o.yaw.cos() * o.pitch.cos()) * o.radius;
        *t = Transform::from_translation(o.focus + offset).looking_at(o.focus, Vec3::Y);
    }
}

/// `--shot <png>`: once `ready`, saves one frame and exits. With `burst > 1` it saves that many
/// frames `interval` seconds apart as `<stem>_<i>.png`.
#[derive(Resource)]
pub struct Shot {
    pub path: PathBuf,
    pub ready: bool,
    pub burst: u32,
    pub interval: f32,
    waited: f32,
    requested: u32,
}

impl Shot {
    pub fn new(path: PathBuf) -> Self {
        Self { path, ready: false, burst: 1, interval: 0.05, waited: 0.0, requested: 0 }
    }

    fn file(&self, i: u32) -> PathBuf {
        if self.burst <= 1 {
            return self.path.clone();
        }
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.path.with_file_name(format!("{stem}_{i}.png"))
    }
}

/// Pipelines compile asynchronously, so the shot waits for wall-clock time rather than frames.
const SHOT_DELAY: f32 = 4.0;

pub fn take_shot(mut commands: Commands, mut shot: ResMut<Shot>, time: Res<Time>) {
    if !shot.ready {
        return;
    }
    shot.waited += time.delta_secs();
    let n = shot.burst.max(1);
    if shot.requested < n && shot.waited >= SHOT_DELAY + shot.requested as f32 * shot.interval {
        let file = shot.file(shot.requested);
        let _ = std::fs::remove_file(&file);
        shot.requested += 1;
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(file));
    }
    if shot.requested == n && shot.waited >= SHOT_DELAY + n as f32 * shot.interval + 2.0 && (0..n).all(|i| shot.file(i).is_file()) {
        std::process::exit(0);
    }
    if shot.waited > SHOT_DELAY + 30.0 {
        eprintln!("screenshot never arrived");
        std::process::exit(1);
    }
}
