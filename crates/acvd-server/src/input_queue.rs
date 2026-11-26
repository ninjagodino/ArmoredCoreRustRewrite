//! Per-player server input queue with bounded sequence advance and a repeat-then-idle
//! starvation policy.
//!
//! Clients send their current frame plus the last two unacked frames. Packets can arrive out of
//! order and a frame can be repeated across packets, so the queue:
//! - accepts only sequence numbers inside a bounded window ahead of the next expected frame,
//! - drops duplicates and already-consumed frames,
//! - consumes at most [`MAX_NEW_TICKS_PER_STEP`] queued frames per server tick,
//! - repeats the last applied frame with all one-shot edges cleared for
//!   [`REPEAT_STARVED_TICKS`] ticks, then falls back to idle input that keeps the last absolute
//!   aim.

use acvd_proto::InputFrame;
use acvd_sim::input::InputFrame as SimInput;
use acvd_sim::world::SimAc;

/// New queued input frames consumed per server tick. Extra frames wait for later ticks instead of
/// letting a client accelerate its AC by flooding the queue.
pub const MAX_NEW_TICKS_PER_STEP: usize = 2;
/// How far ahead of the next expected frame a packet may be.
pub const MAX_SEQ_AHEAD: u16 = 120;
/// Ticks the last applied input is repeated while the queue is empty (100 ms).
pub const REPEAT_STARVED_TICKS: u32 = 6;

/// Counters for one queue lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueStats {
    pub accepted: u64,
    pub duplicates: u64,
    pub stale: u64,
    pub too_far: u64,
    pub starved_ticks: u64,
    pub idle_ticks: u64,
}

/// What one server tick should run for this player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputTick {
    pub frame: InputFrame,
    /// Sequence number echoed in `SnapshotPacket::acked_input_seq`.
    pub acked_seq: u16,
    /// True when no fresh queued frame was available.
    pub starved: bool,
}

/// One player's validated input stream.
#[derive(Clone, Debug, Default)]
pub struct InputQueue {
    next_seq: Option<u16>,
    pending: Vec<(u16, InputFrame)>,
    last_acked: u16,
    last_frame: Option<InputFrame>,
    starved: u32,
    stats: QueueStats,
}

impl InputQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_acked(&self) -> u16 {
        self.last_acked
    }

    pub fn next_seq(&self) -> Option<u16> {
        self.next_seq
    }

    /// Last accepted wire frame (for telemetry/tests).
    pub fn last_frame(&self) -> Option<InputFrame> {
        self.last_frame
    }

    /// True once at least one client frame has been applied. An AC is not stepped before that, so
    /// its authoritative history starts at its first input instead of idle ticks.
    pub fn has_input(&self) -> bool {
        self.next_seq.is_some() || self.last_frame.is_some()
    }

    /// Drops pending/stream state after a respawn. Cumulative stats stay.
    pub fn reset(&mut self) {
        self.next_seq = None;
        self.pending.clear();
        self.last_frame = None;
        self.starved = 0;
    }

    pub fn queued(&self) -> usize {
        self.pending.len()
    }

    pub fn stats(&self) -> QueueStats {
        self.stats
    }

    /// Stores the frames of one `InputPacket`. Returns how many frames were accepted.
    pub fn accept(&mut self, start_seq: u16, frames: &[InputFrame]) -> usize {
        let mut accepted = 0;
        if self.next_seq.is_none() {
            self.next_seq = Some(start_seq);
        }
        for (i, frame) in frames.iter().enumerate() {
            let seq = start_seq.wrapping_add(i as u16);
            let Some(next) = self.next_seq else {
                continue;
            };
            let ahead = seq.wrapping_sub(next);
            if ahead >= 0x8000 {
                // Older than the next frame we need.
                self.stats.stale += 1;
                continue;
            }
            if ahead > MAX_SEQ_AHEAD {
                self.stats.too_far += 1;
                continue;
            }
            if self.pending.iter().any(|(s, _)| *s == seq) {
                self.stats.duplicates += 1;
                continue;
            }
            let at = self
                .pending
                .iter()
                .position(|(s, _)| s.wrapping_sub(next) > ahead)
                .unwrap_or(self.pending.len());
            self.pending.insert(at, (seq, *frame));
            self.stats.accepted += 1;
            accepted += 1;
        }
        accepted
    }

    /// One server tick of input: the next queued frame, then a sanitized repeat, then idle.
    ///
    /// Exactly one queued frame is applied per simulation tick, so one-shot edges (a Quick Boost,
    /// jump, or kick) are never skipped when two frames become available together. A server step
    /// that runs catch-up ticks may consume up to [`MAX_NEW_TICKS_PER_STEP`] frames, but every
    /// consumed frame gets its own simulation tick.
    pub fn next_tick(&mut self, ac: &SimAc) -> InputTick {
        let Some(mut next) = self.next_seq else {
            // No packet has ever arrived: idle at the AC's current absolute aim.
            self.stats.idle_ticks += 1;
            return InputTick {
                frame: idle_wire(None, &ac.motion),
                acked_seq: self.last_acked,
                starved: true,
            };
        };
        let mut newest: Option<(u16, InputFrame)> = None;
        if let Some(&(seq, frame)) = self.pending.first() {
            let ahead = seq.wrapping_sub(next);
            if ahead == 0 {
                self.pending.remove(0);
                newest = Some((seq, frame));
                next = next.wrapping_add(1);
                self.next_seq = Some(next);
            }
        }
        if let Some((seq, frame)) = newest {
            self.last_acked = seq;
            self.last_frame = Some(frame);
            self.starved = 0;
            return InputTick {
                frame,
                acked_seq: seq,
                starved: false,
            };
        }

        self.starved += 1;
        self.stats.starved_ticks += 1;
        if self.starved <= REPEAT_STARVED_TICKS {
            if let Some(last) = self.last_frame {
                return InputTick {
                    frame: last.repeat_safe(),
                    acked_seq: self.last_acked,
                    starved: true,
                };
            }
        }
        self.stats.idle_ticks += 1;
        InputTick {
            frame: idle_wire(self.last_frame.as_ref(), &ac.motion),
            acked_seq: self.last_acked,
            starved: true,
        }
    }
}

/// Idle wire input that keeps aiming where the player last aimed (or at the AC's current aim).
fn idle_wire(last: Option<&InputFrame>, motion: &acvd_sim::motion::AcMotion) -> InputFrame {
    match last {
        Some(last) => InputFrame {
            buttons: 0,
            stick: [0, 0],
            aim_yaw: last.aim_yaw,
            aim_pitch: last.aim_pitch,
            lock_target: last.lock_target,
            ..InputFrame::default()
        },
        None => InputFrame {
            buttons: 0,
            stick: [0, 0],
            aim_yaw: acvd_proto::quantize_yaw(motion.yaw + motion.aim_yaw_off),
            aim_pitch: acvd_proto::quantize_input_pitch(motion.aim_pitch),
            lock_target: 0,
            ..InputFrame::default()
        },
    }
}

/// Converts a wire frame to the raw simulation input. Absolute-aim validation (turn clamp) runs
/// inside `acvd_sim::world::step`.
pub fn to_sim(frame: &InputFrame, _ac: &SimAc) -> SimInput {
    let buttons = frame.buttons;
    let stick = glam::Vec2::new(
        f32::from(frame.stick[0]) / 127.0,
        f32::from(frame.stick[1]) / 127.0,
    )
    .clamp_length_max(1.0);
    let input = SimInput {
        seq: 0,
        stick,
        turn: 0.0,
        look: 0.0,
        jump: buttons & acvd_proto::input_buttons::JUMP != 0,
        boost_toggle: buttons & acvd_proto::input_buttons::BOOST_TOGGLE != 0,
        glide: buttons & acvd_proto::input_buttons::GLIDE != 0,
        quick_boost: buttons & acvd_proto::input_buttons::QUICK_BOOST != 0,
        qb_held: buttons & acvd_proto::input_buttons::QB_HELD != 0,
        kick: buttons & acvd_proto::input_buttons::KICK != 0,
        fire_r: buttons & acvd_proto::input_buttons::FIRE_R != 0,
        fire_l: buttons & acvd_proto::input_buttons::FIRE_L != 0,
        fire_shoulder: buttons & acvd_proto::input_buttons::FIRE_SHOULDER != 0,
        bay_r: buttons & acvd_proto::input_buttons::BAY_R != 0,
        bay_l: buttons & acvd_proto::input_buttons::BAY_L != 0,
        fire_ow: buttons & acvd_proto::input_buttons::FIRE_OW != 0,
        aim_yaw: acvd_proto::dequantize_yaw(frame.aim_yaw),
        aim_pitch: acvd_proto::dequantize_input_pitch(frame.aim_pitch),
        lock_target: acvd_sim::ids::NetId(frame.lock_target),
    };
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_proto::{input_buttons, quantize_yaw};
    use acvd_sim::ids::NetId;
    use acvd_sim::world::SimAc;

    fn frame(yaw: f32) -> InputFrame {
        InputFrame {
            buttons: 0,
            stick: [60, 0],
            aim_yaw: quantize_yaw(yaw),
            aim_pitch: 0,
            lock_target: 0,
        }
    }

    fn ac() -> SimAc {
        SimAc::new(
            NetId(1),
            acvd_proto::Loadout::default(),
            glam::Vec3::ZERO,
            0.0,
            0,
        )
    }

    #[test]
    fn applies_one_frame_per_tick_without_skipping_edges() {
        let ac = ac();
        let mut queue = InputQueue::new();
        let mut a = frame(0.0);
        a.buttons = input_buttons::BOOST_TOGGLE;
        let frames = vec![a, frame(0.1), frame(0.2), frame(0.3)];
        assert_eq!(queue.accept(1, &frames), 4);
        let first = queue.next_tick(&ac);
        assert_eq!(first.acked_seq, 1);
        assert_eq!(first.frame.buttons, input_buttons::BOOST_TOGGLE);
        let second = queue.next_tick(&ac);
        assert_eq!(second.acked_seq, 2, "no input tick is skipped");
        assert_eq!(queue.stats().accepted, 4);
    }

    #[test]
    fn drops_duplicates_stale_and_too_far() {
        let ac = ac();
        let mut queue = InputQueue::new();
        queue.accept(10, &[frame(0.0), frame(0.1)]);
        queue.accept(10, &[frame(0.0), frame(0.2)]);
        assert_eq!(queue.stats().duplicates, 2);
        queue.next_tick(&ac);
        queue.next_tick(&ac);
        queue.accept(5, &[frame(0.3)]);
        assert_eq!(queue.stats().stale, 1);
        queue.accept(300, &[frame(0.4)]);
        assert_eq!(queue.stats().too_far, 1);
    }

    #[test]
    fn repeat_then_idle_clears_edges() {
        let ac = ac();
        let mut queue = InputQueue::new();
        let mut pressed = frame(0.0);
        pressed.buttons =
            input_buttons::JUMP | input_buttons::BOOST_TOGGLE | input_buttons::QUICK_BOOST;
        queue.accept(1, &[pressed]);
        let first = queue.next_tick(&ac);
        assert_eq!(first.frame.buttons, pressed.buttons);
        for _ in 0..REPEAT_STARVED_TICKS {
            let repeat = queue.next_tick(&ac);
            assert!(repeat.starved);
            assert_eq!(repeat.frame.buttons, 0, "repeats must not re-trigger edges");
            assert_eq!(
                repeat.frame.stick, pressed.stick,
                "movement axes still repeat"
            );
        }
        let idle = queue.next_tick(&ac);
        assert_eq!(idle.frame.stick, [0, 0]);
    }

    #[test]
    fn sim_conversion_keeps_absolute_aim() {
        let ac = ac();
        let wire = frame(0.5);
        let sim = to_sim(&wire, &ac);
        assert!((sim.aim_yaw - acvd_proto::dequantize_yaw(wire.aim_yaw)).abs() < 1e-3);
        assert_eq!(sim.stick.x, 60.0 / 127.0);
    }
}
