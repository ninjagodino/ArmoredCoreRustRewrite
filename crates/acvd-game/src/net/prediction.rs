//! Local AC prediction and reconciliation.
//!
//! The local AC is simulated immediately with the same validated input conversion the server
//! uses. On every authoritative `LocalPlayerState` the prediction is reset to the exact server
//! state (position, velocity, EN, every phase/cooldown timer) and all unacked input frames are
//! replayed. Small corrections are hidden behind a visual offset that decays over 100 ms; large
//! corrections snap so the player never sees a long rubber-band.

use acvd_proto::{Loadout, LocalPlayerState};
use acvd_sim::collision::Collision;
use acvd_sim::ids::NetId;
use acvd_sim::world::{apply_local_state, SimAc};
use glam::Vec3;

use super::input_sampler::SampledInput;

/// Visual smoothing time for small corrections.
pub const SMOOTH_SECS: f32 = 0.1;
/// Corrections above this distance snap instead of smoothing.
pub const SNAP_DISTANCE: f32 = 2.0;
/// Corrections above this yaw snap instead of smoothing.
pub const SNAP_YAW: f32 = 0.5;

/// What happened on one correction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reconcile {
    /// No authoritative state had been applied yet (join/respawn).
    Initial,
    /// The difference was smoothed over the visual offset.
    Smooth { offset: Vec3, yaw_offset: f32 },
    /// The difference exceeded the snap threshold.
    Snapped { distance: f32, yaw: f32 },
}

/// Reconciliation counters for telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PredStats {
    pub reconciles: u64,
    pub replays: u64,
    pub smoothed: u64,
    pub snaps: u64,
    pub last_distance: f32,
    pub last_yaw: f32,
}

/// The predicted local AC.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub ac: SimAc,
    visual_offset: Vec3,
    visual_yaw_offset: f32,
    /// Authoritative server tick the current prediction state is at.
    pub tick: u32,
    next_projectile: u32,
    pub stats: PredStats,
}

impl Prediction {
    /// A standing AC before the first authoritative snapshot arrives.
    #[cfg(test)]
    pub fn new(net_id: NetId, loadout: Loadout, position: Vec3, yaw: f32) -> Self {
        let mut ac = SimAc::new(net_id, loadout, position, yaw, 0);
        ac.motion.yaw = yaw;
        Self {
            ac,
            visual_offset: Vec3::ZERO,
            visual_yaw_offset: 0.0,
            tick: 0,
            next_projectile: 1,
            stats: PredStats::default(),
        }
    }

    /// Builds from the first `LocalPlayerState` (join/respawn).
    pub fn from_state(net_id: NetId, loadout: Loadout, state: &LocalPlayerState) -> Self {
        let mut ac = SimAc::new(net_id, loadout, Vec3::ZERO, state.yaw, 0);
        apply_local_state(&mut ac, state);
        Self {
            ac,
            visual_offset: Vec3::ZERO,
            visual_yaw_offset: 0.0,
            tick: state.server_tick,
            next_projectile: 1,
            stats: PredStats::default(),
        }
    }

    #[cfg(test)]
    pub fn position(&self) -> Vec3 {
        self.ac.position
    }

    pub fn visual_position(&self) -> Vec3 {
        self.ac.position + self.visual_offset
    }

    pub fn visual_yaw(&self) -> f32 {
        self.ac.motion.yaw + self.visual_yaw_offset
    }

    #[cfg(test)]
    pub fn visual_offset(&self) -> Vec3 {
        self.visual_offset
    }

    pub fn set_loadout(&mut self, loadout: Loadout) {
        let stats = acvd_sim::stats::stats_from_loadout(&loadout);
        let design = acvd_sim::stats::design_from_loadout(&loadout);
        self.ac.loadout = loadout;
        self.ac.stats = stats;
        self.ac.armament = acvd_sim::weapons::Armament::from_design(&design);
        self.ac.motion.ctrl = self.ac.stats.ctrl.clone();
        self.ac.motion.en_max = self.ac.motion.ctrl.en_capacity.max(0.0);
        self.ac.motion.en = self.ac.motion.en.min(self.ac.motion.en_max);
        self.ac.max_ap = self.ac.stats.ap_max;
        self.ac.ap = self.ac.ap.min(self.ac.max_ap);
    }

    /// Predicts one tick with the quantized input the server will receive.
    pub fn step(
        &mut self,
        sample: SampledInput,
        collision: &mut Collision,
        apply_to_prop: impl FnMut(u32, f32) -> bool,
    ) {
        self.ac.last_input = sample.sim;
        self.ac.motion.prev_position = self.ac.position;
        self.tick = self.tick.wrapping_add(1);
        let _ = self.ac.step_weapons(
            &sample.sim,
            self.tick,
            acvd_sim::world::DEFAULT_MATCH_SEED,
            None,
            &mut self.next_projectile,
        );
        acvd_sim::motion::step_validated_with_props(
            &mut self.ac.motion,
            &sample.sim,
            &mut self.ac.position,
            collision,
            apply_to_prop,
        );
        self.ac.pose = acvd_sim::world::remote_pose(&self.ac.motion);
    }

    /// Resets to the authoritative state and replays `frames` (unacked, in order).
    pub fn reconcile(
        &mut self,
        state: &LocalPlayerState,
        frames: impl Iterator<Item = SampledInput>,
        collision: &mut Collision,
        mut apply_to_prop: impl FnMut(u32, f32) -> bool,
    ) -> Reconcile {
        let rendered = self.ac.position + self.visual_offset;
        let rendered_yaw = self.ac.motion.yaw + self.visual_yaw_offset;
        let old_pos = self.ac.position;
        let old_yaw = self.ac.motion.yaw;

        apply_local_state(&mut self.ac, state);
        self.tick = state.server_tick;
        let mut replayed = 0u32;
        for sample in frames {
            self.ac.motion.prev_position = self.ac.position;
            self.tick = self.tick.wrapping_add(1);
            let _ = self.ac.step_weapons(
                &sample.sim,
                self.tick,
                acvd_sim::world::DEFAULT_MATCH_SEED,
                None,
                &mut self.next_projectile,
            );
            acvd_sim::motion::step_validated_with_props(
                &mut self.ac.motion,
                &sample.sim,
                &mut self.ac.position,
                collision,
                &mut apply_to_prop,
            );
            replayed += 1;
        }
        self.ac.pose = acvd_sim::world::remote_pose(&self.ac.motion);
        self.stats.reconciles += 1;
        self.stats.replays += u64::from(replayed);

        let distance = old_pos.distance(state.pos.into());
        let yaw_distance = acvd_proto::wrap_angle(old_yaw - state.yaw).abs();
        self.stats.last_distance = distance;
        self.stats.last_yaw = yaw_distance;

        let snap_distance = (rendered - self.ac.position).length();
        let snap_yaw = acvd_proto::wrap_angle(rendered_yaw - self.ac.motion.yaw).abs();
        if snap_distance > SNAP_DISTANCE || snap_yaw > SNAP_YAW {
            self.visual_offset = Vec3::ZERO;
            self.visual_yaw_offset = 0.0;
            self.stats.snaps += 1;
            Reconcile::Snapped {
                distance: snap_distance,
                yaw: snap_yaw,
            }
        } else {
            self.visual_offset = rendered - self.ac.position;
            self.visual_yaw_offset = acvd_proto::wrap_angle(rendered_yaw - self.ac.motion.yaw);
            self.stats.smoothed += 1;
            Reconcile::Smooth {
                offset: self.visual_offset,
                yaw_offset: self.visual_yaw_offset,
            }
        }
    }

    /// Clears presentation-only one-frame flags after they were copied to the game's `Pilot`.
    pub fn clear_presentation(&mut self) {
        self.ac.motion.landed = false;
        self.ac.motion.water_splash_at = None;
        self.ac.motion.quick_pending = None;
    }

    /// Decays the visual offset toward the authoritative prediction.
    pub fn decay_visual(&mut self, dt: f32) {
        let k = (1.0 - (dt / SMOOTH_SECS).min(1.0)).max(0.0);
        self.visual_offset *= k;
        self.visual_yaw_offset *= k;
        if self.visual_offset.length_squared() < 1e-8 {
            self.visual_offset = Vec3::ZERO;
        }
        if self.visual_yaw_offset.abs() < 1e-6 {
            self.visual_yaw_offset = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_sim::input::InputFrame;
    use acvd_sim::world::local_state_of;

    fn loadout() -> Loadout {
        acvd_sim::stats::default_loadout()
    }

    fn sample(seq: u16, yaw: f32) -> SampledInput {
        let frame = InputFrame {
            stick: glam::Vec2::Y,
            ..InputFrame::default()
        };
        super::super::input_sampler::sample(seq, &frame, yaw, 0.0)
    }

    fn step_frames(pred: &mut Prediction, frames: &[SampledInput], collision: &mut Collision) {
        for frame in frames {
            pred.step(*frame, collision, |_, _| false);
        }
    }

    #[test]
    fn prediction_is_immediate() {
        let loadout = loadout();
        let mut pred = Prediction::new(NetId(1), loadout, Vec3::new(0.0, 10.0, 0.0), 0.0);
        let mut collision = Collision::default();
        let before = pred.position();
        pred.step(sample(1, 0.0), &mut collision, |_, _| false);
        assert!(
            (pred.position() - before).length() > 0.0 || pred.ac.motion.velocity.length() > 0.0,
            "the local AC responds on the input tick"
        );
    }

    #[test]
    fn replay_reaches_the_same_state_as_the_server() {
        let loadout = loadout();
        let mut collision = Collision::default();
        let frames: Vec<SampledInput> = (1..=12).map(|i| sample(i, 0.4)).collect();

        let mut server = SimAc::new(NetId(1), loadout, Vec3::new(4.0, 30.0, -2.0), 0.2, 0);
        for frame in &frames {
            acvd_sim::motion::step_validated_with_props(
                &mut server.motion,
                &frame.sim,
                &mut server.position,
                &mut collision,
                |_, _| false,
            );
        }
        let authoritative = local_state_of(&server, 8, 0);

        let mut pred = Prediction::new(NetId(1), loadout, Vec3::new(0.0, 30.0, 0.0), 0.0);
        step_frames(&mut pred, &frames, &mut collision);
        // Force a visible mismatch, then reconcile from the server state and replay the frames
        // the server has not acked (9..=12).
        pred.ac.position += Vec3::new(1.0, 0.0, 0.0);
        let unacked: Vec<SampledInput> = frames.iter().copied().filter(|f| f.seq > 8).collect();
        pred.reconcile(
            &authoritative,
            unacked.into_iter(),
            &mut collision,
            |_, _| false,
        );

        // Rebuild the server's own post-8 state to compare replay.
        let mut expect = SimAc::new(NetId(1), loadout, Vec3::ZERO, 0.0, 0);
        apply_local_state(&mut expect, &authoritative);
        for frame in &frames {
            if frame.seq > 8 {
                acvd_sim::motion::step_validated_with_props(
                    &mut expect.motion,
                    &frame.sim,
                    &mut expect.position,
                    &mut collision,
                    |_, _| false,
                );
            }
        }
        assert!(
            (pred.position() - expect.position).length() < 1e-4,
            "prediction replay {:?} != server {:?}",
            pred.position(),
            expect.position
        );
        assert!((pred.ac.motion.yaw - expect.motion.yaw).abs() < 1e-4);
    }

    #[test]
    fn small_error_smooths_and_large_error_snaps() {
        let loadout = loadout();
        let mut collision = Collision::default();
        let mut pred = Prediction::new(NetId(1), loadout, Vec3::new(0.0, 30.0, 0.0), 0.0);
        let mut server = SimAc::new(NetId(1), loadout, Vec3::new(0.0, 30.0, 0.0), 0.0, 0);
        let state = local_state_of(&server, 0, 0);
        let r = pred.reconcile(
            &state,
            std::iter::empty::<SampledInput>(),
            &mut collision,
            |_, _| false,
        );
        match r {
            Reconcile::Smooth { .. } => {}
            other => panic!("small error should smooth: {other:?}"),
        }
        pred.decay_visual(SMOOTH_SECS);
        assert_eq!(pred.visual_offset(), Vec3::ZERO);

        server.position += Vec3::new(5.0, 0.0, 0.0);
        let state = local_state_of(&server, 0, 0);
        let r = pred.reconcile(
            &state,
            std::iter::empty::<SampledInput>(),
            &mut collision,
            |_, _| false,
        );
        assert!(matches!(r, Reconcile::Snapped { .. }), "{r:?}");
        assert_eq!(pred.visual_offset(), Vec3::ZERO);
        assert!((pred.position() - Vec3::new(5.0, 30.0, 0.0)).length() < 1e-4);
    }
}
