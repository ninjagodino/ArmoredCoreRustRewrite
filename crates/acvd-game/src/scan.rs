//! Scan Mode, Target Scan, and Recon Units (`AC + 0xdb0`, `0x828bbb00`, `0x825a7c10`, `0x828bca28`).
//!
//! Matches the 360 runtime pipeline:
//! - **Mode toggle** (`AC + 0xdb0`, `0x828bbb00` at `0x828bbb78..0x828bbbc0`, read by `0x828b41a0`):
//!   toggles between Combat Mode (`false`) and Scan Mode (`true`) on `Z` / `Select` / `R3`
//!   (`manual.fmg` 20301/20407). Entering Scan Mode switches passive EN drain from `0x8286a4c0`
//!   to `0x8286a558` (`NewAcBehavior` fields 319–331, `0x828bef18` at `0x828bf6d4..0x828bf6f4`),
//!   recharging EN much faster, and disables weapon fire (`0x828b51e0` → `0x82879d58(_, 0)`).
//! - **Target Scan** (`0x828bbb00` + `0x825a7c10` + `AcCtrlParamCalc.lua` lines 315–317):
//!   uses a `NewAcBehavior +0x454` (`13.0°`) half-angle scan cone out to `head.camera_ability`
//!   (`+0x140`, `0x824791d8`, 508–812 m). Scanning a target in the cone advances `elapsed` toward
//!   `scan_complete_secs = scanCompleteFrame / 60` (Lua param 148 = `120 * (1 + (100 - head.scan_performance) / 100)`).
//!   Holding `F` / RMB / `R2` in Scan Mode completes and displays `ACV_Scan_Cur_EnemyParamInfo`
//!   (AP, KE/CE/TE defense thresholds) for `scan_display_secs` (`NewAcBehavior` field 279 = `10.0` s).
//! - **Recon deployment** (`0x828bca28`, `0x82a0d698`, `bullet/bulletrecon.bin`, `manual.fmg` 20406):
//!   press `B` / `GamepadButton::East` (○) to deploy a recon unit from `AcAssemblyDesignSt::ob`
//!   (slot 9, category 13 `recon`). Core `+0x114` (`0x824760a8`) sets carried capacity (`20–90`),
//!   recon `+0x114` (`0x8247b340`) sets max simultaneously active recons (`1–8`), recon `+0x116`
//!   (`0x824780a8`) sets reload cooldown (`20` or `40` ticks), and `BULLET_BULLETRECON_BIN`
//!   (looked up by recon `+0xd4` `bullet_id`, `0x82477fc8`) sets `recon_type` (`1` Sticky,
//!   `2` Follow, `3` UAV, `4` Hover), `life_time_frame` (`0x82478148`), and `effective_range`
//!   (`0x824780e8`, 220–500 m). Targets inside any active recon's `effective_range` are marked
//!   on the HUD (`ACV_MapScan_ReconInfo*`, `ACV_MapScan_EspialMachine*`, `FE_ReconRaderMode`).

use std::collections::HashSet;

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::bullet::{BulletReconSt, BULLET_BULLETRECON_BIN};
use acvd_data::{find, part_field};
use acvd_render::Packs;
use bevy::prelude::*;

use crate::collision::{Collision, RayFilter};
use crate::control::{Held, Pilot, Piloting};
use crate::fcs;
use crate::targets::{Target, TargetHealth};

/// Game tick rate (`DAT_82004408` = `1/60`).
const TICK_RATE: f32 = 60.0;
/// Approximate AC waist/core height above feet (recon launch / scan cone origin).
const AC_ORIGIN_Y: f32 = 4.5;
/// Follow-type recon hover offset above the AC (`manual.fmg` 20406).
const FOLLOW_HOVER_Y: f32 = 14.0;
/// Maximum simultaneous HUD recon slots (highest `max_simultaneous` across all 9 parts is 8).
pub const MAX_RECON_SLOTS: usize = 8;

/// Recon flight/deployment behavior (`BULLET_RECON_ST::recon_type`, `+0x20`, `0x824781a8`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconType {
    /// `1`: launched forward along aim, falls with gravity, sticks to ground/walls.
    Sticky,
    /// `2`: launched upward, brakes, then follows above the AC.
    Follow,
    /// `3`: UAV / camera recon (launches upward, brakes, hovers high).
    Uav,
    /// `4`: floating recon (launches upward, brakes, hovers in place).
    Hover,
}

impl ReconType {
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            2 => Self::Follow,
            3 => Self::Uav,
            4 => Self::Hover,
            _ => Self::Sticky,
        }
    }
}

/// Static recon unit specification loaded from `AcAssemblyDesignSt::ob` (category 13),
/// `AcAssemblyDesignSt::core` (`recon_capacity`), and `BULLET_BULLETRECON_BIN` (`0x828bca28`).
#[derive(Clone, Copy, Debug)]
pub struct ReconSpec {
    pub part_id: i16,
    #[allow(dead_code)]
    pub bullet_id: u32,
    pub recon_type: ReconType,
    /// Total carried recon units (`core +0x114`, `0x824760a8` → `0x828bca28 +0xe6`).
    pub capacity: u16,
    /// Maximum simultaneously deployed recon units (`recon +0x114`, `0x8247b340`, 1..=8).
    pub max_simultaneous: u8,
    /// Cooldown between recon deployments in seconds (`recon +0x116 / 60`, `0x824780a8`).
    pub reload_secs: f32,
    /// Initial launch speed in m/s (`recon +0xf4`, `Scr_GetACVParam(13, int16 0)`).
    pub init_speed: f32,
    /// Detection radius in metres (`BulletReconSt::effective_range`, `+0x32`, `0x824780e8`).
    pub effective_range: f32,
    /// Active lifetime in seconds (`BulletReconSt::life_time_frame / 60`, `+0x30`, `0x82478148`).
    pub life_time_secs: f32,
    /// Downward gravity acceleration scale (`BulletReconSt::gravity`, `+0x24`).
    pub gravity: f32,
    /// Deceleration rate after `brake_start_secs` (`BulletReconSt::brake`, `+0x28`).
    pub brake: f32,
    /// Initial propulsion phase in seconds (`BulletReconSt::move_frame / 60`, `+0x2c`).
    pub move_secs: f32,
    /// Delay before braking starts in seconds (`BulletReconSt::brake_start_frame / 60`, `+0x2e`).
    pub brake_start_secs: f32,
}

impl ReconSpec {
    /// Builds `ReconSpec` for `design` using the exact 360 getters in `0x828bca28` / `0x82a0d698`.
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let part_id = design.ob;
        let has_recon = part_id > 0;
        let bullet_id = if has_recon {
            part_field(part_id as i64, 13, "bullet_id") as u32
        } else {
            0
        };
        let row: Option<&BulletReconSt> = (bullet_id > 0)
            .then(|| find(BULLET_BULLETRECON_BIN, bullet_id).map(|r| &r.data))
            .flatten();
        let capacity = if has_recon && design.core > 0 {
            part_field(design.core as i64, 1, "recon_capacity").max(0.0) as u16
        } else {
            0
        };
        let max_sim_raw = if has_recon {
            part_field(part_id as i64, 13, "max_simultaneous") as u8
        } else {
            0
        };
        let max_simultaneous = if has_recon {
            max_sim_raw.max(1).min(MAX_RECON_SLOTS as u8)
        } else {
            0
        };
        let reload_ticks = if has_recon {
            part_field(part_id as i64, 13, "reload_ticks").max(0.0)
        } else {
            0.0
        };
        let init_speed = if has_recon {
            part_field(part_id as i64, 13, "init_speed").max(0.0)
        } else {
            0.0
        };
        let (recon_type, effective_range, life_time_secs, gravity, brake, move_secs, brake_start_secs) =
            match row {
                Some(r) => (
                    ReconType::from_raw(r.recon_type),
                    f32::from(r.effective_range).max(1.0),
                    (f32::from(r.life_time_frame) / TICK_RATE).max(1.0),
                    r.gravity.max(0.0),
                    r.brake.max(0.0),
                    f32::from(r.move_frame) / TICK_RATE,
                    f32::from(r.brake_start_frame) / TICK_RATE,
                ),
                None => (ReconType::Sticky, 100.0, 30.0, 0.6, 0.0, 0.0, 0.0),
            };
        Self {
            part_id,
            bullet_id,
            recon_type,
            capacity,
            max_simultaneous,
            reload_secs: reload_ticks / TICK_RATE,
            init_speed,
            effective_range,
            life_time_secs,
            gravity,
            brake,
            move_secs,
            brake_start_secs,
        }
    }

    pub fn equipped(&self) -> bool {
        self.part_id > 0 && self.capacity > 0 && self.max_simultaneous > 0
    }

    /// FLVER asset path for this recon part (`model/ac/parts/recon/rc####/rc####_m.bnd.dcx|rc####.flv`).
    pub fn model_asset(&self) -> Option<String> {
        (self.part_id > 0).then(|| {
            let id = self.part_id;
            format!("model/ac/parts/recon/rc{id:04}/rc{id:04}_m.bnd.dcx|rc{id:04}.flv")
        })
    }
}

/// Target currently inside the Scan Mode sight cone (`0x825a7c10`).
#[derive(Clone, Debug)]
pub struct ScanCandidate {
    pub entity: Entity,
    #[allow(dead_code)]
    pub name: String,
    pub model: String,
    pub world_pos: Vec3,
    pub distance: f32,
    pub elapsed: f32,
    pub complete_secs: f32,
}

impl ScanCandidate {
    pub fn progress(&self) -> f32 {
        if self.complete_secs <= 0.0 {
            1.0
        } else {
            (self.elapsed / self.complete_secs).clamp(0.0, 1.0)
        }
    }

    pub fn complete(&self) -> bool {
        self.elapsed >= self.complete_secs
    }
}

/// Target portrait shown in `ACV_Scan_Cur_EnemyParamInfo` (`0x825a5040`):
/// - [`Self::Enemy`] (`tex_id = 30000 + id >= 30000`, `0x828e41c0` / `0x827b29e0` / `0x82572000`):
///   loads `image/enethumbnail/e%04d.tpf` into `EnemyPic` (`Enemy_BG` shown, `EmblemPic` hidden).
/// - [`Self::Emblem`] (`tex_id < 30000`, `0x823861d0` / `0x82305d98`, `AcAssemblyDesignSt.emblem_id`):
///   loads `image/emblem/im%04d.tpf` into `EmblemPic` (`Emblem_BG` shown, `EnemyPic` hidden).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanPortrait {
    Enemy(u32),
    Emblem(u16),
}

impl ScanPortrait {
    /// Resolves the 360 disc portrait for a target (`0x828e41f4` `sscanf(model, "e%04d")`
    /// for enemies; `AcAssemblyDesignSt.emblem_id` for AC designs).
    pub fn for_target(model: &str, ac_design_id: Option<u16>) -> Option<Self> {
        if let Some(design_id) = ac_design_id {
            let emblem_id = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
                .iter()
                .flat_map(|(_, rows)| rows.iter())
                .find(|r| r.id == u32::from(design_id))
                .map(|r| r.data.emblem_id)
                .filter(|&id| id > 0)?;
            return Some(Self::Emblem(emblem_id));
        }
        let digits = model.strip_prefix('e').or_else(|| model.strip_prefix('E'))?;
        let id = digits.parse::<u32>().ok()?;
        Some(Self::Enemy(id))
    }

    /// Disc asset path (`0x827b2a4c` / `0x8238622c` / `0x8238626c`).
    pub fn asset_path(self) -> String {
        match self {
            Self::Enemy(id) => format!("image/enethumbnail/e{id:04}.tpf"),
            Self::Emblem(id) => format!("image/emblem/im{id:04}.tpf"),
        }
    }

    /// True when this portrait uses the `EnemyPic` + `Enemy_BG` slot (`tex_id >= 30000` at
    /// `0x825a5064`), false when it uses `EmblemPic` + `Emblem_BG`.
    pub fn is_enemy(self) -> bool {
        matches!(self, Self::Enemy(_))
    }
}

/// Completed Target Scan readout displayed in `ACV_Scan_Cur_EnemyParamInfo` (`0x825a4460`).
#[derive(Clone, Debug)]
pub struct ScannedTarget {
    pub entity: Entity,
    #[allow(dead_code)]
    pub name: String,
    pub model: String,
    pub portrait: Option<ScanPortrait>,
    pub world_pos: Vec3,
    #[allow(dead_code)]
    pub distance: f32,
    pub ap: f32,
    #[allow(dead_code)]
    pub ap_max: f32,
    pub ke_def: u16,
    pub ce_def: u16,
    pub te_def: u16,
    /// Remaining display duration in seconds (`NewAcBehavior` field 279 = `10.0` s).
    pub display_remaining: f32,
}

/// Per-AC Scan Mode & Recon runtime state (`AC + 0xdb0` + `0x825a7c10` + `0x828bca28`).
#[derive(Component, Clone, Debug)]
pub struct ScanState {
    /// True while Scan Mode is active (`AC + 0xdb0 != 0`).
    pub active: bool,
    pub recon_spec: ReconSpec,
    /// Remaining carried recon units (`0x82592da8` `+0x23c`).
    pub recon_remaining: u16,
    /// Remaining deployment cooldown in seconds (`0x828bca28` `+0xe8`).
    pub recon_cooldown: f32,
    /// Target currently centered in the Scan Mode sight cone.
    pub candidate: Option<ScanCandidate>,
    /// Most recently scanned target (`ACV_Scan_Cur_EnemyParamInfo`).
    pub scanned: Option<ScannedTarget>,
    /// Unique alive targets currently inside any active recon unit's `effective_range`.
    pub recon_detected_targets: Vec<(Entity, Vec3)>,
    /// Monotonic 1-based counter for deployed recon indices.
    pub next_recon_index: u32,
}

impl ScanState {
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let recon_spec = ReconSpec::from_design(design);
        Self {
            active: false,
            recon_remaining: recon_spec.capacity,
            recon_cooldown: 0.0,
            recon_spec,
            candidate: None,
            scanned: None,
            recon_detected_targets: Vec::new(),
            next_recon_index: 1,
        }
    }

    /// True if `entity` is currently detected by any deployed recon unit.
    #[allow(dead_code)]
    pub fn is_recon_detected(&self, entity: Entity) -> bool {
        self.recon_detected_targets
            .iter()
            .any(|&(e, _)| e == entity)
    }
}

/// A deployed Recon drone in the world (`BULLET_RECON_ST`).
#[derive(Component, Clone, Debug)]
pub struct ReconUnit {
    pub index: u32,
    pub recon_type: ReconType,
    pub velocity: Vec3,
    pub age_secs: f32,
    pub life_time_secs: f32,
    pub effective_range: f32,
    pub gravity: f32,
    pub brake: f32,
    pub move_secs: f32,
    pub brake_start_secs: f32,
    pub stuck: bool,
    /// Number of alive targets inside `effective_range` of this recon unit.
    pub detected_count: usize,
}

impl ReconUnit {
    /// Pure per-frame step of a deployed recon unit's flight/hover/follow motion and lifetime.
    /// Returns `false` once expired (`age_secs >= life_time_secs`).
    pub fn step(
        &mut self,
        pos: &mut Vec3,
        ac_pos: Vec3,
        collision: &Collision,
        dt: f32,
    ) -> bool {
        self.age_secs += dt;
        if self.age_secs >= self.life_time_secs {
            return false;
        }
        match self.recon_type {
            ReconType::Sticky => {
                if !self.stuck {
                    // `BulletReconSt.gravity` is in m/s per 60 Hz tick (same scale as rigid bullets).
                    self.velocity.y -= self.gravity * TICK_RATE * dt;
                    let next = *pos + self.velocity * dt;
                    if let Some((_, hit, normal)) = collision.raycast(*pos, next, RayFilter::Bullet) {
                        *pos = hit + normal * 0.15;
                        self.velocity = Vec3::ZERO;
                        self.stuck = true;
                    } else {
                        *pos = next;
                    }
                }
            }
            ReconType::Follow => {
                if self.age_secs < self.move_secs {
                    if self.age_secs >= self.brake_start_secs && self.brake > 0.0 {
                        let damp = (1.0 - self.brake * dt * 2.0).clamp(0.0, 1.0);
                        self.velocity *= damp;
                    }
                    *pos += self.velocity * dt;
                } else {
                    // Hover and follow above the AC (`manual.fmg` 20406).
                    let goal = ac_pos + Vec3::Y * FOLLOW_HOVER_Y;
                    let alpha = (dt * 4.0).clamp(0.0, 1.0);
                    *pos = pos.lerp(goal, alpha);
                    self.velocity = Vec3::ZERO;
                }
            }
            ReconType::Uav | ReconType::Hover => {
                if !self.stuck {
                    if self.age_secs >= self.brake_start_secs && self.brake > 0.0 {
                        let speed = self.velocity.length();
                        let decel = self.brake * TICK_RATE * dt;
                        if speed <= decel {
                            self.velocity = Vec3::ZERO;
                            self.stuck = true;
                        } else {
                            self.velocity *= (speed - decel) / speed;
                        }
                    }
                    self.velocity.y -= self.gravity * TICK_RATE * dt;
                    let next = *pos + self.velocity * dt;
                    if let Some(ground_y) = collision.ground_below(next + Vec3::Y * 1.0) {
                        if next.y <= ground_y + 0.5 {
                            *pos = Vec3::new(next.x, ground_y + 0.5, next.z);
                            self.velocity = Vec3::ZERO;
                            self.stuck = true;
                        } else {
                            *pos = next;
                        }
                    } else {
                        *pos = next;
                    }
                    if self.move_secs > 0.0 && self.age_secs >= self.move_secs {
                        self.velocity = Vec3::ZERO;
                        self.stuck = true;
                    }
                }
            }
        }
        true
    }
}

/// Computes initial launch velocity for a newly deployed recon unit (`manual.fmg` 20406).
pub fn recon_launch_velocity(spec: &ReconSpec, aim_dir: Vec3) -> Vec3 {
    let dir = aim_dir.normalize_or_zero();
    let forward = if dir == Vec3::ZERO { Vec3::NEG_Z } else { dir };
    match spec.recon_type {
        ReconType::Sticky => {
            // Sticky recon fires forward along aim (capped so it lands cleanly in arena view).
            let speed = spec.init_speed.clamp(15.0, 80.0);
            (forward + Vec3::Y * 0.12).normalize() * speed
        }
        ReconType::Follow => {
            let speed = spec.init_speed.clamp(10.0, 35.0);
            (Vec3::Y * 0.9 + forward * 0.2).normalize() * speed
        }
        ReconType::Uav | ReconType::Hover => {
            let speed = spec.init_speed.clamp(8.0, 30.0);
            (Vec3::Y * 0.75 + forward * 0.65).normalize() * speed
        }
    }
}

/// Pure helper: checks whether `target_pos` is inside `effective_range` of `recon_pos`.
pub fn in_recon_range(recon_pos: Vec3, target_pos: Vec3, effective_range: f32) -> bool {
    (target_pos - recon_pos).length_squared() <= effective_range * effective_range
}

/// Per-frame Scan Mode toggle, Target Scan timer, and Recon deployment/update system.
#[allow(clippy::too_many_arguments)]
pub fn update(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    pads: Query<&Gamepad>,
    held: Res<Held>,
    piloting: Res<Piloting>,
    collision: Res<Collision>,
    garage: Res<crate::Garage>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut held_recon_fired: Local<bool>,
    mut acs: Query<(&mut Pilot, &Transform, &mut ScanState)>,
    mut recons: Query<(Entity, &mut ReconUnit, &mut Transform), (Without<Pilot>, Without<Target>)>,
    targets: Query<(Entity, &Target, &TargetHealth, &Transform), Without<ReconUnit>>,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    let Ok((mut pilot, ac_tf, mut scan)) = acs.single_mut() else {
        return;
    };

    // 1. Scan Mode toggle (`AC + 0xdb0`, `0x828bbb00`): Z / Select / R3 (`manual.fmg` 20301/20407).
    let mut toggle_scan = keys.just_pressed(KeyCode::KeyZ);
    let mut deploy_recon = keys.just_pressed(KeyCode::KeyB);
    let mut scan_trigger = keys.pressed(KeyCode::KeyF)
        || mouse.pressed(MouseButton::Right)
        || held.0.contains(&KeyCode::KeyF);
    for pad in &pads {
        toggle_scan |=
            pad.just_pressed(GamepadButton::Select) || pad.just_pressed(GamepadButton::RightThumb);
        deploy_recon |= pad.just_pressed(GamepadButton::East);
        scan_trigger |= pad.pressed(GamepadButton::RightTrigger2);
    }
    if held.0.contains(&KeyCode::KeyB) && !*held_recon_fired {
        deploy_recon = true;
        *held_recon_fired = true;
    }

    if toggle_scan {
        scan.active = !scan.active;
    }
    if held.0.contains(&KeyCode::KeyZ) {
        scan.active = true;
    }
    pilot.scan_mode = scan.active;

    // 2. Recon deployment (`B` / Gamepad East ○) — works in both Combat and Scan Mode.
    if scan.recon_cooldown > 0.0 {
        scan.recon_cooldown = (scan.recon_cooldown - dt).max(0.0);
    }
    let ac_pos = ac_tf.translation + Vec3::Y * AC_ORIGIN_Y;
    let aim_dir = pilot.aim_direction();

    if deploy_recon
        && scan.recon_spec.equipped()
        && scan.recon_remaining > 0
        && scan.recon_cooldown <= 0.0
    {
        // Enforce `max_simultaneous` (`0x8247b340`): despawn oldest active recon if at cap.
        let mut active_recons: Vec<(Entity, u32)> =
            recons.iter().map(|(e, r, _)| (e, r.index)).collect();
        active_recons.sort_by_key(|&(_, idx)| idx);
        let max_sim = usize::from(scan.recon_spec.max_simultaneous);
        while active_recons.len() >= max_sim {
            let (oldest, _) = active_recons.remove(0);
            commands.entity(oldest).try_despawn();
        }

        scan.recon_remaining = scan.recon_remaining.saturating_sub(1);
        scan.recon_cooldown = scan.recon_spec.reload_secs;
        let index = scan.next_recon_index;
        scan.next_recon_index = scan.next_recon_index.saturating_add(1);
        let spec = scan.recon_spec;
        let vel = recon_launch_velocity(&spec, aim_dir);
        spawn_recon_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &garage,
            index,
            &spec,
            ac_pos,
            vel,
        );
        info!(
            "recon #{index} deployed (part {}, {:?}, range {:.0} m, life {:.1} s, rem {})",
            spec.part_id, spec.recon_type, spec.effective_range, spec.life_time_secs, scan.recon_remaining
        );
    }

    // 3. Update active recon units & compute detected targets inside recon range.
    let mut detected_set: HashSet<Entity> = HashSet::new();
    let mut detected_list: Vec<(Entity, Vec3)> = Vec::new();
    for (entity, mut recon, mut tf) in &mut recons {
        if !recon.step(&mut tf.translation, ac_tf.translation, &collision, dt) {
            commands.entity(entity).try_despawn();
            continue;
        }
        let mut count = 0usize;
        for (t_entity, _, health, t_tf) in &targets {
            if health.destroyed {
                continue;
            }
            let center = t_tf.translation + Vec3::Y * health.center_offset_y();
            if in_recon_range(tf.translation, center, recon.effective_range) {
                count += 1;
                if detected_set.insert(t_entity) {
                    detected_list.push((t_entity, center));
                }
            }
        }
        recon.detected_count = count;
    }
    scan.recon_detected_targets = detected_list;

    // 4. Target Scan (`0x828bbb00` + `0x825a7c10`): only active while in Scan Mode.
    if !scan.active {
        scan.candidate = None;
        scan.scanned = None;
        return;
    }

    // Tick down any active scanned-target display timer and refresh live AP / position.
    if let Some(scanned) = scan.scanned.as_mut() {
        scanned.display_remaining -= dt;
        if let Ok((_, _, health, t_tf)) = targets.get(scanned.entity) {
            if health.destroyed {
                scan.scanned = None;
            } else {
                scanned.ap = health.ap;
                scanned.ap_max = health.ap_max;
                scanned.world_pos = t_tf.translation + Vec3::Y * health.center_offset_y();
                scanned.distance = (scanned.world_pos - ac_pos).length();
                if scanned.display_remaining <= 0.0 {
                    scan.scanned = None;
                }
            }
        } else {
            scan.scanned = None;
        }
    }

    // Find target closest to the center of the Scan Mode cone (`NewAcBehavior +0x454` = 13 deg,
    // range = `head.camera_ability` `0x824791d8`).
    let scan_sight = pilot.ctrl.scan_sight_rad.max(0.05);
    let scan_range = pilot.ctrl.scan_range.max(100.0);
    let datum_back = pilot.ctrl.fcs_datum_back.max(0.0);
    let cur_entity = scan.candidate.as_ref().map(|c| c.entity);

    let mut best: Option<(f32, Entity, String, String, Option<u16>, Vec3, f32, &TargetHealth)> = None;
    for (entity, target, health, t_tf) in &targets {
        if health.destroyed {
            continue;
        }
        let center = t_tf.translation + Vec3::Y * health.center_offset_y();
        let Some(ell) = fcs::in_lock_cone(
            ac_pos,
            aim_dir,
            center,
            scan_range,
            scan_sight,
            scan_sight,
            datum_back,
        ) else {
            continue;
        };
        let score = if Some(entity) == cur_entity {
            ell * 0.85
        } else {
            ell
        };
        if best.as_ref().is_none_or(|b| score < b.0) {
            let dist = (center - ac_pos).length();
            best = Some((
                score,
                entity,
                target.name.clone(),
                target.model.clone(),
                target.ac_design_id,
                center,
                dist,
                health,
            ));
        }
    }

    let Some((_, entity, name, model, ac_design_id, world_pos, distance, health)) = best else {
        scan.candidate = None;
        return;
    };

    let complete_secs = pilot.ctrl.scan_complete_secs.max(0.05);
    let prev_elapsed = match scan.candidate.as_ref() {
        Some(prev) if prev.entity == entity => prev.elapsed,
        _ => 0.0,
    };
    let elapsed = (prev_elapsed + dt).min(complete_secs);
    let candidate = ScanCandidate {
        entity,
        name: name.clone(),
        model: model.clone(),
        world_pos,
        distance,
        elapsed,
        complete_secs,
    };
    let ready = candidate.complete();
    scan.candidate = Some(candidate);

    // Holding / pressing Target Scan (`F` / RMB / `R2`) populates `ACV_Scan_Cur_EnemyParamInfo`
    // once scan progress reaches 100% (`0x825a7c10` at `0x825a8184..0x825a81c8`, `manual.fmg` 20502).
    if scan_trigger && ready {
        let portrait = ScanPortrait::for_target(&model, ac_design_id);
        scan.scanned = Some(ScannedTarget {
            entity,
            name,
            model,
            portrait,
            world_pos,
            distance,
            ap: health.ap,
            ap_max: health.ap_max,
            ke_def: health.defense.ke.threshold,
            ce_def: health.defense.ce.threshold,
            te_def: health.defense.te.threshold,
            display_remaining: pilot.ctrl.scan_display_secs.max(1.0),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_recon_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    garage: &crate::Garage,
    index: u32,
    spec: &ReconSpec,
    origin: Vec3,
    velocity: Vec3,
) {
    let unit = ReconUnit {
        index,
        recon_type: spec.recon_type,
        velocity,
        age_secs: 0.0,
        life_time_secs: spec.life_time_secs,
        effective_range: spec.effective_range,
        gravity: spec.gravity,
        brake: spec.brake,
        move_secs: spec.move_secs,
        brake_start_secs: spec.brake_start_secs,
        stuck: false,
        detected_count: 0,
    };
    let root = commands
        .spawn((
            unit,
            Name::new(format!("recon_{index}")),
            Transform::from_translation(origin),
            Visibility::default(),
        ))
        .id();

    // Try spawning the actual disc FLVER (`model/ac/parts/recon/rc####/...`) for this recon part.
    let mut loaded_flver = false;
    if let Some(asset) = spec.model_asset() {
        if let Ok(list) = acvd_render::model(&garage.disc, &asset) {
            if !list.is_empty() {
                let mut packs = Packs::default();
                let mut missing = 0usize;
                commands.entity(root).with_children(|c| {
                    for m in list {
                        let mat = acvd_render::material(
                            &garage.disc,
                            &m,
                            &mut packs,
                            images,
                            garage.flat,
                            &mut missing,
                        );
                        c.spawn((Mesh3d(meshes.add(m.mesh)), MeshMaterial3d(materials.add(mat))));
                    }
                });
                loaded_flver = true;
            }
        }
    }
    if !loaded_flver {
        commands.entity(root).with_children(|c| {
            c.spawn((
                Mesh3d(meshes.add(Sphere::new(0.45))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(0.1, 0.9, 1.0),
                    emissive: LinearRgba::rgb(0.2, 2.0, 2.5),
                    ..default()
                })),
            ));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
    use acvd_data::generated::ctrl::AcCtrlParam;

    fn design_5001() -> &'static AcAssemblyDesignSt {
        &AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .unwrap()
            .data
    }

    fn design_5010() -> &'static AcAssemblyDesignSt {
        &AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5010)
            .unwrap()
            .data
    }

    #[test]
    fn scan_mode_passive_en_drain_is_lower_than_combat_mode() {
        // Verify 0x8286a558 + NewAcBehavior fields 319-331:
        // Scan Mode cuts arms, FCS, weapons, shoulder, hanger by 100% and core by 50%,
        // so scan_passive_en_drain < passive_en_drain and net EN recharge is faster.
        for d in [design_5001(), design_5010()] {
            let ctrl = AcCtrlParam::calculate(d);
            assert!(
                ctrl.scan_passive_en_drain < ctrl.passive_en_drain,
                "scan_passive_en_drain ({}) must be < passive_en_drain ({})",
                ctrl.scan_passive_en_drain,
                ctrl.passive_en_drain
            );
            // Exact formula check for NewAcBehavior 319-331:
            // Only head (100%), core (50%), legs (100%), booster (100%), ob/recon (100%) remain.
            let expected = part_field(d.head as i64, 0, "en_consumption")
                + part_field(d.core as i64, 1, "en_consumption") * 0.5
                + part_field(d.legs as i64, 3, "en_consumption")
                + part_field(d.booster as i64, 6, "en_consumption")
                + part_field(d.ob as i64, 13, "en_consumption");
            assert!(
                (ctrl.scan_passive_en_drain - expected).abs() < 1e-3,
                "got {} vs expected {expected}",
                ctrl.scan_passive_en_drain
            );
            assert!(
                ctrl.en_output_tick - ctrl.scan_passive_en_drain_tick
                    > ctrl.en_output_tick - ctrl.passive_en_drain_tick
            );
        }
    }

    #[test]
    fn target_scan_time_and_sight_match_lua_and_new_ac_behavior() {
        let d = design_5001(); // head 1010: scan_performance = 150, camera_ability = 599
        let ctrl = AcCtrlParam::calculate(d);
        assert_eq!(part_field(d.head as i64, 0, "scan_performance"), 150.0);
        // AcCtrlParamCalc.lua line 316: 120 * (1.0 + (100 - 150) / 100) = 60 ticks = 1.0 s.
        assert!((ctrl.scan_complete_frames - 60.0).abs() < 1e-4);
        assert!((ctrl.scan_complete_secs - 1.0).abs() < 1e-4);
        // NewAcBehavior +0x454 = 13.0 deg; head 1010 camera_ability = 599 m.
        assert!((ctrl.scan_sight_rad - 13.0_f32.to_radians()).abs() < 1e-5);
        assert!((ctrl.scan_range - 599.0).abs() < 1e-4);
        assert!((ctrl.scan_display_secs - 10.0).abs() < 1e-4);
    }

    #[test]
    fn recon_spec_reads_category_13_core_capacity_and_bullet_recon() {
        // Design 5001: ob = 130 (Sticky, bullet 9130, range 270 m, life 2100 = 35 s, max_sim 3,
        // reload 40 ticks, init_speed 180), core = 230 (recon_capacity = 65).
        let spec = ReconSpec::from_design(design_5001());
        assert!(spec.equipped());
        assert_eq!(spec.part_id, 130);
        assert_eq!(spec.bullet_id, 9130);
        assert_eq!(spec.recon_type, ReconType::Sticky);
        assert_eq!(spec.capacity, 65);
        assert_eq!(spec.max_simultaneous, 3);
        assert!((spec.reload_secs - 40.0 / 60.0).abs() < 1e-5);
        assert!((spec.init_speed - 180.0).abs() < 1e-4);
        assert!((spec.effective_range - 270.0).abs() < 1e-4);
        assert!((spec.life_time_secs - 35.0).abs() < 1e-4);

        // Design 5010: ob = 111 (Sticky, bullet 9111, range 220 m, life 1800 = 30 s), core 211 (50).
        let spec10 = ReconSpec::from_design(design_5010());
        assert_eq!(spec10.part_id, 111);
        assert_eq!(spec10.bullet_id, 9111);
        assert_eq!(spec10.capacity, 50);
        assert!((spec10.effective_range - 220.0).abs() < 1e-4);
        assert!((spec10.life_time_secs - 30.0).abs() < 1e-4);

        // Check all 9 category-13 recon records resolve cleanly into BULLET_BULLETRECON_BIN.
        for &(id, expected_type) in &[
            (111, ReconType::Sticky),
            (110, ReconType::Sticky),
            (120, ReconType::Sticky),
            (130, ReconType::Sticky),
            (210, ReconType::Follow),
            (220, ReconType::Follow),
            (230, ReconType::Follow),
            (410, ReconType::Hover),
            (420, ReconType::Hover),
        ] {
            let mut d = *design_5001();
            d.ob = id;
            let s = ReconSpec::from_design(&d);
            assert!(s.equipped(), "recon {id} not equipped");
            assert_eq!(s.recon_type, expected_type, "recon {id} type");
            assert!(s.effective_range >= 220.0 && s.effective_range <= 550.0);
            assert!(s.life_time_secs >= 3.5 && s.life_time_secs <= 150.0);
        }
    }

    #[test]
    fn recon_unit_sticks_expires_and_detects_targets_in_range() {
        let collision = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: crate::collision::Layer::Ground,
            }],
            Vec::new(),
        );
        let spec = ReconSpec::from_design(design_5001());
        let mut unit = ReconUnit {
            index: 1,
            recon_type: ReconType::Sticky,
            velocity: Vec3::new(0.0, 2.0, -30.0),
            age_secs: 0.0,
            life_time_secs: 2.0,
            effective_range: spec.effective_range,
            gravity: spec.gravity,
            brake: spec.brake,
            move_secs: spec.move_secs,
            brake_start_secs: spec.brake_start_secs,
            stuck: false,
            detected_count: 0,
        };
        let mut pos = Vec3::new(0.0, 4.5, 0.0);
        for _ in 0..60 {
            assert!(unit.step(&mut pos, Vec3::ZERO, &collision, 1.0 / 60.0));
        }
        assert!(unit.stuck, "sticky recon should land and stick on ground");
        assert!(pos.y <= 0.25 && pos.z < -5.0, "landed at {pos}");

        // Detection range check (270 m).
        assert!(in_recon_range(pos, pos + Vec3::new(200.0, 0.0, 0.0), unit.effective_range));
        assert!(!in_recon_range(pos, pos + Vec3::new(300.0, 0.0, 0.0), unit.effective_range));

        // Lifetime expiry.
        assert!(!unit.step(&mut pos, Vec3::ZERO, &collision, 2.0));
    }

    #[test]
    fn follow_recon_tracks_ac_after_launch() {
        let collision = Collision::with_hits(Vec::new(), Vec::new());
        let mut unit = ReconUnit {
            index: 2,
            recon_type: ReconType::Follow,
            velocity: Vec3::new(0.0, 15.0, 0.0),
            age_secs: 0.0,
            life_time_secs: 20.0,
            effective_range: 380.0,
            gravity: 0.1,
            brake: 1.0,
            move_secs: 0.5,
            brake_start_secs: 0.0,
            stuck: false,
            detected_count: 0,
        };
        let mut pos = Vec3::new(0.0, 4.5, 0.0);
        let ac_pos = Vec3::new(50.0, 0.0, -30.0);
        for _ in 0..180 {
            assert!(unit.step(&mut pos, ac_pos, &collision, 1.0 / 60.0));
        }
        let expected = ac_pos + Vec3::Y * FOLLOW_HOVER_Y;
        assert!(
            (pos - expected).length() < 0.5,
            "follow recon at {pos} should track {expected}"
        );
    }
}
