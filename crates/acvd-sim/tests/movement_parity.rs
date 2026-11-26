//! Scripted movement parity against the pre-refactor `acvd-game` `Pilot`.
//!
//! The expected values below were printed by `control::tests::phase0_parity_baseline` on the
//! unmodified single-player code before `collision`/`damage`/`Pilot` moved into `acvd-sim`.
//! They cover boost, a High Boost burst, turning, a jump, and a glide over flat ground.

use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_sim::collision::{Collision, Layer, Plane};
use acvd_sim::input::InputFrame;
use acvd_sim::motion::{start_quick_boost, step_tick, AcMotion, GlidePhase};
use glam::{Vec2, Vec3};

/// One recorded baseline checkpoint: `(tick, pos, velocity, yaw, en, airborne, landings, phase,
/// quick_boosts, wall_jumps, kicks_started)`.
type Checkpoint = (
    u32,
    [f32; 3],
    [f32; 3],
    f32,
    f32,
    bool,
    u32,
    GlidePhase,
    u32,
    u32,
    u32,
);

const BASELINE: [Checkpoint; 9] = [
    (
        0,
        [0.0, 0.0, 10.0],
        [0.0, 0.0, 0.0],
        0.0,
        59980.0,
        false,
        0,
        GlidePhase::Ground,
        0,
        0,
        0,
    ),
    (
        20,
        [0.0, 0.0, 0.341811299],
        [0.0, 0.0, -0.577902436],
        0.0,
        59980.0,
        false,
        0,
        GlidePhase::Ground,
        0,
        0,
        0,
    ),
    (
        40,
        [0.0, 0.0, -22.741935730],
        [0.0, 0.0, -1.097658277],
        0.0,
        54715.085937500,
        false,
        1,
        GlidePhase::Ground,
        1,
        0,
        0,
    ),
    (
        60,
        [-5.730917931, 0.0, -41.402503967],
        [-0.487131417, 0.0, -0.754133046],
        0.435007244,
        56679.070312500,
        false,
        1,
        GlidePhase::Ground,
        1,
        0,
        0,
    ),
    (
        80,
        [-8.361261368, 0.0, -57.010177612],
        [0.123289533, 0.0, -0.723878086],
        -0.000000011,
        58643.054687500,
        false,
        1,
        GlidePhase::Ground,
        1,
        0,
        0,
    ),
    (
        110,
        [-7.200133801, 10.342613220, -78.151603699],
        [0.004661025, -0.005335141, -0.610887825],
        -0.000000011,
        59980.0,
        true,
        1,
        GlidePhase::Ground,
        1,
        0,
        0,
    ),
    (
        140,
        [-7.166657448, 9.958739281, -95.563247681],
        [0.000107483, -0.019775072, -0.577902436],
        -0.000000011,
        59980.0,
        true,
        1,
        GlidePhase::Ground,
        1,
        0,
        0,
    ),
    (
        180,
        [-7.166524410, 13.958754539, -138.745849609],
        [0.000000012, 0.100000001, -1.079566360],
        -0.000000011,
        57023.281250000,
        true,
        1,
        GlidePhase::Air,
        1,
        0,
        0,
    ),
    (
        220,
        [-7.166524410, 14.958739281, -181.928222656],
        [0.000000012, 0.0, -1.079566360],
        -0.000000011,
        54066.562500000,
        true,
        1,
        GlidePhase::Air,
        1,
        0,
        0,
    ),
];

fn test_ctrl() -> AcCtrlParam {
    AC_ASSEMBLY_DESIGN_ST_FILES
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .find(|r| r.id == 5001)
        .map(|r| AcCtrlParam::calculate(&r.data))
        .expect("design 5001")
}

fn flat_ground() -> Collision {
    Collision::with_hits(
        vec![Plane {
            y: 0.0,
            layer: Layer::Ground,
        }],
        Vec::new(),
    )
}

fn assert_checkpoint(tick: u32, p: &AcMotion, pos: Vec3) {
    let Some(expected) = BASELINE.iter().find(|c| c.0 == tick) else {
        return;
    };
    let (_, e_pos, e_vel, e_yaw, e_en, e_air, e_landings, e_phase, e_qb, e_wj, e_kicks) = *expected;
    let eps = 1e-4;
    for i in 0..3 {
        assert!(
            (pos[i] - e_pos[i]).abs() <= eps,
            "tick {tick}: pos[{i}] {} vs {}",
            pos[i],
            e_pos[i]
        );
        assert!(
            (p.velocity[i] - e_vel[i]).abs() <= eps,
            "tick {tick}: velocity[{i}] {} vs {}",
            p.velocity[i],
            e_vel[i]
        );
    }
    assert!(
        (p.yaw - e_yaw).abs() <= eps,
        "tick {tick}: yaw {} vs {e_yaw}",
        p.yaw
    );
    assert!(
        (p.en - e_en).abs() <= 0.01,
        "tick {tick}: EN {} vs {e_en}",
        p.en
    );
    assert_eq!(p.airborne, e_air, "tick {tick}: airborne");
    assert_eq!(p.landings, e_landings, "tick {tick}: landings");
    assert_eq!(p.glide_phase, e_phase, "tick {tick}: glide phase");
    assert_eq!(p.quick_boosts, e_qb, "tick {tick}: quick boosts");
    assert_eq!(p.wall_jumps, e_wj, "tick {tick}: wall jumps");
    assert_eq!(p.kicks_started, e_kicks, "tick {tick}: kicks");
}

#[test]
fn scripted_movement_matches_pre_refactor_baseline() {
    let ctrl = test_ctrl();
    let collision = flat_ground();
    let mut p = AcMotion::new(ctrl);
    let mut pos = Vec3::new(0.0, 0.0, 10.0);
    let mut input = InputFrame {
        stick: Vec2::Y,
        ..InputFrame::default()
    };
    p.boost = true;
    let mut tick = 0u32;
    assert_checkpoint(tick, &p, pos);

    for _ in 0..20 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    assert!(start_quick_boost(&mut p, false));
    for _ in 0..20 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    input = InputFrame {
        stick: Vec2::new(-0.5, 1.0).clamp_length_max(1.0),
        turn: 1.0,
        ..InputFrame::default()
    };
    for _ in 0..20 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    input = InputFrame {
        stick: Vec2::new(0.5, 1.0).clamp_length_max(1.0),
        turn: -1.0,
        ..InputFrame::default()
    };
    for _ in 0..20 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    input = InputFrame {
        stick: Vec2::Y,
        ..InputFrame::default()
    };
    p.jump_queued = true;
    for _ in 0..30 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    for _ in 0..30 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    p.boost = true;
    p.glide = true;
    p.glide_phase = GlidePhase::Ground;
    p.glide_phase_time = 0.0;
    p.glide_lift_started = false;
    for _ in 0..40 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);

    for _ in 0..40 {
        step_tick(&mut p, &input, &mut pos, &collision);
        tick += 1;
    }
    assert_checkpoint(tick, &p, pos);
}
