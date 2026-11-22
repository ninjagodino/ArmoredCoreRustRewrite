//! AC TEST enemy & test-AC AI (`script/enemy/*_actest.lc`, `script/arena/actest_*.lc`).
//!
//! Drives all `m4000_actest.msb` targets using their real 360 parameter tables:
//! - Enemies (`e1020` tank, `e2030` CHheli, `e0010` ATK-MT, `e0110` SNP-MT, `e0210` DEF-MT):
//!   movement speeds & turn rates from `EnemyBasicSt`, `EnemyPerformanceSt`, `HeriBehaviorParamSt`,
//!   and `MechBehaviorParamSt`; weapon stats from `EnemyCommonSt` → `EnemyWeaponSt` →
//!   `param/enemy/parts/acvenemyparts.bin` (`0x824a14e4` → `0x824a0430`) + `enemybullet*` +
//!   `enemyweaponsfxparam.bin` + `enemyweaponsoundparam.bin`.
//! - Test ACs (`a0000`–`a0003`, designs `5011`–`5014`): movement & turn rates from
//!   `AcCtrlParam::calculate(design)` and weapons from `weapons::Armament::from_design(design)`.

use acvd_data::find;
use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::enemy::{
    ENEMY_ENEMYBASICPARAM_BIN, ENEMY_ENEMYCOMMON_BIN, ENEMY_ENEMYHERIBEHAVIORPARAM_BIN,
    ENEMY_ENEMYMECHBEHAVIORPARAM_BIN, ENEMY_ENEMYPERFORMANCEPARAM_BIN, ENEMY_ENEMYWEAPON_BIN,
};
use bevy::audio::AudioSource;
use bevy::prelude::*;

use crate::collision::{Collision, RayFilter, RAY_HALF};
use crate::control::{Pilot, Piloting};
use crate::damage::PlayerHealth;
use crate::map::MapNav;
use crate::pose::NpcMotion;
use crate::sound::Cues;
use crate::targets::{enemy_param_id, EnemyAnim, Target, TargetHealth};
use crate::weapons::{self, Armament, EnemyParts, Gun, Kind, Tracers};

const TICK_RATE: f32 = 60.0;
const KMH_TO_MS: f32 = 1.0 / 3.6;
/// Unturned models in Bevy axes face `-Z` (`control::FORWARD`).
const FORWARD: Vec3 = Vec3::NEG_Z;
/// Approximate waist/center height of the player AC aimed at by enemies.
const PLAYER_AIM_Y: f32 = 4.5;

/// Behavioral archetype matching the AC TEST Lua scripts in `private/lua360/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archetype {
    /// `e1020` (`script/enemy/e1020_actest.lua`): turns on the spot / keeps range, fires tank cannon `8120`.
    Tank,
    /// `e2030` (`script/enemy/e2020_actest.lua`): airborne altitude-oscillating heli, fires homing missile `8231`.
    Heli,
    /// `e0010` (`script/enemy/e0010_actest.lua`): fast pass-attack MT, fires EN machinegun `8000` bursts.
    AtkMt,
    /// `e0110` (`script/enemy/e0110_actest.lua`): long-range sniper MT (`8030`), retreats & fires gatling `8031` if rushed.
    SnpMt,
    /// `e0210` (`script/enemy/e0210_actest.lua`): slow advancing shield MT, dashes at <50% AP, fires gatling `8060`.
    DefMt,
    /// `a0000` / design `5011` (`script/arena/actest_acai.lua`): aggressive close-range circling AC.
    AcAtk,
    /// `a0001` / design `5012` (`script/arena/actest_def.lua`): heavy defensive tank AC keeping 100 m.
    AcDef,
    /// `a0002` / design `5013` (`script/arena/actest_snp.lua`): sniper AC circling at 300 m (150 m in infight mode).
    AcSnp,
    /// `a0003` / design `5014` (`script/arena/actest_erik.lua`): mid-range circling AC keeping 60–80 m.
    AcEirik,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponRole {
    Main,
    Sub,
    Shoulder,
}

/// One weapon mounted on an AI-controlled enemy or test AC.
#[derive(Clone, Copy, Debug)]
pub struct AiWeaponSlot {
    pub role: WeaponRole,
    pub gun: Gun,
    pub min_range: f32,
    pub max_range: f32,
    /// Maximum horizontal aim error (degrees) required before pulling the trigger (`GetMuzzleOuterAngle`).
    pub muzzle_cone_deg: f32,
    /// Rounds per burst (`1` for rifles/cannons/missiles, `4..8` for `Op_ShootMachinegun`).
    pub burst_shots: u8,
    /// Extra pause between bursts in seconds (from `Op_ShootRifle` / `Op_ShootMachinegun` params).
    pub shoot_wait_secs: f32,
    pub burst_left: u8,
    pub burst_timer: f32,
    pub wait_timer: f32,
}

impl AiWeaponSlot {
    fn new(
        role: WeaponRole,
        gun: Gun,
        min_range: f32,
        max_range: f32,
        muzzle_cone_deg: f32,
        burst_shots: u8,
        shoot_wait_secs: f32,
    ) -> Self {
        Self {
            role,
            gun,
            min_range,
            max_range,
            muzzle_cone_deg,
            burst_shots: burst_shots.max(1),
            shoot_wait_secs,
            burst_left: 0,
            burst_timer: 0.0,
            wait_timer: 0.0,
        }
    }
}

/// A single shot emitted by `EnemyAi::step` to be spawned in Bevy world space.
#[derive(Clone, Copy, Debug)]
pub struct AiShot {
    pub gun: Gun,
    pub origin: Vec3,
    pub aim: Vec3,
}

/// Deterministic output of one `EnemyAi::step` tick.
#[derive(Clone, Debug)]
pub struct AiStepOutput {
    pub velocity: Vec3,
    pub yaw: f32,
    pub target_altitude: Option<f32>,
    pub shots: Vec<AiShot>,
}

/// Per-target AI controller component attached to AC TEST enemies and test ACs.
#[derive(Component, Debug, Clone)]
pub struct EnemyAi {
    pub archetype: Archetype,
    pub init_pos: Vec3,
    pub yaw: f32,
    pub velocity: Vec3,
    pub walk_speed_ms: f32,
    pub dash_speed_ms: f32,
    pub lift_speed_ms: f32,
    pub turn_rate_rad_s: f32,
    pub airborne: bool,
    pub muzzle_height: f32,
    pub visible_dist: f32,
    pub visible_half_angle_rad: f32,
    pub hearing_dist: f32,
    pub control_disable_dist: f32,
    pub start_stop_secs: f32,
    pub elapsed: f32,
    pub engaged: bool,
    pub sub_state: u8,
    pub state_timer: f32,
    pub strafe_sign: f32,
    pub patrol_yaw: f32,
    pub rng: u32,
    pub weapons: Vec<AiWeaponSlot>,
    /// Current `HNAV` navmesh waypoint path in Bevy world space (grounded units).
    pub nav_path: Vec<Vec3>,
    pub nav_index: usize,
    pub nav_repath_timer: f32,
    pub nav_last_goal: Vec3,
}

impl EnemyAi {
    /// Builds an `EnemyAi` for a non-AC enemy (`e1020`, `e2030`, `e0010`, `e0110`, `e0210`) from
    /// its `EnemyCommonSt`, `EnemyBasicSt`, `EnemyPerformanceSt`, `EnemyWeaponSt`,
    /// `HeriBehaviorParamSt` / `MechBehaviorParamSt`, and `acvenemyparts.bin`.
    pub fn for_enemy(model: &str, init_pos: Vec3, init_yaw: f32, enemy_parts: &EnemyParts) -> Self {
        let param_id = enemy_param_id(model).unwrap_or(0);
        let common = find(ENEMY_ENEMYCOMMON_BIN, param_id).map(|r| &r.data);
        let basic_id = common.map_or(param_id, |c| c.basic_param_id);
        let perf_id = common.map_or(param_id, |c| u32::from(c.performance_id));
        let basic = find(ENEMY_ENEMYBASICPARAM_BIN, basic_id)
            .or_else(|| find(ENEMY_ENEMYBASICPARAM_BIN, 0))
            .map(|r| &r.data);
        let perf = find(ENEMY_ENEMYPERFORMANCEPARAM_BIN, perf_id).map(|r| &r.data);

        let archetype = match param_id {
            1020 | 1021 => Archetype::Tank,
            2020..=2040 => Archetype::Heli,
            10..=45 => Archetype::AtkMt,
            110..=145 => Archetype::SnpMt,
            210..=255 => Archetype::DefMt,
            _ => Archetype::Tank,
        };

        let (walk_kmh, dash_kmh, lift_kmh, airborne) =
            if let Some(h) = find(ENEMY_ENEMYHERIBEHAVIORPARAM_BIN, param_id).map(|r| &r.data) {
                (
                    h.horizontal_max_speed,
                    h.dash_horizontal_max_speed.max(h.horizontal_max_speed),
                    h.lift_up_max_speed,
                    matches!(archetype, Archetype::Heli),
                )
            } else if let Some(m) =
                find(ENEMY_ENEMYMECHBEHAVIORPARAM_BIN, param_id).map(|r| &r.data)
            {
                (
                    m.horizontal_max_speed,
                    m.dash_horizontal_max_speed.max(m.horizontal_max_speed),
                    0.0,
                    false,
                )
            } else {
                (40.0, 60.0, 0.0, false)
            };

        let turn_deg_s = basic
            .map(|b| f32::from(b.turn_accel_deg_per_sec).max(20.0))
            .unwrap_or(45.0);
        let muzzle_height = match archetype {
            Archetype::Tank => 2.8,
            Archetype::Heli => 0.0,
            _ => 5.0,
        };

        let resolve_slot = |ewep_row_id: u32| -> Option<Gun> {
            if ewep_row_id == 0 {
                return None;
            }
            let ewep = find(ENEMY_ENEMYWEAPON_BIN, ewep_row_id)?;
            let g = enemy_parts.weapon(ewep.data.weapon_id);
            (g.kind != Kind::Skip).then_some(g)
        };
        let w0 = common.and_then(|c| resolve_slot(u32::from(c.weapon_id_00)));
        let w1 = common.and_then(|c| resolve_slot(u32::from(c.weapon_id_01)));

        let mut weapons = Vec::new();
        let (vis_dist, vis_ang_deg, hear_dist, start_stop): (f32, f32, f32, f32) = match archetype {
            Archetype::Tank => {
                // `e1020_actest.lua`: boot/visible 300 m, cone 30 deg, start_stop 0.5 s,
                // `GetMuzzleOuterAngle < 15`, `Op_ShootRifle` wait 0.1..0.5 s.
                if let Some(g) = w0 {
                    weapons.push(AiWeaponSlot::new(
                        WeaponRole::Main,
                        g,
                        0.0,
                        300.0,
                        15.0,
                        1,
                        0.4,
                    ));
                }
                (300.0, 30.0, 60.0, 0.5)
            }
            Archetype::Heli => {
                // `e2020_actest.lua`: visible 300 m 180 deg, start_stop 0.5 s, fires
                // `WEAPON_ID__SUB` (slot 1 = `2031` missile `8231`) when `distXZ < 150`.
                if let Some(g) = w1.or(w0) {
                    weapons.push(AiWeaponSlot::new(
                        WeaponRole::Sub,
                        g,
                        0.0,
                        150.0,
                        60.0,
                        2,
                        2.5,
                    ));
                }
                (300.0, 180.0, 300.0, 0.5)
            }
            Archetype::AtkMt => {
                // `e0010_actest.lua`: pre_visible 350 m / 120 deg, pre_hearing 100 m,
                // start_stop 1.5 s, `Op_ShootMachinegun` when `distXZ < 280`.
                if let Some(g) = w0 {
                    weapons.push(AiWeaponSlot::new(
                        WeaponRole::Main,
                        g,
                        0.0,
                        280.0,
                        25.0,
                        6,
                        1.2,
                    ));
                }
                (350.0, 120.0, 100.0, 1.5)
            }
            Archetype::SnpMt => {
                // `e0110_actest.lua`: visible 500 m / 60 deg, hearing 100 m, start_stop 0.5 s.
                // `SNIPE`: main sniper cannon `8030` (`distXZ > 70`, angle < 3 deg, wait 1.0 s).
                // `ESCAPE`: sub gatling `8031` (`distXZ < 250`, front).
                if let Some(g) = w0 {
                    weapons.push(AiWeaponSlot::new(
                        WeaponRole::Main,
                        g,
                        70.0,
                        500.0,
                        5.0,
                        1,
                        1.0,
                    ));
                }
                if let Some(g) = w1 {
                    weapons.push(AiWeaponSlot::new(
                        WeaponRole::Sub,
                        g,
                        0.0,
                        250.0,
                        25.0,
                        6,
                        0.8,
                    ));
                }
                (500.0, 60.0, 100.0, 0.5)
            }
            Archetype::DefMt => {
                // `e0210_actest.lua`: visible 350 m / 60 deg, hearing 40 m, start_stop 0.5 s.
                // Main gatling `8060` when `distXZ <= 300` and angle < 15 deg.
                if let Some(g) = w0 {
                    weapons.push(AiWeaponSlot::new(
                        WeaponRole::Main,
                        g,
                        0.0,
                        300.0,
                        15.0,
                        8,
                        1.0,
                    ));
                }
                (350.0, 60.0, 40.0, 0.5)
            }
            _ => (300.0, 60.0, 80.0, 0.5),
        };

        let seed = ((init_pos.x.to_bits() ^ init_pos.z.to_bits().rotate_left(13)) | 1) ^ param_id;
        Self {
            archetype,
            init_pos,
            yaw: init_yaw,
            velocity: Vec3::ZERO,
            walk_speed_ms: (walk_kmh * KMH_TO_MS).max(0.0),
            dash_speed_ms: (dash_kmh * KMH_TO_MS).max(0.0),
            lift_speed_ms: (lift_kmh * KMH_TO_MS).max(0.0),
            turn_rate_rad_s: turn_deg_s.to_radians(),
            airborne,
            muzzle_height,
            visible_dist: vis_dist,
            visible_half_angle_rad: vis_ang_deg.to_radians(),
            hearing_dist: hear_dist,
            control_disable_dist: perf.map_or(0.0, |p| p.no_controll_dist),
            start_stop_secs: start_stop,
            elapsed: 0.0,
            engaged: false,
            sub_state: 0,
            state_timer: 0.0,
            strafe_sign: if seed & 2 == 0 { 1.0 } else { -1.0 },
            patrol_yaw: init_yaw,
            rng: seed,
            weapons,
            nav_path: Vec::new(),
            nav_index: 0,
            nav_repath_timer: 0.0,
            nav_last_goal: Vec3::ZERO,
        }
    }

    /// Builds an `EnemyAi` for an AC TEST opponent AC (`a0000`–`a0003`, designs `5011`–`5014`)
    /// from its `AcCtrlParam` and equipped `Armament`.
    pub fn for_ac(
        design_id: u16,
        design: &AcAssemblyDesignSt,
        init_pos: Vec3,
        init_yaw: f32,
    ) -> Self {
        let ctrl = AcCtrlParam::calculate(design);
        let arm = Armament::from_design(design);
        let archetype = match design_id {
            5011 => Archetype::AcAtk,
            5012 => Archetype::AcDef,
            5013 => Archetype::AcSnp,
            _ => Archetype::AcEirik,
        };

        let mut weapons = Vec::new();
        // Right arm weapon (`WEAPON_ID__ARMWEP_R`)
        if arm.hands[0].kind != Kind::Skip {
            let max_r = arm.hands[0].decay.damp_range.clamp(160.0, 500.0);
            let burst = if arm.hands[0].reload_time <= 14.0 { 5 } else { 1 };
            weapons.push(AiWeaponSlot::new(
                WeaponRole::Main,
                arm.hands[0],
                0.0,
                max_r,
                20.0,
                burst,
                0.6,
            ));
        }
        // Left arm weapon (`WEAPON_ID__ARMWEP_L`)
        if arm.hands[1].kind != Kind::Skip {
            let max_r = arm.hands[1].decay.damp_range.clamp(160.0, 500.0);
            let burst = if arm.hands[1].reload_time <= 14.0 { 5 } else { 1 };
            weapons.push(AiWeaponSlot::new(
                WeaponRole::Sub,
                arm.hands[1],
                0.0,
                max_r,
                20.0,
                burst,
                0.8,
            ));
        }
        // Shoulder missiles (`WEAPON_ID__SHOULDER`)
        if arm.shoulder.kind != Kind::Skip {
            weapons.push(AiWeaponSlot::new(
                WeaponRole::Shoulder,
                arm.shoulder,
                40.0,
                450.0,
                45.0,
                arm.shoulder.burst_count.clamp(1, 3),
                2.5,
            ));
        }

        let (vis_dist, vis_ang_deg, hear_dist): (f32, f32, f32) = match archetype {
            Archetype::AcAtk => (500.0, 30.0, 280.0),
            Archetype::AcDef | Archetype::AcSnp => (500.0, 30.0, 80.0),
            _ => (500.0, 60.0, 200.0),
        };
        let seed = ((init_pos.x.to_bits() ^ init_pos.z.to_bits().rotate_left(7)) | 1)
            ^ u32::from(design_id);
        Self {
            archetype,
            init_pos,
            yaw: init_yaw,
            velocity: Vec3::ZERO,
            walk_speed_ms: (ctrl.walk_speed * KMH_TO_MS).max(6.0),
            dash_speed_ms: (ctrl.boost_max_speed * KMH_TO_MS).max(12.0),
            lift_speed_ms: 12.0,
            turn_rate_rad_s: ctrl.turn_speed.max(45.0).to_radians(),
            airborne: false,
            muzzle_height: 5.0,
            visible_dist: vis_dist,
            visible_half_angle_rad: vis_ang_deg.to_radians(),
            hearing_dist: hear_dist,
            control_disable_dist: 0.0,
            start_stop_secs: 1.0,
            elapsed: 0.0,
            engaged: false,
            sub_state: 0,
            state_timer: 0.0,
            strafe_sign: if seed & 1 == 0 { 1.0 } else { -1.0 },
            patrol_yaw: init_yaw,
            rng: seed,
            weapons,
            nav_path: Vec::new(),
            nav_index: 0,
            nav_repath_timer: 0.0,
            nav_last_goal: Vec3::ZERO,
        }
    }

    fn next_rand(&mut self) -> f32 {
        // xorshift32 -> [0, 1)
        let mut x = self.rng.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x & 0x00ff_ffff) as f32 / 16_777_216.0
    }

    /// Steps AI perception, movement intent, facing turn, and weapon firing for `dt` seconds.
    /// `has_los` is the result of `Collision::has_los(..., RayFilter::AiLos)` (`LockCheckRay`
    /// mode 2 `0x829c5cd0` -> `0x829aacb8` `!b_look_line_penetrate`).
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        pos: Vec3,
        player_pos: Vec3,
        player_vel: Vec3,
        player_alive: bool,
        has_los: bool,
        health: &TargetHealth,
        dt: f32,
    ) -> AiStepOutput {
        self.elapsed += dt;
        let frames = dt * TICK_RATE;
        for slot in &mut self.weapons {
            slot.gun.cooldown = (slot.gun.cooldown - frames).max(0.0);
            slot.burst_timer = (slot.burst_timer - frames).max(0.0);
            slot.wait_timer = (slot.wait_timer - dt).max(0.0);
        }

        if health.destroyed || !player_alive || self.elapsed < self.start_stop_secs {
            self.velocity = Vec3::ZERO;
            return AiStepOutput {
                velocity: Vec3::ZERO,
                yaw: self.yaw,
                target_altitude: self.airborne.then_some(self.init_pos.y),
                shots: Vec::new(),
            };
        }

        let to_player = player_pos - pos;
        let to_player_xz = Vec3::new(to_player.x, 0.0, to_player.z);
        let dist_xz = to_player_xz.length();
        let dist_3d = to_player.length();

        // Respect `EnemyPerformanceSt::no_controll_dist` when non-zero.
        if self.control_disable_dist > 0.0 && dist_3d > self.control_disable_dist {
            self.velocity = Vec3::ZERO;
            return AiStepOutput {
                velocity: Vec3::ZERO,
                yaw: self.yaw,
                target_altitude: self.airborne.then_some(self.init_pos.y),
                shots: Vec::new(),
            };
        }

        let forward = Quat::from_rotation_y(self.yaw) * FORWARD;
        let dir_xz = to_player_xz.normalize_or_zero();
        let angle_to_player = if dir_xz != Vec3::ZERO {
            forward.dot(dir_xz).clamp(-1.0, 1.0).acos()
        } else {
            0.0
        };
        let desired_player_yaw = yaw_from_dir(dir_xz).unwrap_or(self.yaw);

        // Perception (`Targetting_CatchUpWithConeInSide` + `LockCheckRay` + `IsLastHitRemember`).
        let in_sight =
            has_los && dist_3d <= self.visible_dist && angle_to_player <= self.visible_half_angle_rad;
        let in_hearing = dist_3d <= self.hearing_dist;
        let hit_alert = health.last_hit_timer > 0.0;
        let max_keep_dist = self.visible_dist.max(self.hearing_dist) * 1.35;
        if in_sight || in_hearing || hit_alert {
            self.engaged = true;
        } else if self.engaged && dist_3d > max_keep_dist {
            self.engaged = false;
        }

        let mut target_yaw = self.yaw;
        let mut desired_vel = Vec3::ZERO;
        let mut target_altitude = None;
        let mut allow_roles = [false; 3]; // [Main, Sub, Shoulder]

        if !self.engaged {
            // `LOST` behavior per script.
            match self.archetype {
                Archetype::Tank => {
                    // `e1020_actest.lua`: `Op_TurnWorldDir(RandomRangeInt(-180, 180))` + `Op_Stop`.
                    self.state_timer -= dt;
                    if self.state_timer <= 0.0 {
                        self.state_timer = 4.0 + self.next_rand() * 3.0;
                        self.patrol_yaw = (self.next_rand() * 2.0 - 1.0) * std::f32::consts::PI;
                    }
                    target_yaw = self.patrol_yaw;
                }
                Archetype::Heli => {
                    target_altitude = Some(self.init_pos.y);
                }
                _ => {
                    // Return smoothly toward `init_pos` (`Op_MoveToInitPos`).
                    let to_home = Vec3::new(self.init_pos.x - pos.x, 0.0, self.init_pos.z - pos.z);
                    if to_home.length() > 12.0 {
                        let d = to_home.normalize();
                        target_yaw = yaw_from_dir(d).unwrap_or(self.yaw);
                        desired_vel = d * self.walk_speed_ms;
                    }
                }
            }
        } else {
            // Engaged combat behavior per AC TEST Lua script.
            target_yaw = if hit_alert && !in_sight && !in_hearing {
                health
                    .last_hit_from
                    .and_then(|src| yaw_from_dir(Vec3::new(src.x - pos.x, 0.0, src.z - pos.z)))
                    .unwrap_or(desired_player_yaw)
            } else {
                desired_player_yaw
            };
            let right_xz = if dir_xz != Vec3::ZERO {
                Vec3::new(-dir_xz.z, 0.0, dir_xz.x)
            } else {
                Vec3::X
            };
            let ap_rate = if health.ap_max > 0.0 {
                health.ap / health.ap_max
            } else {
                1.0
            };

            match self.archetype {
                Archetype::Tank => {
                    // `e1020_actest.lua`: turns hull to face player; holds ground (`Op_KeepDist(0,0)`
                    // inside partial sphere, gently advances if far).
                    if dist_xz > 220.0 {
                        desired_vel = dir_xz * self.walk_speed_ms;
                    }
                    allow_roles[0] = true;
                }
                Archetype::Heli => {
                    // `e2020_actest.lua`: alternates altitude every 7 s between target-relative
                    // (`player.y + 18 m`) and initial altitude; circles gently around player.
                    self.state_timer += dt;
                    if self.state_timer >= 7.0 {
                        self.state_timer = 0.0;
                        self.sub_state = if self.sub_state == 1 { 2 } else { 1 };
                    }
                    if self.sub_state == 0 {
                        self.sub_state = 1;
                    }
                    let alt = if dist_xz < 200.0 && self.sub_state == 1 {
                        (player_pos.y + 20.0).max(self.init_pos.y)
                    } else {
                        self.init_pos.y
                    };
                    target_altitude = Some(alt);
                    // Orbit smoothly at ~100 m radius so helis stay menacing & visible.
                    let radial = if dist_xz > 130.0 {
                        0.6
                    } else if dist_xz < 70.0 {
                        -0.4
                    } else {
                        0.0
                    };
                    let move_dir = (dir_xz * radial + right_xz * (0.65 * self.strafe_sign))
                        .normalize_or_zero();
                    desired_vel = move_dir * (self.walk_speed_ms * 0.35).clamp(8.0, 28.0);
                    if dist_xz < 150.0 && (player_pos.y - pos.y) > -50.0 {
                        allow_roles[1] = true;
                    }
                }
                Archetype::AtkMt => {
                    // `e0010_actest.lua`: `Op_PassAttack` (`bolt_start=70`, `bolt_side=15`,
                    // `escape=5 s`), dashing past then circling back.
                    self.state_timer += dt;
                    if self.sub_state == 0 {
                        // Approach & flank sweep.
                        let move_dir = (dir_xz + right_xz * (0.4 * self.strafe_sign))
                            .normalize_or_zero();
                        desired_vel = move_dir * self.dash_speed_ms;
                        if dist_xz < 70.0 {
                            self.sub_state = 1;
                            self.state_timer = 0.0;
                            self.strafe_sign = -self.strafe_sign;
                        }
                    } else {
                        // Escape & loop wide for up to 5 s (`TIME_OF_ESCAPE = 5`).
                        let move_dir = (-dir_xz * 0.65 + right_xz * (0.75 * self.strafe_sign))
                            .normalize_or_zero();
                        desired_vel = move_dir * self.dash_speed_ms;
                        if self.state_timer >= 5.0 || dist_xz > 160.0 {
                            self.sub_state = 0;
                            self.state_timer = 0.0;
                        }
                    }
                    if dist_xz < 280.0 {
                        allow_roles[0] = true;
                    }
                }
                Archetype::SnpMt => {
                    // `e0110_actest.lua`: `STATE_OF_SNIPE` (0) vs `STATE_OF_ESCAPE` (1).
                    if self.sub_state == 0 {
                        if dist_xz < 70.0 {
                            self.sub_state = 1;
                        }
                    } else if dist_xz > 300.0 {
                        self.sub_state = 0;
                    }
                    if self.sub_state == 0 {
                        // Sniping stance: hold ground and fire sniper cannon `8030`.
                        desired_vel = Vec3::ZERO;
                        if dist_xz > 70.0 {
                            allow_roles[0] = true;
                        }
                    } else {
                        // Escape: dash away toward 350 m while firing sub gatling `8031`.
                        desired_vel = (-dir_xz * 0.9 + right_xz * (0.3 * self.strafe_sign))
                            .normalize_or_zero()
                            * self.dash_speed_ms;
                        if dist_xz < 250.0 {
                            allow_roles[1] = true;
                        }
                    }
                }
                Archetype::DefMt => {
                    // `e0210_actest.lua`: slow walk toward 20 m while AP > 50%; dashes at 40 m below 50%.
                    let (want_dist, speed) = if ap_rate > 0.5 {
                        (20.0, self.walk_speed_ms * 0.6)
                    } else {
                        (40.0, self.dash_speed_ms)
                    };
                    if dist_xz > want_dist + 8.0 {
                        desired_vel = dir_xz * speed;
                    } else if dist_xz < want_dist - 8.0 {
                        desired_vel = -dir_xz * speed * 0.5;
                    }
                    if dist_xz <= 300.0 {
                        allow_roles[0] = true;
                    }
                }
                Archetype::AcAtk => {
                    // `actest_acai.lua`: approaches when >200 m, circles tightly when close.
                    self.state_timer += dt;
                    if self.state_timer >= 4.5 {
                        self.state_timer = 0.0;
                        self.strafe_sign = -self.strafe_sign;
                    }
                    let radial = if dist_xz > 200.0 {
                        1.0
                    } else if dist_xz > 80.0 {
                        0.45
                    } else if dist_xz < 40.0 {
                        -0.4
                    } else {
                        0.0
                    };
                    let move_dir = (dir_xz * radial + right_xz * (0.85 * self.strafe_sign))
                        .normalize_or_zero();
                    desired_vel = move_dir * self.dash_speed_ms * 0.75;
                    allow_roles = [true, true, true];
                }
                Archetype::AcDef => {
                    // `actest_def.lua`: keeps ~100 m distance (`Op_KeepDist(..., 100, 0)`).
                    let radial = if dist_xz > 115.0 {
                        1.0
                    } else if dist_xz < 85.0 {
                        -0.6
                    } else {
                        0.0
                    };
                    desired_vel = (dir_xz * radial + right_xz * (0.3 * self.strafe_sign))
                        .normalize_or_zero()
                        * self.walk_speed_ms;
                    allow_roles = [true, true, true];
                }
                Archetype::AcSnp => {
                    // `actest_snp.lua`: circles at 300 m (`MODE 0`), switches to 150 m (`MODE 1`) if rushed.
                    self.state_timer += dt;
                    if self.sub_state == 0 && dist_xz < 80.0 {
                        self.sub_state = 1;
                        self.state_timer = 0.0;
                    } else if self.sub_state == 1 && self.state_timer >= 8.0 {
                        self.sub_state = 0;
                        self.state_timer = 0.0;
                    }
                    let want = if self.sub_state == 1 { 150.0 } else { 300.0 };
                    let radial = if dist_xz > want + 20.0 {
                        0.8
                    } else if dist_xz < want - 20.0 {
                        -0.8
                    } else {
                        0.0
                    };
                    desired_vel = (dir_xz * radial + right_xz * (0.7 * self.strafe_sign))
                        .normalize_or_zero()
                        * self.dash_speed_ms
                        * 0.7;
                    allow_roles[0] = true;
                    allow_roles[1] = true;
                    allow_roles[2] = hit_alert;
                }
                Archetype::AcEirik => {
                    // `actest_erik.lua`: circles at 60–80 m (`Op_RoundKeepDist(..., 60..80)`).
                    self.state_timer += dt;
                    if self.state_timer >= 4.0 {
                        self.state_timer = 0.0;
                        self.strafe_sign = -self.strafe_sign;
                    }
                    let radial = if dist_xz > 85.0 {
                        0.8
                    } else if dist_xz < 55.0 {
                        -0.6
                    } else {
                        0.0
                    };
                    desired_vel = (dir_xz * radial + right_xz * (0.8 * self.strafe_sign))
                        .normalize_or_zero()
                        * self.dash_speed_ms
                        * 0.75;
                    allow_roles[0] = true;
                    allow_roles[1] = true;
                }
            }
        }

        // Smooth yaw turn capped by `turn_rate_rad_s`.
        let delta_yaw = wrap_angle(target_yaw - self.yaw);
        let max_step = self.turn_rate_rad_s * dt;
        self.yaw = wrap_angle(self.yaw + delta_yaw.clamp(-max_step, max_step));

        // Smooth velocity blend so movement isn't jerky.
        let accel_blend = (dt * 4.0).min(1.0);
        self.velocity = self.velocity.lerp(desired_vel, accel_blend);

        // Evaluate weapon firing (`GetMuzzleOuterAngle` + range + terrain LOS + reload/burst timers).
        let mut shots = Vec::new();
        if !has_los {
            for slot in &mut self.weapons {
                slot.burst_left = 0;
            }
        }
        if self.engaged && has_los {
            let new_forward = Quat::from_rotation_y(self.yaw) * FORWARD;
            let outer_angle_deg = if dir_xz != Vec3::ZERO {
                new_forward
                    .dot(dir_xz)
                    .clamp(-1.0, 1.0)
                    .acos()
                    .to_degrees()
            } else {
                0.0
            };
            let origin_base = pos + Vec3::Y * self.muzzle_height;
            let aim_target = player_pos + Vec3::Y * PLAYER_AIM_Y;

            for slot in &mut self.weapons {
                let role_idx = match slot.role {
                    WeaponRole::Main => 0,
                    WeaponRole::Sub => 1,
                    WeaponRole::Shoulder => 2,
                };
                if !allow_roles[role_idx]
                    || matches!(slot.gun.kind, Kind::Skip | Kind::Blade)
                    || slot.gun.remaining == 0
                {
                    continue;
                }
                let in_range = dist_3d >= slot.min_range && dist_3d <= slot.max_range;
                let aimed = outer_angle_deg <= slot.muzzle_cone_deg;

                // Start a new burst when ready.
                if slot.burst_left == 0 && in_range && aimed && slot.gun.cooldown <= 0.0 && slot.wait_timer <= 0.0 {
                    slot.burst_left = slot.burst_shots.min(slot.gun.remaining.min(u8::MAX as u16) as u8);
                    slot.burst_timer = 0.0;
                }

                // Emit one round of the active burst when `burst_timer` hits 0.
                if slot.burst_left > 0 && slot.burst_timer <= 0.0 {
                    // Slight lead prediction for moving player AC.
                    let speed = slot.gun.init_speed.max(60.0);
                    let t_flight = (dist_3d / speed).min(1.5);
                    let lead_pos = aim_target + player_vel * (t_flight * 0.65);
                    let aim_dir = (lead_pos - origin_base).normalize_or_zero();
                    if aim_dir != Vec3::ZERO {
                        let offset_side = match slot.role {
                            WeaponRole::Main => 1.2,
                            WeaponRole::Sub => -1.2,
                            WeaponRole::Shoulder => 0.0,
                        };
                        let right = new_forward.cross(Vec3::Y).normalize_or_zero();
                        let origin = origin_base + new_forward * 2.5 + right * offset_side;
                        shots.push(AiShot {
                            gun: slot.gun,
                            origin,
                            aim: aim_dir,
                        });
                        slot.gun.remaining = slot.gun.remaining.saturating_sub(1);
                        slot.burst_left = slot.burst_left.saturating_sub(1);
                        if slot.burst_left > 0 {
                            slot.burst_timer = slot.gun.burst_interval.max(4.0);
                        } else {
                            slot.gun.cooldown = slot.gun.reload_time.max(15.0);
                            slot.wait_timer = slot.shoot_wait_secs;
                        }
                    }
                }
            }
        }

        AiStepOutput {
            velocity: self.velocity,
            yaw: self.yaw,
            target_altitude,
            shots,
        }
    }

    /// Reroutes `desired_vel` along the map's `HNAV`/`HTR` navmesh for grounded enemies when
    /// line-of-sight is blocked or a wall lies directly ahead.
    pub fn apply_navmesh(
        &mut self,
        pos: Vec3,
        desired_vel: Vec3,
        player_pos: Vec3,
        has_los: bool,
        nav: &MapNav,
        collision: &Collision,
        dt: f32,
    ) -> Vec3 {
        if self.airborne || !nav.is_loaded() {
            return desired_vel;
        }
        let speed = Vec2::new(desired_vel.x, desired_vel.z).length();
        if speed < 0.5 {
            self.nav_path.clear();
            self.nav_index = 0;
            return desired_vel;
        }
        self.nav_repath_timer = (self.nav_repath_timer - dt).max(0.0);

        let dir_xz = Vec3::new(desired_vel.x, 0.0, desired_vel.z) / speed;
        // Probe 14 m ahead at chest height to detect walls/pillars in our steering direction.
        let chest = pos + Vec3::Y * 3.0;
        let blocked_ahead = !collision.has_los(chest, chest + dir_xz * 14.0, RayFilter::AiLos);
        let need_nav = (self.engaged && !has_los) || blocked_ahead;

        if !need_nav {
            self.nav_path.clear();
            self.nav_index = 0;
            return desired_vel;
        }

        let goal = if self.engaged && !has_los {
            player_pos
        } else {
            pos + dir_xz * 45.0
        };

        let goal_moved = Vec2::new(goal.x - self.nav_last_goal.x, goal.z - self.nav_last_goal.z).length() > 12.0;
        if self.nav_path.is_empty()
            || self.nav_index >= self.nav_path.len()
            || (self.nav_repath_timer <= 0.0 && goal_moved)
        {
            self.nav_path = nav.find_path(pos, goal);
            self.nav_index = 0;
            self.nav_last_goal = goal;
            self.nav_repath_timer = 0.75;
        }

        // Advance past waypoints within 6 m XZ.
        while self.nav_index < self.nav_path.len() {
            let wp = self.nav_path[self.nav_index];
            let d = Vec2::new(wp.x - pos.x, wp.z - pos.z).length();
            if d < 6.0 && self.nav_index + 1 < self.nav_path.len() {
                self.nav_index += 1;
            } else {
                break;
            }
        }

        if let Some(&wp) = self.nav_path.get(self.nav_index) {
            let to_wp = Vec3::new(wp.x - pos.x, 0.0, wp.z - pos.z);
            if let Some(nav_dir) = to_wp.try_normalize() {
                return nav_dir * speed;
            }
        }
        desired_vel
    }
}

/// Bevy update system driving all living AC TEST enemies and test ACs.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update(
    time: Res<Time>,
    piloting: Res<Piloting>,
    garage: Res<crate::Garage>,
    collision: Res<Collision>,
    nav: Res<MapNav>,
    tracers: Res<Tracers>,
    mut cues: ResMut<Cues>,
    mut audio_sources: ResMut<Assets<AudioSource>>,
    mut commands: Commands,
    player_q: Query<(Entity, &Transform, &Pilot, &PlayerHealth), Without<Target>>,
    mut enemies: Query<
        (
            &mut Transform,
            &mut EnemyAi,
            &TargetHealth,
            Option<&mut EnemyAnim>,
            Option<&mut NpcMotion>,
        ),
        With<Target>,
    >,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Ok((player_entity, player_tf, pilot, player_health)) = player_q.single() else {
        return;
    };
    let player_pos = player_tf.translation;
    let player_vel = pilot.velocity * TICK_RATE;
    let player_alive = !player_health.destroyed;

    for (mut tf, mut ai, health, enemy_anim, npc_motion) in &mut enemies {
        if health.destroyed {
            continue;
        }
        let prev_yaw = ai.yaw;
        let has_los = collision.has_los(
            tf.translation + Vec3::Y * ai.muzzle_height,
            player_pos + Vec3::Y * PLAYER_AIM_Y,
            RayFilter::AiLos,
        );
        let out = ai.step(
            tf.translation,
            player_pos,
            player_vel,
            player_alive,
            has_los,
            health,
            dt,
        );

        let nav_vel = ai.apply_navmesh(
            tf.translation,
            out.velocity,
            player_pos,
            has_los,
            &nav,
            &collision,
            dt,
        );
        let mut next = tf.translation + nav_vel * dt;
        if ai.airborne {
            if let Some(target_alt) = out.target_altitude {
                let max_lift = ai.lift_speed_ms.max(6.0) * dt;
                let dy = (target_alt - next.y).clamp(-max_lift, max_lift);
                next.y += dy;
            }
            // Keep airborne helis above terrain.
            if let Some(g) = collision.ground_below(next + Vec3::Y * RAY_HALF) {
                next.y = next.y.max(g + 8.0);
            }
        } else {
            // Slide grounded units smoothly against map walls and snap to terrain height.
            collision.push_out(&mut next);
            if let Some(g) = collision.ground_below(next + Vec3::Y * 12.0) {
                if (g - next.y).abs() <= 16.0 {
                    next.y = g;
                }
            }
        }
        tf.translation = next;
        tf.rotation = Quat::from_rotation_y(out.yaw);

        // Compute local-space velocity & aim angles for skeletal animation + turret/core tracking.
        let inv_rot = Quat::from_rotation_y(-out.yaw);
        let local_vel = inv_rot * nav_vel;
        let yaw_rate = wrap_angle(out.yaw - prev_yaw) / dt.max(1e-4);
        let aim_yaw_pitch = if ai.engaged && player_alive {
            let rel = (player_pos + Vec3::Y * PLAYER_AIM_Y) - (next + Vec3::Y * ai.muzzle_height);
            let local_rel = inv_rot * rel;
            let horiz = Vec2::new(local_rel.x, local_rel.z).length().max(1e-3);
            let rel_yaw = (-local_rel.x).atan2(-local_rel.z);
            let rel_pitch = local_rel.y.atan2(horiz);
            Some((rel_yaw, rel_pitch))
        } else {
            None
        };

        if let Some(mut anim) = enemy_anim {
            anim.select_state(
                ai.archetype,
                local_vel,
                ai.walk_speed_ms,
                yaw_rate,
                ai.sub_state,
                health.is_staggered(),
                !out.shots.is_empty(),
                dt,
            );
            anim.step(dt, aim_yaw_pitch);
            anim.prev_yaw = out.yaw;
        }
        if let Some(mut npc) = npc_motion {
            npc.set_locomotion(&garage.disc, local_vel, ai.walk_speed_ms, aim_yaw_pitch);
        }

        for shot in out.shots {
            weapons::spawn_enemy_shot(
                &mut commands,
                &tracers,
                &mut cues,
                &mut audio_sources,
                &shot.gun,
                shot.origin,
                shot.aim,
                Some(player_entity),
            );
        }
    }
}

/// Computes Bevy Y-rotation yaw (radians) whose `FORWARD` (`-Z`) points along horizontal `dir`.
pub fn yaw_from_dir(dir: Vec3) -> Option<f32> {
    let h = Vec2::new(dir.x, dir.z);
    if h.length_squared() < 1.0e-6 {
        return None;
    }
    Some((-h.x).atan2(-h.y))
}

/// Extracts Y-rotation yaw (radians) from a world `Quat` for `-Z` forward.
pub fn yaw_from_quat(rot: Quat) -> f32 {
    let fwd = rot * FORWARD;
    yaw_from_dir(fwd).unwrap_or(0.0)
}

fn wrap_angle(mut a: f32) -> f32 {
    while a > std::f32::consts::PI {
        a -= std::f32::consts::TAU;
    }
    while a < -std::f32::consts::PI {
        a += std::f32::consts::TAU;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
    use acvd_formats::vfs::{self, Disc};

    fn load_enemy_parts() -> Option<EnemyParts> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let disc = Disc::open(&vfs::default_disc(&root)).ok()?;
        Some(EnemyParts::load(&disc))
    }

    #[test]
    fn enemy_weapons_resolve_from_acvenemyparts_and_fire_in_range() {
        let Some(parts) = load_enemy_parts() else {
            return;
        };
        // Tank e1020 -> weapon 8120 (KE 800, recoil 800, 100 m/s).
        let mut tank = EnemyAi::for_enemy("e1020", Vec3::ZERO, 0.0, &parts);
        assert_eq!(tank.archetype, Archetype::Tank);
        assert_eq!(tank.weapons.len(), 1);
        assert_eq!(tank.weapons[0].gun.damage_power, 800.0);
        assert_eq!(tank.weapons[0].gun.damage_recoil, 800.0);
        assert_eq!(tank.weapons[0].gun.init_speed, 100.0);

        // Heli e2030 -> weapon 8231 (CE 200 homing missile, bullet 3630).
        let heli = EnemyAi::for_enemy("e2030", Vec3::new(0.0, 40.0, 0.0), 0.0, &parts);
        assert_eq!(heli.archetype, Archetype::Heli);
        assert!(heli.airborne);
        assert_eq!(heli.weapons.len(), 1);
        assert_eq!(heli.weapons[0].gun.kind, Kind::Missile);
        assert_eq!(heli.weapons[0].gun.damage_power, 200.0);

        // ATK-MT e0010 -> weapon 8000 (TE 500 EN machinegun).
        let atk = EnemyAi::for_enemy("e0010", Vec3::ZERO, 0.0, &parts);
        assert_eq!(atk.weapons[0].gun.kind, Kind::Energy);
        assert_eq!(atk.weapons[0].gun.damage_power, 500.0);

        // SNP-MT e0110 -> main sniper 8030 (KE 3000, recoil 1200) + sub gatling 8031.
        let snp = EnemyAi::for_enemy("e0110", Vec3::ZERO, 0.0, &parts);
        assert_eq!(snp.weapons.len(), 2);
        assert_eq!(snp.weapons[0].gun.damage_power, 3000.0);
        assert_eq!(snp.weapons[0].gun.damage_recoil, 1200.0);
        assert_eq!(snp.weapons[1].gun.damage_power, 70.0);

        // DEF-MT e0210 -> gatling 8060 (KE 400).
        let def = EnemyAi::for_enemy("e0210", Vec3::ZERO, 0.0, &parts);
        assert_eq!(def.weapons.len(), 1);
        assert_eq!(def.weapons[0].gun.damage_power, 400.0);

        // Place player 120 m directly ahead (-Z): tank wakes up after start_stop (0.5 s) and fires!
        let hp = TargetHealth::from_enemy("e1020");
        let mut fired = 0usize;
        for _ in 0..60 {
            let out = tank.step(
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, -120.0),
                Vec3::ZERO,
                true,
                true,
                &hp,
                1.0 / 30.0,
            );
            fired += out.shots.len();
        }
        assert!(tank.engaged, "tank should engage player inside 300 m cone");
        assert!(fired >= 1, "tank should fire at player inside 300 m");
    }

    #[test]
    fn snp_mt_switches_to_escape_and_fires_sub_gatling_when_rushed() {
        let Some(parts) = load_enemy_parts() else {
            return;
        };
        let mut snp = EnemyAi::for_enemy("e0110", Vec3::ZERO, 0.0, &parts);
        let hp = TargetHealth::from_enemy("e0110");
        // Rush player to 40 m (< 70 m DIST_OF_MAIN_SHOOT): SNP-MT enters ESCAPE (sub_state == 1),
        // backs away (+Z), and fires sub gatling 8031 (70 KE).
        let mut sub_shots = 0usize;
        let mut backed_away = false;
        for _ in 0..60 {
            let out = snp.step(
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, -40.0),
                Vec3::ZERO,
                true,
                true,
                &hp,
                1.0 / 30.0,
            );
            if out.velocity.z > 1.0 {
                backed_away = true;
            }
            for s in out.shots {
                if (s.gun.damage_power - 70.0).abs() < 1e-3 {
                    sub_shots += 1;
                }
            }
        }
        assert_eq!(snp.sub_state, 1);
        assert!(backed_away, "rushed SNP-MT should dash away from player");
        assert!(sub_shots >= 1, "rushed SNP-MT should fire sub gatling 8031");
    }

    #[test]
    fn test_ac_ai_uses_equipped_weapons_and_strafes() {
        for design_id in [5011u16, 5012, 5013, 5014] {
            let design = AC_ASSEMBLY_DESIGN_ST_FILES
                .iter()
                .flat_map(|(_, rows)| rows.iter())
                .find(|r| r.id == u32::from(design_id))
                .map(|r| &r.data)
                .expect("test AC design");
            let mut ai = EnemyAi::for_ac(design_id, design, Vec3::ZERO, 0.0);
            assert!(
                !ai.weapons.is_empty(),
                "test AC {design_id} should resolve equipped weapons"
            );
            let hp = TargetHealth::from_ac(design);
            let mut moved = false;
            let mut fired = 0usize;
            for _ in 0..90 {
                let out = ai.step(
                    Vec3::ZERO,
                    Vec3::new(0.0, 0.0, -120.0),
                    Vec3::ZERO,
                    true,
                    true,
                    &hp,
                    1.0 / 30.0,
                );
                if out.velocity.length() > 1.0 {
                    moved = true;
                }
                fired += out.shots.len();
            }
            assert!(ai.engaged, "test AC {design_id} should engage");
            assert!(moved, "test AC {design_id} should move/strafe");
            assert!(fired >= 1, "test AC {design_id} should fire at player");
        }
    }

    #[test]
    fn enemy_ai_holds_fire_when_los_occluded_by_wall() {
        use crate::collision::Hit;
        let Some(parts) = load_enemy_parts() else {
            return;
        };
        // Solid building wall at z = -60 m between tank (z = 0) and player (z = -120 m).
        let wall = Collision::with_hits(
            Vec::new(),
            vec![
                Hit::solid(
                    Vec3::new(-30.0, 0.0, -60.0),
                    Vec3::new(30.0, 0.0, -60.0),
                    Vec3::new(-30.0, 30.0, -60.0),
                ),
                Hit::solid(
                    Vec3::new(30.0, 0.0, -60.0),
                    Vec3::new(30.0, 30.0, -60.0),
                    Vec3::new(-30.0, 30.0, -60.0),
                ),
            ],
        );
        let mut tank = EnemyAi::for_enemy("e1020", Vec3::ZERO, 0.0, &parts);
        let hp = TargetHealth::from_enemy("e1020");
        let player_pos = Vec3::new(0.0, 0.0, -120.0);
        let occluded_los = wall.has_los(
            Vec3::Y * tank.muzzle_height,
            player_pos + Vec3::Y * PLAYER_AIM_Y,
            RayFilter::AiLos,
        );
        assert!(!occluded_los, "wall must block AI LOS");

        // Even if already alerted (engaged), enemy must NOT fire through the wall.
        tank.engaged = true;
        let mut blocked_shots = 0usize;
        for _ in 0..60 {
            let out = tank.step(
                Vec3::ZERO,
                player_pos,
                Vec3::ZERO,
                true,
                occluded_los,
                &hp,
                1.0 / 30.0,
            );
            blocked_shots += out.shots.len();
        }
        assert_eq!(blocked_shots, 0, "occluded enemy must not fire through walls");

        // Stepping out from behind cover (x = 70 m -> crosses z = -60 at x = 35 m > 30 m) restores LOS.
        let exposed_pos = Vec3::new(70.0, 0.0, -120.0);
        let clear_los = wall.has_los(
            Vec3::Y * tank.muzzle_height,
            exposed_pos + Vec3::Y * PLAYER_AIM_Y,
            RayFilter::AiLos,
        );
        assert!(clear_los, "stepping past wall restores AI LOS");
        let mut clear_shots = 0usize;
        for _ in 0..90 {
            let out = tank.step(
                Vec3::ZERO,
                exposed_pos,
                Vec3::ZERO,
                true,
                clear_los,
                &hp,
                1.0 / 30.0,
            );
            clear_shots += out.shots.len();
        }
        assert!(clear_shots >= 1, "enemy should fire once LOS is clear");
    }

    #[test]
    fn navmesh_routes_grounded_enemy_across_m4000() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let nav = MapNav::load(&disc, "m4000").expect("load m4000 navmesh");
        assert!(nav.is_loaded());
        assert_eq!(nav.region_count(), 607);

        let parts = EnemyParts::load(&disc);
        // Bevy coordinates for m4000 (X mirrored from FLVER).
        let start = Vec3::new(324.65, 29.0, 649.5);
        let goal = Vec3::new(620.0, 29.0, 520.0);
        let mut mt = EnemyAi::for_enemy("e0210", start, 0.0, &parts);
        mt.engaged = true;
        let collision = Collision::with_hits(Vec::new(), Vec::new());

        let routed = mt.apply_navmesh(
            start,
            Vec3::new(10.0, 0.0, 0.0),
            goal,
            false, // occluded -> triggers navmesh pathfinding
            &nav,
            &collision,
            0.05,
        );
        assert!(
            !mt.nav_path.is_empty(),
            "grounded enemy without LOS should compute HNAV waypoints"
        );
        assert!(
            routed.length() > 5.0 && routed.is_finite(),
            "routed velocity should maintain movement speed: {routed}"
        );
    }
}
