//! Remote AC interpolation.
//!
//! Snapshots arrive at 30/10/2 Hz by interest tier. The renderer samples each remote buffer at
//! `estimated_server_tick - INTERP_DELAY_TICKS` (4 ticks = 66 ms), lerps position/velocity and
//! shortest-arc yaw, and snaps flags/pose. When the buffer starves it extrapolates with the last
//! velocity for at most [`MAX_EXTRAPOLATE_TICKS`] (100 ms), then holds.

use std::collections::BTreeMap;

use acvd_proto::AcRecord;
use glam::Vec3;

/// Render delay behind the newest received snapshot.
pub const INTERP_DELAY_TICKS: u32 = 4;
/// Maximum extrapolation beyond the newest sample.
pub const MAX_EXTRAPOLATE_TICKS: u32 = 6;

/// One snapshot record resolved to simulation units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemoteSample {
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub flags: u16,
    pub pose: u8,
    /// Compact bay/rack + glide-qb bits from `AcRecord::combat_flags`.
    pub combat_flags: u8,
    pub ap: u16,
    pub en: u8,
}

impl RemoteSample {
    /// Builds a simulation-space sample from a wire record using the match quantizer.
    pub fn from_record(record: &AcRecord, quantizer: &acvd_proto::PosQuantizer) -> Self {
        Self {
            pos: quantizer.dequantize(record.pos),
            vel: Vec3::new(
                acvd_proto::dequantize_vel(record.vel[0]),
                acvd_proto::dequantize_vel(record.vel[1]),
                acvd_proto::dequantize_vel(record.vel[2]),
            ),
            yaw: acvd_proto::dequantize_yaw(record.yaw),
            pitch: acvd_proto::dequantize_pitch(record.pitch),
            flags: record.flags,
            pose: record.pose_state,
            combat_flags: record.combat_flags,
            ap: record.ap,
            en: record.en,
        }
    }
}

/// One buffer per remote AC keyed by server tick.
#[derive(Clone, Debug, Default)]
pub struct RemoteBuffer {
    samples: BTreeMap<u32, RemoteSample>,
    /// Newest tick ever inserted (extrapolation is allowed only forward from here).
    newest: Option<u32>,
}

impl RemoteBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, tick: u32, sample: RemoteSample) {
        // Snapshots may arrive out of order; a duplicate tick keeps the first copy.
        self.samples.entry(tick).or_insert(sample);
        self.newest = Some(self.newest.map_or(tick, |n| n.max(tick)));
    }

    /// Drops samples older than `before`, keeping one before it for interpolation.
    pub fn prune(&mut self, before: u32) {
        while self.samples.len() > 2 {
            let Some((&oldest, _)) = self.samples.iter().next() else {
                break;
            };
            if oldest + 1 < before {
                self.samples.remove(&oldest);
            } else {
                break;
            }
        }
    }

    /// Samples the buffer at `render_tick`.
    pub fn sample(&self, render_tick: u32) -> Option<SampledView> {
        let (&first_tick, first) = self.samples.iter().next()?;
        let (&last_tick, last) = self.samples.iter().next_back()?;
        if render_tick <= first_tick {
            return Some(SampledView::from_sample(*first, false));
        }
        if render_tick >= last_tick {
            let ahead = render_tick - last_tick;
            let extra = ahead.min(MAX_EXTRAPOLATE_TICKS) as f32;
            return Some(SampledView {
                pos: last.pos + last.vel * extra,
                vel: last.vel,
                yaw: last.yaw,
                pitch: last.pitch,
                flags: last.flags,
                pose: last.pose,
                combat_flags: last.combat_flags,
                extrapolated: ahead > 0,
            });
        }
        let (&a_tick, a) = self
            .samples
            .range(..=render_tick)
            .next_back()
            .unwrap_or((&first_tick, first));
        let (&b_tick, b) = self
            .samples
            .range(render_tick..)
            .next()
            .unwrap_or((&last_tick, last));
        if b_tick <= a_tick {
            return Some(SampledView::from_sample(*b, false));
        }
        let t = (render_tick - a_tick) as f32 / (b_tick - a_tick) as f32;
        Some(SampledView {
            pos: a.pos.lerp(b.pos, t),
            vel: a.vel.lerp(b.vel, t),
            yaw: short_arc_lerp(a.yaw, b.yaw, t),
            pitch: a.pitch + (b.pitch - a.pitch) * t,
            flags: if t < 0.5 { a.flags } else { b.flags },
            pose: if t < 0.5 { a.pose } else { b.pose },
            combat_flags: if t < 0.5 {
                a.combat_flags
            } else {
                b.combat_flags
            },
            extrapolated: false,
        })
    }
}

/// One remote AC's rendered state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampledView {
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub flags: u16,
    pub pose: u8,
    pub combat_flags: u8,
    pub extrapolated: bool,
}

impl SampledView {
    fn from_sample(sample: RemoteSample, extrapolated: bool) -> Self {
        Self {
            pos: sample.pos,
            vel: sample.vel,
            yaw: sample.yaw,
            pitch: sample.pitch,
            flags: sample.flags,
            pose: sample.pose,
            combat_flags: sample.combat_flags,
            extrapolated,
        }
    }
}

/// Shortest-arc angle interpolation.
pub fn short_arc_lerp(a: f32, b: f32, t: f32) -> f32 {
    let diff = acvd_proto::wrap_angle(b - a);
    a + diff * t
}

/// Estimated server tick from the last snapshot plus elapsed wall time.
pub fn estimated_tick(last_tick: u32, since_last: f32) -> u32 {
    last_tick + (since_last.max(0.0) * 60.0).floor() as u32
}

/// Render tick for a given estimated server tick.
pub fn render_tick(server_tick: u32) -> u32 {
    server_tick.saturating_sub(INTERP_DELAY_TICKS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pos: Vec3, vel: Vec3, yaw: f32) -> RemoteSample {
        RemoteSample {
            pos,
            vel,
            yaw,
            pitch: 0.0,
            flags: 0,
            pose: 0,
            combat_flags: 0,
            ap: 0,
            en: 0,
        }
    }

    #[test]
    fn interpolates_between_ticks() {
        let mut buf = RemoteBuffer::new();
        buf.insert(10, sample(Vec3::ZERO, Vec3::ZERO, 3.0));
        buf.insert(12, sample(Vec3::new(4.0, 0.0, 0.0), Vec3::ZERO, -3.0));
        let view = buf.sample(11).expect("sample");
        assert!((view.pos - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4);
        // Shortest arc from 3.0 to -3.0 is through pi, not back through 0.
        assert!(
            (view.yaw.abs() - std::f32::consts::PI).abs() < 1e-3,
            "{}",
            view.yaw
        );
        assert_eq!(view.flags, 0);
    }

    #[test]
    fn extrapolates_at_most_100ms_then_holds() {
        let mut buf = RemoteBuffer::new();
        buf.insert(10, sample(Vec3::ZERO, Vec3::new(0.5, 0.0, 0.0), 0.0));
        let at_14 = buf.sample(14).expect("extrapolate");
        assert!(at_14.extrapolated);
        assert!((at_14.pos.x - 2.0).abs() < 1e-4, "4 ticks of 0.5 m");
        let at_18 = buf.sample(18).expect("extrapolate");
        assert!(at_18.extrapolated);
        assert!(
            (at_18.pos.x - 3.0).abs() < 1e-4,
            "capped at 6 ticks of 0.5 m"
        );
        let at_40 = buf.sample(40).expect("hold");
        assert!(
            (at_40.pos.x - 3.0).abs() < 1e-4,
            "stops at 6 ticks (100 ms)"
        );
    }

    #[test]
    fn loss_and_jitter_do_not_snap_the_render_position() {
        let mut buf = RemoteBuffer::new();
        // 0.25 m/tick motion, snapshots at irregular ticks (loss + jitter).
        for tick in [10, 11, 14, 15, 19, 20, 21, 27, 30] {
            buf.insert(
                tick,
                sample(
                    Vec3::new(0.25 * tick as f32, 0.0, 0.0),
                    Vec3::new(0.25, 0.0, 0.0),
                    0.0,
                ),
            );
        }
        let mut last: Option<SampledView> = None;
        let mut max_step = 0.0f32;
        for server_tick in 10..=32 {
            let view = buf.sample(render_tick(server_tick)).unwrap();
            if let Some(prev) = last {
                max_step = max_step.max((view.pos - prev.pos).length());
            }
            last = Some(view);
        }
        assert!(
            max_step <= 0.25 + 1e-3,
            "render position stepped {max_step} m in one tick"
        );
    }

    #[test]
    fn reduced_far_records_still_fill_position_and_flags() {
        use acvd_proto::{ac_flags, quantize_yaw, PosQuantizer};
        let q = PosQuantizer::new(Vec3::splat(-100.0), Vec3::splat(100.0));
        let record = AcRecord {
            net_id: acvd_proto::NetId::new(3).unwrap(),
            pos: q.quantize(Vec3::new(12.0, 3.0, -4.0)),
            vel: [0, 0, 0],
            yaw: quantize_yaw(1.0),
            pitch: 0,
            flags: ac_flags::TEAM_B,
            pose_state: 2,
            combat_flags: 0,
            ap: 0,
            en: 0,
            reload: [0, 0],
            ammo: [0, 0],
        };
        let s = RemoteSample::from_record(&record, &q);
        assert!((s.pos.x - 12.0).abs() < 0.01);
        assert_eq!(s.flags & ac_flags::TEAM_B, ac_flags::TEAM_B);
    }
}
