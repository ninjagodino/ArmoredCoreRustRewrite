//! 60 Hz input sampling, the 128-entry unacked ring, and lightweight send pacing.
//!
//! The sampler quantizes the camera-derived absolute aim once, then predicts with the same
//! quantized values the server receives, so replay after a correction is bit-for-bit identical.

use std::collections::VecDeque;

use acvd_proto::{input_buttons, quantize_input_pitch, quantize_yaw, InputFrame as WireInput};
use acvd_sim::input::InputFrame as SimInput;

/// Unacked input history.
pub const RING_CAPACITY: usize = 128;

/// One sampled tick as both wire bytes and simulation input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampledInput {
    pub seq: u16,
    pub wire: WireInput,
    pub sim: SimInput,
}

/// Converts a simulation input + absolute aim into the wire frame.
pub fn to_wire(frame: &SimInput, aim_yaw: f32, aim_pitch: f32) -> WireInput {
    let mut buttons = 0;
    if frame.jump {
        buttons |= input_buttons::JUMP;
    }
    if frame.boost_toggle {
        buttons |= input_buttons::BOOST_TOGGLE;
    }
    if frame.glide {
        buttons |= input_buttons::GLIDE;
    }
    if frame.quick_boost {
        buttons |= input_buttons::QUICK_BOOST;
    }
    if frame.qb_held {
        buttons |= input_buttons::QB_HELD;
    }
    if frame.kick {
        buttons |= input_buttons::KICK;
    }
    if frame.fire_r {
        buttons |= input_buttons::FIRE_R;
    }
    if frame.fire_l {
        buttons |= input_buttons::FIRE_L;
    }
    if frame.fire_shoulder {
        buttons |= input_buttons::FIRE_SHOULDER;
    }
    if frame.bay_r {
        buttons |= input_buttons::BAY_R;
    }
    if frame.bay_l {
        buttons |= input_buttons::BAY_L;
    }
    if frame.fire_ow {
        buttons |= input_buttons::FIRE_OW;
    }
    WireInput {
        buttons,
        stick: [
            (frame.stick.x * 127.0).round().clamp(-127.0, 127.0) as i8,
            (frame.stick.y * 127.0).round().clamp(-127.0, 127.0) as i8,
        ],
        aim_yaw: quantize_yaw(aim_yaw),
        aim_pitch: quantize_input_pitch(aim_pitch),
        lock_target: frame.lock_target.0,
    }
}

/// Converts a wire frame back to simulation input (the same conversion the server applies).
pub fn from_wire(wire: &WireInput) -> SimInput {
    let buttons = wire.buttons;
    SimInput {
        seq: 0,
        stick: glam::Vec2::new(
            f32::from(wire.stick[0]) / 127.0,
            f32::from(wire.stick[1]) / 127.0,
        )
        .clamp_length_max(1.0),
        turn: 0.0,
        look: 0.0,
        jump: buttons & input_buttons::JUMP != 0,
        boost_toggle: buttons & input_buttons::BOOST_TOGGLE != 0,
        glide: buttons & input_buttons::GLIDE != 0,
        quick_boost: buttons & input_buttons::QUICK_BOOST != 0,
        qb_held: buttons & input_buttons::QB_HELD != 0,
        kick: buttons & input_buttons::KICK != 0,
        fire_r: buttons & input_buttons::FIRE_R != 0,
        fire_l: buttons & input_buttons::FIRE_L != 0,
        fire_shoulder: buttons & input_buttons::FIRE_SHOULDER != 0,
        bay_r: buttons & input_buttons::BAY_R != 0,
        bay_l: buttons & input_buttons::BAY_L != 0,
        fire_ow: buttons & input_buttons::FIRE_OW != 0,
        aim_yaw: acvd_proto::dequantize_yaw(wire.aim_yaw),
        aim_pitch: acvd_proto::dequantize_input_pitch(wire.aim_pitch),
        lock_target: acvd_sim::ids::NetId(wire.lock_target),
    }
}

/// Samples one tick. `seq` advances by one per call on the caller side.
pub fn sample(seq: u16, frame: &SimInput, aim_yaw: f32, aim_pitch: f32) -> SampledInput {
    let wire = to_wire(frame, aim_yaw, aim_pitch);
    SampledInput {
        seq,
        wire,
        sim: from_wire(&wire),
    }
}

/// 128-entry history of unacked samples.
#[derive(Clone, Debug, Default)]
pub struct InputRing {
    entries: VecDeque<SampledInput>,
}

impl InputRing {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push(&mut self, sample: SampledInput) {
        if self.entries.len() >= RING_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(sample);
    }

    /// Drops every frame up to and including `acked` (wrapping sequence).
    pub fn ack(&mut self, acked: u16) {
        while let Some(front) = self.entries.front() {
            if front.seq.wrapping_sub(acked) >= 0x8000 || front.seq == acked {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The current frame plus the last two unacked frames, oldest first, capped at 3.
    pub fn send_frames(&self) -> Vec<WireInput> {
        let start = self.entries.len().saturating_sub(3);
        self.entries.iter().skip(start).map(|s| s.wire).collect()
    }

    /// Sequence of the first frame [`Self::send_frames`] returns.
    pub fn send_start_seq(&self) -> Option<u16> {
        let start = self.entries.len().saturating_sub(3);
        self.entries.get(start).map(|s| s.seq)
    }

    #[cfg(test)]
    pub fn oldest_seq(&self) -> Option<u16> {
        self.entries.front().map(|s| s.seq)
    }

    #[cfg(test)]
    pub fn newest_seq(&self) -> Option<u16> {
        self.entries.back().map(|s| s.seq)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SampledInput> {
        self.entries.iter()
    }
}

/// Client input-tick pacing.
///
/// Runs the normal 60 Hz ticks from wall time, then nudges one tick forward when the server is
/// close behind the sent sequence (about to starve), and drops a tick when the client has run far
/// ahead. Never more than two ticks per frame.
#[derive(Clone, Debug)]
pub struct Pacer {
    accumulator: f32,
}

impl Default for Pacer {
    fn default() -> Self {
        Self { accumulator: 0.0 }
    }
}

impl Pacer {
    pub const TICK_DT: f32 = 1.0 / 60.0;

    /// `lead` = sent seq - acked seq (wrapping). `ideal_lead` = RTT/2 ticks + 1.
    pub fn ticks(&mut self, dt: f32, lead: i32, ideal_lead: i32) -> u32 {
        self.accumulator = (self.accumulator + dt).min(0.1);
        let mut ticks: u32 = 0;
        while self.accumulator >= Self::TICK_DT && ticks < 2 {
            self.accumulator -= Self::TICK_DT;
            ticks += 1;
        }
        if ticks == 0 && self.accumulator >= Self::TICK_DT * 0.5 && lead < ideal_lead {
            // Send a hair early so the next input is in flight before the server's tick.
            self.accumulator = 0.0;
            ticks = 1;
        }
        if lead > ideal_lead + 4 {
            // Far ahead of what the server can consume (max two queued ticks per step).
            ticks = ticks.saturating_sub(1);
        }
        ticks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    fn frame() -> SimInput {
        SimInput {
            stick: Vec2::new(0.5, -0.25),
            jump: true,
            boost_toggle: true,
            glide: true,
            quick_boost: true,
            qb_held: true,
            kick: true,
            ..SimInput::default()
        }
    }

    #[test]
    fn wire_roundtrip_keeps_buttons_and_quantized_aim() {
        let s = sample(7, &frame(), 1.25, -0.4);
        assert_eq!(s.wire.stick, [64, -32]);
        assert_eq!(
            s.wire.buttons,
            input_buttons::JUMP
                | input_buttons::BOOST_TOGGLE
                | input_buttons::GLIDE
                | input_buttons::QUICK_BOOST
                | input_buttons::QB_HELD
                | input_buttons::KICK
        );
        let back = from_wire(&s.wire);
        assert_eq!(back.stick, s.sim.stick);
        assert!(back.jump && back.kick && back.qb_held);
        assert!((back.aim_yaw - 1.25).abs() < 1e-3);
        assert!((back.aim_pitch - -0.4).abs() < 0.01);
    }

    #[test]
    fn ring_keeps_128_and_sends_last_three() {
        let mut ring = InputRing::new();
        for seq in 0..200u16 {
            ring.push(sample(seq, &frame(), 0.0, 0.0));
        }
        assert_eq!(ring.len(), RING_CAPACITY);
        assert_eq!(ring.oldest_seq(), Some(72));
        let frames = ring.send_frames();
        assert_eq!(frames.len(), 3);
        assert_eq!(ring.newest_seq(), Some(199));
        ring.ack(140);
        assert_eq!(ring.oldest_seq(), Some(141));
        ring.ack(199);
        assert!(ring.is_empty());
    }

    #[test]
    fn ack_wraps_across_u16_boundary() {
        let mut ring = InputRing::new();
        for seq in [65534, 65535, 0, 1] {
            ring.push(sample(seq, &frame(), 0.0, 0.0));
        }
        ring.ack(0);
        assert_eq!(ring.oldest_seq(), Some(1));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn pacer_bounds_two_ticks_and_catches_up_when_starving() {
        let mut pacer = Pacer::default();
        assert_eq!(pacer.ticks(1.0 / 60.0, 3, 3), 1);
        assert_eq!(pacer.ticks(0.2, 3, 3), 2, "catch-up is capped at two ticks");
        // Half a frame with the server close behind sends one early input.
        let mut pacer = Pacer::default();
        assert_eq!(pacer.ticks(Pacer::TICK_DT * 0.5, 0, 3), 1);
        // Far ahead: drop a tick so the queue cannot grow without bound.
        let mut pacer = Pacer::default();
        assert_eq!(pacer.ticks(Pacer::TICK_DT * 3.0, 20, 3), 1);
    }
}
