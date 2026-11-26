//! Quantizers shared by the snapshot, input, and event messages.
//!
//! Positions quantize against a per-match map AABB (~3 cm at 2 km), velocities as s16 at
//! 0.01 m/tick, yaw as a full u16 turn, pitch as an i8 across +/-90 degrees, and directions as
//! three i8 components.

use std::f32::consts::{FRAC_PI_2, TAU};

use glam::Vec3;

/// Map bounds used to turn world positions into `[u16; 3]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PosQuantizer {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for PosQuantizer {
    fn default() -> Self {
        Self {
            min: Vec3::splat(-1000.0),
            max: Vec3::splat(1000.0),
        }
    }
}

impl PosQuantizer {
    pub const DEFAULT: Self = Self {
        min: Vec3::splat(-1000.0),
        max: Vec3::splat(1000.0),
    };

    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Largest per-axis quantization error in metres.
    pub fn step(&self) -> Vec3 {
        let span = self.max - self.min;
        Vec3::new(
            span.x.max(0.0) / 65535.0,
            span.y.max(0.0) / 65535.0,
            span.z.max(0.0) / 65535.0,
        )
    }

    pub fn quantize(&self, pos: Vec3) -> [u16; 3] {
        let axis = |p: f32, min: f32, max: f32| {
            let span = max - min;
            if span <= f32::EPSILON {
                0
            } else {
                (((p - min) / span).clamp(0.0, 1.0) * 65535.0).round() as u16
            }
        };
        [
            axis(pos.x, self.min.x, self.max.x),
            axis(pos.y, self.min.y, self.max.y),
            axis(pos.z, self.min.z, self.max.z),
        ]
    }

    pub fn dequantize(&self, q: [u16; 3]) -> Vec3 {
        let axis = |v: u16, min: f32, max: f32| min + (v as f32 / 65535.0) * (max - min);
        Vec3::new(
            axis(q[0], self.min.x, self.max.x),
            axis(q[1], self.min.y, self.max.y),
            axis(q[2], self.min.z, self.max.z),
        )
    }
}

pub fn quantize_yaw(yaw: f32) -> u16 {
    ((yaw.rem_euclid(TAU) / TAU) * 65535.0).round() as u16
}

pub fn dequantize_yaw(q: u16) -> f32 {
    (q as f32 / 65535.0) * TAU
}

pub fn quantize_pitch(pitch: f32) -> i8 {
    ((pitch.clamp(-FRAC_PI_2, FRAC_PI_2) / FRAC_PI_2) * 127.0).round() as i8
}

pub fn dequantize_pitch(q: i8) -> f32 {
    (q as f32 / 127.0) * FRAC_PI_2
}

/// Input frames carry pitch as `i16`; Phase 1 uses the same +/-90 degree scale as snapshots.
pub fn quantize_input_pitch(pitch: f32) -> i16 {
    i16::from(quantize_pitch(pitch))
}

pub fn dequantize_input_pitch(q: i16) -> f32 {
    dequantize_pitch(q.clamp(-127, 127) as i8)
}

/// Metres per tick in s16 units.
pub const VEL_SCALE: f32 = 0.01;

pub fn quantize_vel(v: f32) -> i16 {
    (v / VEL_SCALE)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

pub fn dequantize_vel(q: i16) -> f32 {
    f32::from(q) * VEL_SCALE
}

/// Metres in an s16 event/missile coordinate.
pub const COORD_SCALE: f32 = 0.01;

pub fn quantize_coord(v: f32) -> i16 {
    (v / COORD_SCALE)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

pub fn dequantize_coord(q: i16) -> f32 {
    f32::from(q) * COORD_SCALE
}

/// Unit direction as three signed bytes (each component `/127`).
pub fn quantize_dir3(dir: Vec3) -> [i8; 3] {
    let dir = dir.normalize_or_zero();
    [
        (dir.x * 127.0).round().clamp(-127.0, 127.0) as i8,
        (dir.y * 127.0).round().clamp(-127.0, 127.0) as i8,
        (dir.z * 127.0).round().clamp(-127.0, 127.0) as i8,
    ]
}

pub fn dequantize_dir3(q: [i8; 3]) -> Vec3 {
    Vec3::new(f32::from(q[0]), f32::from(q[1]), f32::from(q[2])).normalize_or_zero()
}

/// Wraps an angle difference into `(-pi, pi]`.
pub fn wrap_angle(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_quantizer_roundtrips_within_a_step() {
        let q = PosQuantizer::new(
            Vec3::new(-800.0, -20.0, -800.0),
            Vec3::new(800.0, 60.0, 800.0),
        );
        for p in [
            Vec3::ZERO,
            Vec3::new(123.4, 12.5, -456.7),
            Vec3::new(-799.9, -19.9, 799.9),
        ] {
            let back = q.dequantize(q.quantize(p));
            assert!((back - p).abs().max_element() <= q.step().max_element() * 0.51 + 1e-4);
        }
    }

    #[test]
    fn yaw_pitch_and_velocity_roundtrip() {
        for yaw in [-3.0f32, -1.2, 0.0, 1.9, 6.2] {
            let d = wrap_angle(dequantize_yaw(quantize_yaw(yaw)) - yaw).abs();
            assert!(d < 1e-4, "yaw {yaw} err {d}");
        }
        for pitch in [-1.5f32, -0.4, 0.0, 0.7, 1.5] {
            let d = (dequantize_pitch(quantize_pitch(pitch)) - pitch).abs();
            assert!(d < 0.013, "pitch {pitch} err {d}");
        }
        for v in [-12.34f32, -0.01, 0.0, 0.55, 9.99] {
            let d = (dequantize_vel(quantize_vel(v)) - v).abs();
            assert!(d <= VEL_SCALE * 0.5 + 1e-6, "vel {v} err {d}");
        }
    }

    #[test]
    fn direction_roundtrip_is_close() {
        for dir in [Vec3::X, Vec3::Y, Vec3::NEG_Z, Vec3::new(0.3, -0.8, 0.2)] {
            let back = dequantize_dir3(quantize_dir3(dir));
            assert!(
                (back - dir.normalize()).length() < 0.02,
                "{dir:?} -> {back:?}"
            );
        }
    }
}
