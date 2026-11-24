//! Bevy pieces the viewer and the game share: an orbit camera and a one-frame screenshot mode.

use std::path::PathBuf;

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

/// Drag the left mouse button to orbit `focus`, scroll to zoom, WASD + Space/Ctrl to fly.
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

/// Camera-relative horizontal (WASD) and vertical (Space/Ctrl) displacement for [`orbit`].
pub fn freecam_move(
    yaw: f32,
    radius: f32,
    dt: f32,
    forward_in: f32,
    right_in: f32,
    up_in: f32,
    fast: bool,
) -> Vec3 {
    let dir = Vec3::new(-yaw.sin(), 0.0, -yaw.cos()) * forward_in
        + Vec3::new(yaw.cos(), 0.0, -yaw.sin()) * right_in
        + Vec3::Y * up_in;
    if dir.length_squared() == 0.0 || dt <= 0.0 {
        return Vec3::ZERO;
    }
    let base_speed = (radius * 1.5).clamp(10.0, 200.0);
    let speed = if fast { base_speed * 3.0 } else { base_speed };
    dir.normalize() * speed * dt
}

pub fn orbit(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut cams: Query<(&mut Orbit, &mut Transform)>,
) {
    let forward_in = (keys.pressed(KeyCode::KeyW) as i32 - keys.pressed(KeyCode::KeyS) as i32) as f32;
    let right_in = (keys.pressed(KeyCode::KeyD) as i32 - keys.pressed(KeyCode::KeyA) as i32) as f32;
    let up_in = (keys.pressed(KeyCode::Space) as i32
        - (keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)) as i32)
        as f32;
    let fast = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let dt = time.delta_secs();
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
        let delta = freecam_move(o.yaw, o.radius, dt, forward_in, right_in, up_in, fast);
        o.focus += delta;
        let offset = Vec3::new(o.yaw.sin() * o.pitch.cos(), o.pitch.sin(), o.yaw.cos() * o.pitch.cos()) * o.radius;
        *t = Transform::from_translation(o.focus + offset).looking_at(o.focus, Vec3::Y);
    }
}

/// Set on the frame [`take_shot`] requests a screenshot, so a probe can log that exact frame.
#[derive(Resource, Default)]
pub struct ShotMark {
    pub path: Option<PathBuf>,
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

pub fn take_shot(
    mut commands: Commands,
    mut shot: ResMut<Shot>,
    mut mark: Option<ResMut<ShotMark>>,
    time: Res<Time>,
) {
    if !shot.ready {
        return;
    }
    shot.waited += time.delta_secs();
    let n = shot.burst.max(1);
    if shot.requested < n && shot.waited >= SHOT_DELAY + shot.requested as f32 * shot.interval {
        let file = shot.file(shot.requested);
        let _ = std::fs::remove_file(&file);
        if let Some(mark) = mark.as_mut() {
            mark.path = Some(file.clone());
        }
        eprintln!("SHOT request {}", file.display());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freecam_move_directions_and_vertical() {
        // At yaw = 0, camera sits at +Z looking toward -Z; right is +X, up is +Y.
        let fwd = freecam_move(0.0, 10.0, 1.0, 1.0, 0.0, 0.0, false);
        assert!((fwd - Vec3::new(0.0, 0.0, -15.0)).length() < 1e-4, "fwd={fwd:?}");

        let right = freecam_move(0.0, 10.0, 1.0, 0.0, 1.0, 0.0, false);
        assert!((right - Vec3::new(15.0, 0.0, 0.0)).length() < 1e-4, "right={right:?}");

        let up = freecam_move(0.0, 10.0, 1.0, 0.0, 0.0, 1.0, false);
        assert!((up - Vec3::new(0.0, 15.0, 0.0)).length() < 1e-4, "up={up:?}");

        let down = freecam_move(0.0, 10.0, 1.0, 0.0, 0.0, -1.0, false);
        assert!((down - Vec3::new(0.0, -15.0, 0.0)).length() < 1e-4, "down={down:?}");

        // At yaw = PI/2, camera sits at +X looking toward -X; right is -Z.
        let half_pi = std::f32::consts::FRAC_PI_2;
        let fwd_rot = freecam_move(half_pi, 10.0, 1.0, 1.0, 0.0, 0.0, false);
        assert!((fwd_rot - Vec3::new(-15.0, 0.0, 0.0)).length() < 1e-4, "fwd_rot={fwd_rot:?}");

        let right_rot = freecam_move(half_pi, 10.0, 1.0, 0.0, 1.0, 0.0, false);
        assert!((right_rot - Vec3::new(0.0, 0.0, -15.0)).length() < 1e-4, "right_rot={right_rot:?}");

        // Diagonal + vertical input is normalized; Shift triples speed.
        let diag = freecam_move(0.0, 10.0, 1.0, 1.0, 1.0, 1.0, false);
        assert!((diag.length() - 15.0).abs() < 1e-4, "diag len={}", diag.length());

        let fast = freecam_move(0.0, 10.0, 1.0, 1.0, 0.0, 0.0, true);
        assert!((fast.length() - 45.0).abs() < 1e-4, "fast len={}", fast.length());

        assert_eq!(freecam_move(0.0, 10.0, 1.0, 0.0, 0.0, 0.0, false), Vec3::ZERO);
        assert_eq!(freecam_move(0.0, 10.0, 0.0, 1.0, 0.0, 0.0, false), Vec3::ZERO);
    }
}
