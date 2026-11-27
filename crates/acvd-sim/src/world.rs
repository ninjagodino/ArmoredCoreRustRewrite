//! Authoritative fixed-tick world: every AC, projectiles/missiles, the destructible set, and the
//! snapshot-record production shared by the offline client, the client predictor, and the
//! dedicated server.
//!
//! One `step` is one 60 Hz tick. Inputs are applied through the same validated conversion on the
//! server and on the predicting client, so reconciliation can replay exactly what the server ran.

use std::collections::{BTreeMap, BTreeSet};

use acvd_proto::{
    ac_flags, quantize_coord, quantize_dir3, quantize_pitch, quantize_vel, quantize_yaw,
    wrap_angle, AcRecord, Loadout, LocalPlayerState, MissileCorrection, PropState, WorldState,
};
use glam::Vec3;

use crate::collision::{Collision, RayFilter};
use crate::damage::{self, DamageType, HitResult};
use crate::events::{SimEvent, WeaponSlot};
use crate::ids::{NetId, ProjectileId, PropId};
use crate::input::InputFrame;
use crate::map::{AreaZone, DestructibleSet, Start, WaterReturnPoints};
use crate::motion::{
    self, AcMotion, BladeAction, BladeLaunch, BladePhase, BladeSweep, GlidePhase, KickState, QbPhase,
    WallJumpPhase,
};
use crate::stats::{self, AcStats};
use crate::tick::{Tick, TICK_DT};
use crate::weapons::{self, Armament, Missile, Projectile, Shot};

/// Default deterministic spread seed when a caller does not configure a match seed.
pub const DEFAULT_MATCH_SEED: u64 = 0x4143_5644_0000_0001;

/// Remote animation pose ids carried in `AcRecord::pose_state`. The client maps these to the
/// existing `param/acmotion.bin` clips; the simulation never names clips.
pub mod pose_state {
    pub const IDLE: u8 = 0;
    pub const WALK: u8 = 1;
    pub const DASH: u8 = 2;
    pub const JUMP: u8 = 3;
    pub const RISE: u8 = 4;
    pub const FALL: u8 = 5;
    pub const AIR_MOVE: u8 = 6;
    pub const GLIDE: u8 = 7;
    pub const GLIDE_AIR: u8 = 8;
    pub const QB: u8 = 9;
    pub const WALL_JUMP: u8 = 10;
    pub const KICK: u8 = 11;
    pub const BLADE: u8 = 12;
    pub const LAND: u8 = 13;
}

/// One simulated AC.
#[derive(Clone, Debug)]
pub struct SimAc {
    pub id: NetId,
    pub position: Vec3,
    pub motion: AcMotion,
    pub zone: AreaZone,
    pub team: u8,
    pub loadout: Loadout,
    pub stats: AcStats,
    pub armament: Armament,
    /// Absolute AP (`stats.ap_max` at spawn).
    pub ap: f32,
    pub max_ap: f32,
    pub alive: bool,
    /// Remaining seconds of the stagger defense penalty (2 s per `AcCtrlParamCalc.lua`).
    pub stagger_timer: f32,
    pub pose: u8,
    /// Tick the AC last took damage (interest-management tier 0 for 3 s).
    pub last_hit_tick: Option<Tick>,
    /// Last input actually applied (used for a starved step when no queued frame exists).
    pub last_input: InputFrame,
    /// Monotonic per-AC shot counter used by the deterministic spread seed.
    pub shot_seq: u32,
    pub kills: u16,
    pub deaths: u16,
}

impl SimAc {
    pub fn new(id: NetId, loadout: Loadout, position: Vec3, yaw: f32, team: u8) -> Self {
        let stats = stats::stats_from_loadout(&loadout);
        let design = stats::design_from_loadout(&loadout);
        let armament = Armament::from_design(&design);
        let mut motion = AcMotion::new(stats.ctrl.clone());
        motion.yaw = yaw;
        let ap = stats.ap_max;
        Self {
            id,
            position,
            motion,
            zone: AreaZone::Inside,
            team,
            loadout,
            stats,
            armament,
            ap,
            max_ap: ap,
            alive: true,
            stagger_timer: 0.0,
            pose: pose_state::IDLE,
            last_hit_tick: None,
            last_input: InputFrame::default(),
            shot_seq: 0,
            kills: 0,
            deaths: 0,
        }
    }

    /// Forces the AC to a spawn pose with full EN/AP/ammo and no active movement phases.
    pub fn respawn(&mut self, position: Vec3, yaw: f32) {
        let design = stats::design_from_loadout(&self.loadout);
        self.position = position;
        self.motion = AcMotion::new(self.stats.ctrl.clone());
        self.motion.yaw = yaw;
        self.armament.replenish(&design);
        self.ap = self.max_ap;
        self.alive = true;
        self.stagger_timer = 0.0;
        self.pose = pose_state::IDLE;
        self.last_input = InputFrame::default();
        self.shot_seq = 0;
    }

    /// Per-tick weapon simulation before movement. Starts a blade lunge on the same tick.
    pub fn step_weapons(
        &mut self,
        input: &InputFrame,
        tick: u32,
        match_seed: u64,
        lock_pos: Option<Vec3>,
        next_projectile: &mut u32,
    ) -> weapons::WeaponOutput {
        let blocked =
            self.motion.scan_mode || self.motion.kick.is_some() || self.motion.blade.is_some();
        let combat_mode = !self.motion.scan_mode;
        let aim = input.aim_direction();
        let mut seq = self.shot_seq;
        let out = self.armament.step_tick_mode(
            input,
            tick,
            self.id,
            match_seed,
            self.position,
            aim,
            input.lock_target,
            lock_pos,
            blocked,
            combat_mode,
            &mut self.motion.en,
            next_projectile,
            &mut seq,
        );
        self.shot_seq = seq;
        if !out.blades.is_empty() && self.motion.blade.is_none() {
            if let Some(launch) = out.blades.first() {
                self.motion.start_blade(*launch);
                self.pose = pose_state::BLADE;
            }
        }
        out
    }

    pub fn active_gun(&self, slot: WeaponSlot) -> Option<&weapons::Gun> {
        match slot {
            WeaponSlot::RightHand => Some(self.armament.active_gun(0)),
            WeaponSlot::LeftHand => Some(self.armament.active_gun(1)),
            WeaponSlot::Shoulder => Some(&self.armament.shoulder),
            WeaponSlot::Overed => None,
        }
    }
}

/// One authoritative hit's outcome (used by the server's hitscan/rewind path).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageOutcome {
    pub amount: f32,
    pub recoil: f32,
    pub effective: bool,
    pub lethal: bool,
}

/// The authoritative world. ACs, projectiles, and missiles are kept in id order so snapshot,
/// event, and replay output is deterministic.
#[derive(Clone, Debug)]
pub struct SimWorld {
    pub tick: Tick,
    pub acs: BTreeMap<NetId, SimAc>,
    pub projectiles: BTreeMap<ProjectileId, Projectile>,
    pub missiles: BTreeMap<ProjectileId, Missile>,
    pub destructibles: DestructibleSet,
    pub next_projectile: u32,
    pub match_seed: u64,
    /// Team damage is off by default (server config can enable it).
    pub friendly_fire: bool,
}

impl Default for SimWorld {
    fn default() -> Self {
        Self {
            tick: Tick::ZERO,
            acs: BTreeMap::new(),
            projectiles: BTreeMap::new(),
            missiles: BTreeMap::new(),
            destructibles: DestructibleSet::default(),
            next_projectile: 1,
            match_seed: DEFAULT_MATCH_SEED,
            friendly_fire: false,
        }
    }
}

impl SimWorld {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, ac: SimAc) {
        self.acs.insert(ac.id, ac);
    }

    pub fn remove(&mut self, id: NetId) -> Option<SimAc> {
        self.acs.remove(&id)
    }

    pub fn get(&self, id: NetId) -> Option<&SimAc> {
        self.acs.get(&id)
    }

    pub fn get_mut(&mut self, id: NetId) -> Option<&mut SimAc> {
        self.acs.get_mut(&id)
    }

    /// Advances one tick with no water-return handling.
    pub fn step(
        &mut self,
        inputs: &[(NetId, InputFrame)],
        collision: &mut Collision,
    ) -> Vec<SimEvent> {
        self.step_with_water(inputs, collision, None, None)
    }

    /// Advances one tick: apply one validated input per AC, step weapons, integrate movement and
    /// prop crush, resolve melee/projectile/missile hits and destructible damage, tick every
    /// phase/cooldown, and emit events.
    pub fn step_with_water(
        &mut self,
        inputs: &[(NetId, InputFrame)],
        collision: &mut Collision,
        water_return: Option<&WaterReturnPoints>,
        fallback_start: Option<Start>,
    ) -> Vec<SimEvent> {
        self.tick = self.tick.next();
        let tick = self.tick;
        let mut events = Vec::new();
        let positions: BTreeMap<NetId, Vec3> =
            self.acs.iter().map(|(id, ac)| (*id, ac.position)).collect();
        let mut spawned: Vec<(NetId, Shot)> = Vec::new();
        let mut next_projectile = self.next_projectile;
        let match_seed = self.match_seed;

        {
            let destructibles = &mut self.destructibles;
            for ac in self.acs.values_mut() {
                if !ac.alive {
                    continue;
                }
                let input = inputs
                    .iter()
                    .find(|(id, _)| *id == ac.id)
                    .map(|(_, frame)| *frame)
                    .unwrap_or_else(|| ac.last_input.repeat_safe());
                ac.last_input = input;

                let lock_pos = input
                    .lock_target
                    .is_some()
                    .then(|| positions.get(&input.lock_target).copied())
                    .flatten();
                let out =
                    ac.step_weapons(&input, tick.0, match_seed, lock_pos, &mut next_projectile);
                for shot in out.shots {
                    spawned.push((ac.id, shot));
                }

                let mut broken: Vec<PropId> = Vec::new();
                motion::step_validated_with_props(
                    &mut ac.motion,
                    &input,
                    &mut ac.position,
                    collision,
                    |prop_id, raw| {
                        let id = PropId(prop_id);
                        if destructibles.apply_collision_damage(id, raw) {
                            broken.push(id);
                            true
                        } else {
                            false
                        }
                    },
                );
                for prop in broken {
                    events.push(SimEvent::PropBroken { prop, tick: tick.0 });
                }

                if let (Some(water), Some(fallback)) = (water_return, fallback_start) {
                    if motion::check_water_return(
                        &mut ac.motion,
                        &mut ac.position,
                        water,
                        fallback,
                        collision,
                    ) {
                        events.push(SimEvent::Respawned {
                            target: ac.id,
                            position: ac.position,
                            yaw: ac.motion.yaw,
                            tick: tick.0,
                        });
                    }
                }
                if !ac.position.is_finite() {
                    let (position, yaw) = fallback_start
                        .map(|s| (s.position, s.yaw))
                        .unwrap_or((Vec3::ZERO, 0.0));
                    ac.respawn(position, yaw);
                    events.push(SimEvent::Respawned {
                        target: ac.id,
                        position,
                        yaw,
                        tick: tick.0,
                    });
                }
                ac.stagger_timer = (ac.stagger_timer - TICK_DT).max(0.0);
                ac.pose = remote_pose(&ac.motion);
                // Presentation-only one-frame flags.
                ac.motion.landed = false;
                ac.motion.water_splash_at = None;
                ac.motion.quick_pending = None;
            }
        }
        self.next_projectile = next_projectile;

        // Spawn the shots after the AC loop so the world maps can be borrowed freely.
        for (owner, shot) in spawned {
            let hitscan = shot.gun.is_hitscan();
            events.push(SimEvent::Fired {
                shooter: owner,
                projectile: shot.id,
                slot: shot.slot,
                muzzle: shot.position,
                dir: shot.dir,
                tick: tick.0,
                barrel: shot.barrel,
                hitscan,
            });
            if hitscan {
                // Validated by the server against HitboxHistory; never forward-stepped.
                continue;
            }
            if shot.gun.kind == weapons::Kind::Missile {
                self.missiles
                    .insert(shot.id, Missile::from_shot(&shot, owner));
            } else {
                self.projectiles
                    .insert(shot.id, Projectile::from_shot(&shot, owner, tick.0));
            }
        }

        self.step_melee(tick, &mut events);
        self.step_projectiles(collision, tick, &mut events);
        self.step_missiles(collision, tick, &mut events);
        events
    }

    /// Applies one authoritative hit to an AC and records `Damaged`/`Killed` events.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_hit(
        &mut self,
        target: NetId,
        attacker: NetId,
        attr: DamageType,
        power: f32,
        recoil: f32,
        position: Vec3,
        tick: Tick,
        events: &mut Vec<SimEvent>,
    ) -> Option<DamageOutcome> {
        let ac = self.acs.get_mut(&target)?;
        if !ac.alive {
            return None;
        }
        let res: HitResult = damage::calculate_damage(
            attr,
            power,
            recoil,
            &ac.stats.defense,
            ac.stagger_timer > 0.0,
        );
        ac.ap = (ac.ap - res.damage).max(0.0);
        if ac.stats.stagger_threshold > 0.0 && res.recoil >= ac.stats.stagger_threshold {
            ac.stagger_timer = 2.0;
        }
        ac.last_hit_tick = Some(tick);
        let lethal = ac.ap <= 0.0;
        if lethal {
            ac.alive = false;
            ac.deaths = ac.deaths.saturating_add(1);
            ac.pose = pose_state::IDLE;
        }
        if let Some(killer) = self.acs.get_mut(&attacker) {
            if lethal && attacker != target {
                killer.kills = killer.kills.saturating_add(1);
            }
        }
        events.push(SimEvent::Damaged {
            target,
            attacker,
            amount: res.damage,
            recoil: res.recoil,
            attribute: attr.as_raw(),
            effective: res.effective,
            lethal,
            position,
            tick: tick.0,
        });
        if lethal {
            events.push(SimEvent::Killed {
                target,
                attacker,
                tick: tick.0,
            });
        }
        Some(DamageOutcome {
            amount: res.damage,
            recoil: res.recoil,
            effective: res.effective,
            lethal,
        })
    }

    fn apply_blast(
        &mut self,
        blast: &weapons::ImpactBlast,
        center: Vec3,
        attacker: NetId,
        direct: Option<NetId>,
        tick: Tick,
        events: &mut Vec<SimEvent>,
    ) {
        let attacker_team = self.acs.get(&attacker).map(|ac| ac.team);
        let targets: Vec<NetId> = self
            .acs
            .iter()
            .filter(|(id, ac)| ac.alive && Some(**id) != direct)
            .filter(|(id, ac)| {
                self.friendly_fire
                    || *id == &attacker
                    || attacker_team.is_none_or(|team| ac.team != team)
            })
            .filter(|(_, ac)| ac.position.distance(center) <= blast.radius + 5.5)
            .map(|(id, _)| *id)
            .collect();
        for target in targets {
            let _ = self.apply_hit(
                target,
                attacker,
                blast.damage_type,
                blast.damage_power,
                blast.damage_recoil,
                center,
                tick,
                events,
            );
        }
        for prop in self.destructibles.props_mut() {
            if !prop.broken && prop.center.distance(center) <= blast.radius + prop.radius {
                if prop.apply_damage(blast.damage_power.max(1.0)) {
                    events.push(SimEvent::PropBroken {
                        prop: prop.prop_id,
                        tick: tick.0,
                    });
                }
            }
        }
    }

    fn step_melee(&mut self, tick: Tick, events: &mut Vec<SimEvent>) {
        struct Attack {
            attacker: NetId,
            blade: Option<BladeLaunch>,
            from: Vec3,
            to: Vec3,
            dir: Vec3,
            hit_entities: Vec<u64>,
        }
        let mut attacks = Vec::new();
        for ac in self.acs.values() {
            if !ac.alive {
                continue;
            }
            if let Some(blade) = &ac.motion.blade {
                if blade.phase == BladePhase::Swing {
                    attacks.push(Attack {
                        attacker: ac.id,
                        blade: Some(blade.launch),
                        from: ac.motion.prev_position,
                        to: ac.position,
                        dir: (glam::Quat::from_rotation_y(ac.motion.yaw) * Vec3::NEG_Z)
                            .normalize_or(Vec3::NEG_Z),
                        hit_entities: blade.hit_entities.clone(),
                    });
                }
            }
            if let Some(kick) = ac.motion.kick {
                if !kick.hit_applied {
                    attacks.push(Attack {
                        attacker: ac.id,
                        blade: None,
                        from: ac.motion.prev_position,
                        to: ac.position,
                        dir: kick.dir,
                        hit_entities: Vec::new(),
                    });
                }
            }
        }
        for attack in attacks {
            let attacker = match self.acs.get(&attack.attacker) {
                Some(ac) if ac.alive => ac,
                _ => continue,
            };

            if let Some(blade) = attack.blade {
                let (base_power, base_recoil) = weapons::blade_scaled(
                    attacker,
                    blade.damage_power,
                    blade.damage_recoil,
                    blade.physical,
                );
                let max_hits = blade.max_hits.max(1) as usize;
                let remaining_slots = max_hits.saturating_sub(attack.hit_entities.len());
                let origin = attack.to + Vec3::Y * 4.5 + attack.dir * 2.0;
                let tip = origin + attack.dir * blade.hit_radius.max(0.0);
                let sweep = BladeSweep::new(
                    origin,
                    tip,
                    blade.blade_radius,
                    blade.hit_radius,
                    blade.near_hit_only,
                );

                let mut any_hit = !attack.hit_entities.is_empty();
                if remaining_slots > 0 {
                    let mut candidates: Vec<(f32, NetId)> = Vec::new();
                    for (id, target) in &self.acs {
                        if *id == attack.attacker
                            || !target.alive
                            || (!self.friendly_fire && target.team == attacker.team)
                            || attack.hit_entities.contains(&u64::from(id.0))
                        {
                            continue;
                        }
                        let mut best_t = f32::INFINITY;
                        for sphere in damage::ac_hit_spheres() {
                            let center = target.position + Vec3::Y * sphere.offset_y;
                            if let Some(t) = sweep.hits_sphere(center, sphere.radius) {
                                best_t = best_t.min(t);
                            }
                        }
                        if best_t.is_finite() {
                            candidates.push((best_t, *id));
                        }
                    }
                    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
                    candidates.truncate(remaining_slots);

                    let mut first = !any_hit;
                    for (_, target) in &candidates {
                        let (power, recoil) = if first {
                            first = false;
                            any_hit = true;
                            (base_power, base_recoil)
                        } else {
                            (
                                base_power * blade.decay_power.clamp(0.0, 1.0),
                                base_recoil * blade.decay_recoil.clamp(0.0, 1.0),
                            )
                        };
                        let pos = self
                            .acs
                            .get(target)
                            .map(|ac| ac.position + Vec3::Y * 4.0)
                            .unwrap_or(origin);
                        let _ = self.apply_hit(
                            *target,
                            attack.attacker,
                            blade.damage_type,
                            power,
                            recoil,
                            pos,
                            tick,
                            events,
                        );
                        if let Some(ac) = self.acs.get_mut(&attack.attacker) {
                            if let Some(live) = ac.motion.blade.as_mut() {
                                live.hit_entities.push(u64::from(target.0));
                            }
                        }
                    }
                }

                // Destructible props swept by the same capsule. Props do not consume the
                // distinct-character hit count; they use the current decayed power.
                let (prop_power, _) = if !any_hit {
                    (base_power, base_recoil)
                } else {
                    (
                        base_power * blade.decay_power.clamp(0.0, 1.0),
                        base_recoil * blade.decay_recoil.clamp(0.0, 1.0),
                    )
                };
                for prop in self.destructibles.props_mut() {
                    if !prop.broken
                        && sweep.hits_sphere(prop.center, prop.radius).is_some()
                        && prop.apply_damage(prop_power.max(1.0))
                    {
                        events.push(SimEvent::PropBroken {
                            prop: prop.prop_id,
                            tick: tick.0,
                        });
                    }
                }
                continue;
            }

            let (power, recoil) = weapons::kick_scaled(attacker);
            let origin = attack.to + Vec3::Y * 4.0 + attack.dir * 2.5;
            let from = attack.from + Vec3::Y * 4.0 + attack.dir * 2.5;
            let mut best: Option<(f32, NetId, Vec3)> = None;
            for (id, target) in &self.acs {
                if *id == attack.attacker
                    || !target.alive
                    || (!self.friendly_fire && target.team == attacker.team)
                {
                    continue;
                }
                for sphere in damage::ac_hit_spheres() {
                    let center = target.position + Vec3::Y * sphere.offset_y;
                    if let Some((t, p, _)) = damage::swept_sphere_hits_sphere(
                        from,
                        origin,
                        7.0,
                        center,
                        sphere.radius,
                    ) {
                        if best.is_none_or(|(bt, _, _)| t < bt) {
                            best = Some((t, *id, p));
                        }
                    }
                }
            }
            if let Some((_, target, pos)) = best {
                let _ = self.apply_hit(
                    target,
                    attack.attacker,
                    DamageType::Ke,
                    power,
                    recoil,
                    pos,
                    tick,
                    events,
                );
                for prop in self.destructibles.props_mut() {
                    if !prop.broken
                        && damage::swept_sphere_hits_sphere(
                            from,
                            origin,
                            7.0,
                            prop.center,
                            prop.radius,
                        )
                        .is_some()
                        && prop.apply_damage(power.max(1.0))
                    {
                        events.push(SimEvent::PropBroken {
                            prop: prop.prop_id,
                            tick: tick.0,
                        });
                    }
                }
                if let Some(ac) = self.acs.get_mut(&attack.attacker) {
                    if let Some(kick) = ac.motion.kick.as_mut() {
                        kick.hit_applied = true;
                    }
                }
            }
        }
    }

    fn step_projectiles(&mut self, collision: &Collision, tick: Tick, events: &mut Vec<SimEvent>) {
        let ids: Vec<ProjectileId> = self.projectiles.keys().copied().collect();
        for id in ids {
            let Some(mut shot) = self.projectiles.remove(&id) else {
                continue;
            };
            let prev = shot.position;
            let Some(delta) = shot.step_tick() else {
                events.push(SimEvent::ProjectileExpire {
                    projectile: id,
                    position: prev,
                });
                continue;
            };
            let next = prev + delta;
            let terrain = collision.raycast_with_prop(prev, next, RayFilter::Bullet);
            let mut best_target: Option<(f32, NetId, Vec3)> = None;
            for (target_id, target) in &self.acs {
                if *target_id == shot.owner
                    || !target.alive
                    || (!self.friendly_fire
                        && self
                            .acs
                            .get(&shot.owner)
                            .is_some_and(|owner| owner.team == target.team))
                {
                    continue;
                }
                for sphere in damage::ac_hit_spheres() {
                    let center = target.position + Vec3::Y * sphere.offset_y;
                    if let Some((t, pos, _)) =
                        damage::segment_hits_sphere(prev, next, center, sphere.radius)
                    {
                        if best_target.is_none_or(|(bt, _, _)| t < bt) {
                            best_target = Some((t, *target_id, pos));
                        }
                    }
                }
            }
            if let Some((t_hit, target, pos)) = best_target {
                if terrain.is_none_or(|(t_g, _, _, _)| t_hit <= t_g) {
                    let (power, recoil) = shot.effective_damage();
                    events.push(SimEvent::ProjectileHit {
                        projectile: id,
                        target,
                        position: pos,
                    });
                    let _ = self.apply_hit(
                        target,
                        shot.owner,
                        shot.damage_type,
                        power,
                        recoil,
                        pos,
                        tick,
                        events,
                    );
                    if let Some(blast) = shot.blast.clone() {
                        self.apply_blast(&blast, pos, shot.owner, Some(target), tick, events);
                    }
                    continue;
                }
            }
            if let Some((_, at, _, prop_id)) = terrain {
                if let Some(prop_id) = prop_id {
                    let (power, _) = shot.effective_damage();
                    if self
                        .destructibles
                        .apply_damage(PropId(prop_id), power.max(1.0))
                    {
                        events.push(SimEvent::PropBroken {
                            prop: PropId(prop_id),
                            tick: tick.0,
                        });
                    }
                }
                events.push(SimEvent::ProjectileExpire {
                    projectile: id,
                    position: at,
                });
                if let Some(blast) = shot.blast.clone() {
                    self.apply_blast(&blast, at, shot.owner, None, tick, events);
                }
                continue;
            }
            shot.position = next;
            self.projectiles.insert(id, shot);
        }
    }

    fn step_missiles(&mut self, collision: &Collision, tick: Tick, events: &mut Vec<SimEvent>) {
        let ids: Vec<ProjectileId> = self.missiles.keys().copied().collect();
        for id in ids {
            let Some(mut missile) = self.missiles.remove(&id) else {
                continue;
            };
            let target_pos = missile
                .target
                .and_then(|t| self.acs.get(&t))
                .filter(|ac| ac.alive)
                .map(|ac| ac.position + Vec3::Y * 4.5);
            let prev = missile.position;
            let Some((delta, _ignited)) = missile.step_tick(target_pos) else {
                events.push(SimEvent::ProjectileExpire {
                    projectile: id,
                    position: prev,
                });
                continue;
            };
            let next = prev + delta;
            let extra = missile
                .flight
                .proximity_fuse_range
                .max(missile.flight.hit_radius.min(2.0));
            let terrain = collision.raycast_with_prop(prev, next, RayFilter::Bullet);
            let mut best_target: Option<(f32, NetId, Vec3)> = None;
            for (target_id, target) in &self.acs {
                if *target_id == missile.owner
                    || !target.alive
                    || (!self.friendly_fire
                        && self
                            .acs
                            .get(&missile.owner)
                            .is_some_and(|owner| owner.team == target.team))
                {
                    continue;
                }
                for sphere in damage::ac_hit_spheres() {
                    let center = target.position + Vec3::Y * sphere.offset_y;
                    if let Some((t, pos, _)) =
                        damage::segment_hits_sphere(prev, next, center, sphere.radius + extra)
                    {
                        if best_target.is_none_or(|(bt, _, _)| t < bt) {
                            best_target = Some((t, *target_id, pos));
                        }
                    }
                }
            }
            if let Some((t_hit, target, pos)) = best_target {
                if terrain.is_none_or(|(t_g, _, _, _)| t_hit <= t_g) {
                    let (power, recoil) = missile.effective_damage();
                    events.push(SimEvent::ProjectileHit {
                        projectile: id,
                        target,
                        position: pos,
                    });
                    let _ = self.apply_hit(
                        target,
                        missile.owner,
                        missile.damage_type,
                        power,
                        recoil,
                        pos,
                        tick,
                        events,
                    );
                    if let Some(blast) = missile.blast.clone() {
                        self.apply_blast(&blast, pos, missile.owner, Some(target), tick, events);
                    }
                    continue;
                }
            }
            if let Some((_, at, _, prop_id)) = terrain {
                if let Some(prop_id) = prop_id {
                    let (power, _) = missile.effective_damage();
                    if self
                        .destructibles
                        .apply_damage(PropId(prop_id), power.max(1.0))
                    {
                        events.push(SimEvent::PropBroken {
                            prop: PropId(prop_id),
                            tick: tick.0,
                        });
                    }
                }
                events.push(SimEvent::ProjectileExpire {
                    projectile: id,
                    position: at,
                });
                if let Some(blast) = missile.blast.clone() {
                    self.apply_blast(&blast, at, missile.owner, None, tick, events);
                }
                continue;
            }
            missile.position = next;
            self.missiles.insert(id, missile);
        }
    }

    /// Full snapshot record for one AC.
    pub fn ac_record(&self, id: NetId, quantizer: &acvd_proto::PosQuantizer) -> Option<AcRecord> {
        let ac = self.acs.get(&id)?;
        Some(record_of(ac, quantizer))
    }

    /// Every AC record in `NetId` order.
    pub fn records(&self, quantizer: &acvd_proto::PosQuantizer) -> Vec<AcRecord> {
        self.acs
            .values()
            .map(|ac| record_of(ac, quantizer))
            .collect()
    }

    /// Exact authoritative local-player state for reconciliation.
    pub fn local_state(&self, id: NetId, last_acked_seq: u16) -> Option<LocalPlayerState> {
        let ac = self.acs.get(&id)?;
        Some(local_state_of(ac, last_acked_seq, self.tick.0))
    }

    /// Damaged/broken destructibles and the full broken bitset for a late joiner.
    pub fn world_state(&self) -> WorldState {
        let props = self
            .destructibles
            .props()
            .iter()
            .filter(|p| p.broken || (p.hp - p.max_hp).abs() > 1e-3)
            .map(|p| PropState {
                prop_id: p.prop_id.0,
                hp: p.hp,
                broken: p.broken,
            })
            .collect();
        WorldState {
            server_tick: self.tick.0,
            props,
            broken_bits: self.destructibles.broken_bits(),
        }
    }

    /// Nearby homing missile corrections for one snapshot packet (capped by the caller).
    pub fn missile_corrections(
        &self,
        center: Vec3,
        radius: f32,
        max: usize,
    ) -> Vec<MissileCorrection> {
        self.missiles
            .values()
            .filter(|m| m.position.distance(center) <= radius)
            .take(max)
            .map(|m| MissileCorrection {
                projectile_id: m.id.0,
                pos: [
                    quantize_coord(m.position.x),
                    quantize_coord(m.position.y),
                    quantize_coord(m.position.z),
                ],
                vel: [
                    quantize_coord(m.velocity.x),
                    quantize_coord(m.velocity.y),
                    quantize_coord(m.velocity.z),
                ],
                target: m
                    .target
                    .and_then(|id| acvd_proto::NetId::new(id.0).ok())
                    .unwrap_or(acvd_proto::NetId::NONE),
            })
            .collect()
    }
}

/// Simulation id -> wire id (both reserve `0` for none and `255` is a valid AC id on the wire).
pub fn wire_id(id: NetId) -> acvd_proto::NetId {
    acvd_proto::NetId::new(id.0).unwrap_or(acvd_proto::NetId::NONE)
}

/// One AC's compact remote record.
pub fn record_of(ac: &SimAc, quantizer: &acvd_proto::PosQuantizer) -> AcRecord {
    let right = ac.armament.active_gun(0);
    let left = ac.armament.active_gun(1);
    let mut combat = ac.armament.combat_flags();
    if ac.motion.glide_qb {
        combat |= acvd_proto::combat_flags::GLIDE_QB;
    }
    AcRecord {
        net_id: wire_id(ac.id),
        pos: quantizer.quantize(ac.position),
        vel: [
            quantize_vel(ac.motion.velocity.x),
            quantize_vel(ac.motion.velocity.y),
            quantize_vel(ac.motion.velocity.z),
        ],
        yaw: quantize_yaw(ac.motion.yaw),
        pitch: quantize_pitch(ac.motion.aim_pitch),
        flags: flags_of(ac),
        pose_state: ac.pose,
        combat_flags: combat,
        weapon_arm_ticks: ac.armament.weapon_arm_ticks(),
        ap: ((ac.ap / ac.max_ap.max(1.0)).clamp(0.0, 1.0) * 65535.0).round() as u16,
        en: ((ac.motion.en / ac.motion.en_max.max(1.0)).clamp(0.0, 1.0) * 255.0).round() as u8,
        reload: [right.reload_ticks(), left.reload_ticks()],
        ammo: [
            right.remaining.min(255) as u8,
            left.remaining.min(255) as u8,
        ],
    }
}

/// Flags shared by `AcRecord` and `LocalPlayerState`.
pub fn flags_of(ac: &SimAc) -> u16 {
    let p = &ac.motion;
    let mut flags = 0;
    if p.boost {
        flags |= ac_flags::BOOST;
    }
    if p.glide {
        flags |= ac_flags::GLIDE;
    }
    if p.airborne {
        flags |= ac_flags::AIRBORNE;
    }
    if p.scan_mode {
        flags |= ac_flags::SCAN;
    }
    if p.qb.is_some() {
        flags |= ac_flags::QB;
    }
    if p.wj.is_some() {
        flags |= ac_flags::WALL_JUMP;
    }
    if p.kick.is_some() {
        flags |= ac_flags::KICK;
    }
    if p.blade.is_some() {
        flags |= ac_flags::BLADE;
    }
    if p.ow_active {
        flags |= ac_flags::OVERED;
    }
    if !ac.alive {
        flags |= ac_flags::DESTROYED;
    }
    if ac.team == 1 {
        flags |= ac_flags::TEAM_B;
    }
    flags
}

fn qb_phase_code(p: &AcMotion) -> u8 {
    match p.qb {
        None => 0,
        Some(QbPhase::Charge { .. }) => 1,
        Some(QbPhase::Phase1 { .. }) => 2,
        Some(QbPhase::Phase2 { .. }) => 3,
    }
}

fn wj_phase_code(p: &AcMotion) -> u8 {
    match p.wj {
        None => 0,
        Some(WallJumpPhase::Prep { clip_dir, .. }) => 1 | (clip_dir << 4),
        Some(WallJumpPhase::Accel { clip_dir, .. }) => 2 | (clip_dir << 4),
        Some(WallJumpPhase::After { clip_dir, .. }) => 3 | (clip_dir << 4),
    }
}

fn blade_phase_code(p: &AcMotion) -> u8 {
    match p.blade.as_ref().map(|b| b.phase) {
        None => 0,
        Some(BladePhase::Charge) => 1,
        Some(BladePhase::Dash) => 2,
        Some(BladePhase::Swing) => 3,
        Some(BladePhase::Recovery) => 4,
    }
}

/// Exact local-player state for reconciliation.
pub fn local_state_of(ac: &SimAc, last_acked_seq: u16, server_tick: u32) -> LocalPlayerState {
    let p = &ac.motion;
    let qb_dir = match &p.qb {
        Some(QbPhase::Phase1 { dir, .. } | QbPhase::Phase2 { dir, .. }) => quantize_dir3(*dir),
        _ => [0; 3],
    };
    let (wj_dir, wj_normal) = match p.wj {
        Some(WallJumpPhase::Prep { wall_normal, .. }) => ([0; 3], quantize_dir3(wall_normal)),
        Some(WallJumpPhase::Accel { dir, .. }) => (quantize_dir3(dir), [0; 3]),
        _ => ([0; 3], [0; 3]),
    };
    let kick_dir = p.kick.map(|k| quantize_dir3(k.dir)).unwrap_or([0; 3]);
    let blade_target = p
        .blade
        .as_ref()
        .and_then(|b| b.target_pos)
        .map(|t| {
            [
                quantize_coord(t.x),
                quantize_coord(t.y),
                quantize_coord(t.z),
            ]
        })
        .unwrap_or([0; 3]);
    let (qb_timer, qb_weight, qb_p1, qb_p2, qb_held) = match &p.qb {
        Some(QbPhase::Charge { held_ticks }) => (0.0, 0.0, 0.0, 0.0, *held_ticks),
        Some(QbPhase::Phase1 {
            timer,
            p1_ticks,
            p2_ticks,
            weight,
            ..
        }) => (*timer, *weight, *p1_ticks, *p2_ticks, 0.0),
        Some(QbPhase::Phase2 {
            timer,
            p2_ticks,
            weight,
            ..
        }) => (*timer, *weight, 0.0, *p2_ticks, 0.0),
        None => (0.0, 0.0, 0.0, 0.0, 0.0),
    };
    let (wj_timer, kick_timer) = match p.wj {
        Some(WallJumpPhase::Prep { timer, .. }) => (timer, 0.0),
        Some(WallJumpPhase::Accel { timer, .. }) => (timer, 0.0),
        Some(WallJumpPhase::After { timer, .. }) => (timer, 0.0),
        None => (0.0, p.kick.map_or(0.0, |k| k.timer)),
    };
    let blade = p.blade.clone();
    let mut state = LocalPlayerState {
        pos: ac.position.to_array(),
        vel: p.velocity.to_array(),
        yaw: p.yaw,
        pitch: p.aim_pitch,
        aim_yaw: p.yaw + p.aim_yaw_off,
        flags: flags_of(ac),
        en: p.en,
        en_max: p.en_max,
        ap: ac.ap,
        last_acked_seq,
        server_tick,
        qb_timer,
        wj_timer,
        kick_timer,
        blade_timer: blade.as_ref().map_or(0.0, |b| b.timer),
        stagger_timer: ac.stagger_timer,
        en_regen_delay: 0.0,
        glide_time: p.glide_phase_time,
        glide_lift_left: p.glide_lift_left,
        glide_activation_y: p.glide_activation_y,
        takeoff: p.takeoff,
        qb_cooldown: p.qb_cooldown,
        wj_lockout: p.wj_lockout,
        ow_dash_ticks: p.ow_dash_ticks,
        qb_weight,
        qb_p1_ticks: qb_p1,
        qb_p2_ticks: qb_p2,
        qb_charge_held: qb_held,
        qb_dir,
        wj_dir,
        wj_normal,
        kick_dir,
        blade_target,
        blade_max_dash_ticks: blade.as_ref().map_or(0.0, |b| b.max_dash_ticks),
        blade_entry_speed: blade.as_ref().map_or(0.0, |b| b.entry_speed_tick),
        qb_phase: qb_phase_code(p),
        wj_phase: wj_phase_code(p),
        blade_phase: blade_phase_code(p),
        kick_hit: u8::from(p.kick.is_some_and(|k| k.hit_applied)),
        glide_phase: match p.glide_phase {
            GlidePhase::Ground => 0,
            GlidePhase::Air => 1,
        },
        glide_flags: u8::from(p.glide_lifting)
            | (u8::from(p.glide_lift_started) << 1)
            | (u8::from(p.glide_qb) << 2),
        blade_flags: blade
            .as_ref()
            .map_or(0, |b| u8::from(b.air_start) | (u8::from(b.downward) << 1)),
        pose_state: ac.pose,
        ..LocalPlayerState::default()
    };
    ac.armament.write_local_state(&mut state);
    state
}

fn dequant_dir(q: [i8; 3]) -> Vec3 {
    acvd_proto::dequantize_dir3(q)
}

fn dequant_coord(q: [i16; 3]) -> Vec3 {
    Vec3::new(
        acvd_proto::dequantize_coord(q[0]),
        acvd_proto::dequantize_coord(q[1]),
        acvd_proto::dequantize_coord(q[2]),
    )
}

/// Restores an AC's simulation state from the authoritative local-player record.
///
/// Every phase, cooldown, phase parameter, and weapon rack/ammo timer is copied so the client can
/// replay unacked inputs from the exact state the server reached instead of approximating timers.
pub fn apply_local_state(ac: &mut SimAc, state: &LocalPlayerState) {
    let p = &mut ac.motion;
    ac.position = Vec3::from_array(state.pos);
    p.velocity = Vec3::from_array(state.vel);
    p.yaw = state.yaw;
    p.aim_pitch = state.pitch;
    p.aim_yaw_off = wrap_angle(state.aim_yaw - state.yaw);
    p.en = state.en;
    p.en_max = state.en_max.max(p.ctrl.en_capacity.max(0.0));
    ac.ap = state.ap;
    ac.alive = state.flags & ac_flags::DESTROYED == 0;
    ac.stagger_timer = state.stagger_timer;

    p.boost = state.flags & ac_flags::BOOST != 0;
    p.glide = state.flags & ac_flags::GLIDE != 0;
    p.airborne = state.flags & ac_flags::AIRBORNE != 0;
    p.scan_mode = state.flags & ac_flags::SCAN != 0;
    p.ow_active = state.flags & ac_flags::OVERED != 0;
    p.glide_phase = if state.glide_phase == 1 {
        GlidePhase::Air
    } else {
        GlidePhase::Ground
    };
    p.glide_phase_time = state.glide_time;
    p.glide_lift_left = state.glide_lift_left;
    p.glide_activation_y = state.glide_activation_y;
    p.glide_lifting = state.glide_flags & 1 != 0;
    p.glide_lift_started = state.glide_flags & 2 != 0;
    p.glide_qb = state.glide_flags & 4 != 0;
    p.takeoff = state.takeoff;
    p.qb_cooldown = state.qb_cooldown;
    p.wj_lockout = state.wj_lockout;
    p.ow_dash_ticks = state.ow_dash_ticks;
    p.prev_position = ac.position;
    p.jump_queued = false;
    p.wall_jump_queued = false;
    p.quick_pending = None;
    p.landed = false;
    p.kick = None;
    p.blade = None;
    p.qb = None;
    p.wj = None;

    let qb_dir = dequant_dir(state.qb_dir);
    p.qb = match state.qb_phase {
        1 => Some(QbPhase::Charge {
            held_ticks: state.qb_charge_held,
        }),
        2 => Some(QbPhase::Phase1 {
            timer: state.qb_timer,
            p1_ticks: state.qb_p1_ticks,
            p2_ticks: state.qb_p2_ticks,
            weight: state.qb_weight,
            dir: qb_dir,
            eighth: 0,
        }),
        3 => Some(QbPhase::Phase2 {
            timer: state.qb_timer,
            p2_ticks: state.qb_p2_ticks,
            weight: state.qb_weight,
            dir: qb_dir,
        }),
        _ => None,
    };
    let clip_dir = state.wj_phase >> 4;
    let wall_normal = dequant_dir(state.wj_normal);
    p.wj = match state.wj_phase & 0x0f {
        1 => Some(WallJumpPhase::Prep {
            timer: state.wj_timer,
            wall_normal,
            clip_dir,
        }),
        2 => Some(WallJumpPhase::Accel {
            timer: state.wj_timer,
            dir: dequant_dir(state.wj_dir),
            clip_dir,
        }),
        3 => Some(WallJumpPhase::After {
            timer: state.wj_timer,
            clip_dir,
        }),
        _ => None,
    };
    if p.wj.is_some() {
        p.kick = None;
    } else if state.kick_timer > 0.0 {
        p.kick = Some(KickState {
            timer: state.kick_timer,
            dir: dequant_dir(state.kick_dir),
            hit_applied: state.kick_hit != 0,
        });
    }
    if state.blade_phase != 0 {
        let target = dequant_coord(state.blade_target);
        let has_target = state.blade_target != [0; 3];
        p.blade = Some(BladeAction {
            launch: BladeLaunch {
                hand_idx: 0,
                physical: false,
                target: has_target.then_some((0, target)),
                target_valid: has_target,
                hit_radius: 0.0,
                blade_radius: 0.0,
                max_hits: 0,
                near_hit_only: false,
                decay_power: 0.0,
                decay_recoil: 0.0,
                damage_type: crate::damage::DamageType::Ke,
                damage_power: 0.0,
                damage_recoil: 0.0,
                trail_fx: 0,
                hit_low_fx: 0,
                hit_hi_fx: 0,
                hit_low_se: 0,
                hit_hi_se: 0,
            },
            phase: match state.blade_phase {
                2 => BladePhase::Dash,
                3 => BladePhase::Swing,
                4 => BladePhase::Recovery,
                _ => BladePhase::Charge,
            },
            timer: state.blade_timer,
            max_dash_ticks: state.blade_max_dash_ticks,
            entry_speed_tick: state.blade_entry_speed,
            pitch: 0.0,
            target_pos: has_target.then_some(target),
            air_start: state.blade_flags & 1 != 0,
            downward: state.blade_flags & 2 != 0,
            trail_spawned: false,
            hit_entities: Vec::new(),
        });
    }
    ac.armament.read_local_state(state);
    ac.pose = state.pose_state;
}

/// Remote animation pose for one motion state.
pub fn remote_pose(p: &AcMotion) -> u8 {
    if p.blade.is_some() {
        return pose_state::BLADE;
    }
    if p.kick.is_some() {
        return pose_state::KICK;
    }
    if p.wj.is_some() {
        return pose_state::WALL_JUMP;
    }
    if p.qb.is_some() {
        return pose_state::QB;
    }
    if p.glide {
        return if p.airborne || p.glide_phase == GlidePhase::Air {
            pose_state::GLIDE_AIR
        } else {
            pose_state::GLIDE
        };
    }
    if p.landed {
        return pose_state::LAND;
    }
    let horizontal = (p.velocity.x * p.velocity.x + p.velocity.z * p.velocity.z).sqrt();
    if p.airborne {
        if horizontal > 0.15 {
            pose_state::AIR_MOVE
        } else if p.velocity.y > 0.0 {
            pose_state::RISE
        } else {
            pose_state::FALL
        }
    } else if horizontal > p.ctrl.walk_max_tick.max(0.01) * 1.15 {
        pose_state::DASH
    } else if horizontal > 0.03 {
        pose_state::WALK
    } else {
        pose_state::IDLE
    }
}

/// Selects the weapon slot a `Fired` event belongs to.
pub fn weapon_slot_of(slot: u8) -> WeaponSlot {
    match slot {
        0 => WeaponSlot::RightHand,
        1 => WeaponSlot::LeftHand,
        2 => WeaponSlot::Shoulder,
        _ => WeaponSlot::Overed,
    }
}

/// Applies a late-join world state to a local destructible set, including the full broken bitset.
pub fn apply_world_state(destructibles: &mut DestructibleSet, world: &WorldState) {
    for (index, prop) in destructibles.props_mut().iter_mut().enumerate() {
        if world.is_broken(index) {
            prop.hp = 0.0;
            prop.broken = true;
            prop.just_broken = false;
        }
    }
    for prop in &world.props {
        if let Some(p) = destructibles.get_mut(PropId(prop.prop_id)) {
            p.hp = prop.hp;
            p.broken = prop.broken;
            p.just_broken = false;
        }
    }
}

/// Applies an authoritative DestructibleBreak delta.
pub fn apply_break(destructibles: &mut DestructibleSet, prop_id: u32) {
    destructibles.break_prop(PropId(prop_id));
}

/// Helper used by the server: the set of AC ids currently alive on a different team.
pub fn enemy_ids(world: &SimWorld, shooter: NetId) -> BTreeSet<NetId> {
    let team = world.acs.get(&shooter).map_or(0, |ac| ac.team);
    world
        .acs
        .iter()
        .filter(|(id, ac)| **id != shooter && ac.alive && (world.friendly_fire || ac.team != team))
        .map(|(id, _)| *id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_proto::PosQuantizer;

    fn loadout() -> Loadout {
        crate::stats::default_loadout()
    }

    #[test]
    fn slow_projectile_is_forward_simulated_and_damages_the_target() {
        let mut world = SimWorld::new();
        world.insert(SimAc::new(NetId(1), loadout(), Vec3::ZERO, 0.0, 0));
        world.insert(SimAc::new(
            NetId(2),
            loadout(),
            Vec3::new(0.0, 0.0, -30.0),
            0.0,
            1,
        ));
        let mut gun = world.acs[&NetId(1)].armament.hands[0];
        gun.init_speed = 30.0;
        gun.kind = weapons::Kind::Rigid;
        gun.flight = weapons::Flight::default();
        gun.damage_type = DamageType::Ke;
        gun.damage_power = 1000.0;
        gun.damage_recoil = 10.0;
        assert!(!gun.is_hitscan(), "the synthetic round is slow");
        let shot = Shot {
            id: ProjectileId(1),
            slot: WeaponSlot::RightHand,
            position: Vec3::new(0.0, 4.5, -1.0),
            dir: Vec3::NEG_Z,
            target: None,
            gun,
            seed: 0,
            barrel: 0,
        };
        world
            .projectiles
            .insert(shot.id, Projectile::from_shot(&shot, NetId(1), 0));
        let collision = Collision::default();
        let mut hit = false;
        for tick in 0..60 {
            let mut events = Vec::new();
            world.step_projectiles(&collision, Tick(tick), &mut events);
            for event in events {
                if matches!(
                    event,
                    SimEvent::ProjectileHit { target, .. } if target == NetId(2)
                ) {
                    hit = true;
                }
            }
            if hit {
                break;
            }
        }
        assert!(hit, "a slow round is stepped forward into the target");
        let target = world.acs.get(&NetId(2)).unwrap();
        assert!(target.ap < target.max_ap);
    }

    #[test]
    fn bay_swap_flags_survive_record_and_local_state() {
        let mut world = SimWorld::new();
        let mut ac = SimAc::new(NetId(1), loadout(), Vec3::ZERO, 0.0, 0);
        ac.motion.yaw = 0.0;
        world.insert(ac);
        let mut collision = Collision::default();
        let mut swap = InputFrame::default();
        swap.bay_l = true;
        let _ = world.step(&[(NetId(1), swap)], &mut collision);
        assert!(world.acs[&NetId(1)].armament.swapping(1));
        for _ in 0..weapons::BAY_SWAP_TICKS {
            let _ = world.step(&[(NetId(1), InputFrame::default())], &mut collision);
        }
        let ac = &world.acs[&NetId(1)];
        assert!(ac.armament.active_bay[1]);
        let record = record_of(ac, &PosQuantizer::default());
        assert_ne!(
            record.combat_flags & acvd_proto::combat_flags::LEFT_BAY_ACTIVE,
            0
        );
        let state = local_state_of(ac, 0, world.tick.0);
        assert_ne!(
            state.combat_flags & acvd_proto::combat_flags::LEFT_BAY_ACTIVE,
            0
        );
        assert_eq!(state.swap_ticks[1], 0);
        assert_eq!(state.server_tick, world.tick.0);
    }

    #[test]
    fn late_join_bitset_matches_the_authoritative_broken_set() {
        let prop = crate::map::Destructible {
            prop_id: PropId(1),
            model: "test".into(),
            center: Vec3::ZERO,
            radius: 1.0,
            hp: 10.0,
            max_hp: 10.0,
            cutoff_damage: 0.0,
            collision_damage_mult: 0.0,
            damage_id: 0,
            broken: false,
            just_broken: false,
        };
        let mut world = SimWorld::new();
        world.destructibles = DestructibleSet::new(vec![prop]);
        assert!(world.destructibles.apply_damage(PropId(1), 100.0));
        let state = world.world_state();
        assert!(state.is_broken(0));
        let mut clone = DestructibleSet::new(world.destructibles.props().to_vec());
        clone.break_prop(PropId(1));
        assert_eq!(clone.broken_bits(), world.destructibles.broken_bits());
        let mut late = DestructibleSet::new(vec![crate::map::Destructible {
            prop_id: PropId(1),
            model: "test".into(),
            center: Vec3::ZERO,
            radius: 1.0,
            hp: 10.0,
            max_hp: 10.0,
            cutoff_damage: 0.0,
            collision_damage_mult: 0.0,
            damage_id: 0,
            broken: false,
            just_broken: false,
        }]);
        apply_world_state(&mut late, &state);
        assert_eq!(late.broken_bits(), world.destructibles.broken_bits());
    }
    fn test_blade_action(
        hit_radius: f32,
        blade_radius: f32,
        max_hits: u8,
        decay_power: f32,
        decay_recoil: f32,
        near_hit_only: bool,
    ) -> BladeAction {
        BladeAction {
            launch: BladeLaunch {
                hand_idx: 0,
                physical: true,
                target: None,
                target_valid: true,
                hit_radius,
                blade_radius,
                max_hits,
                near_hit_only,
                decay_power,
                decay_recoil,
                damage_type: DamageType::Ke,
                damage_power: 1000.0,
                damage_recoil: 100.0,
                trail_fx: 0,
                hit_low_fx: 0,
                hit_hi_fx: 0,
                hit_low_se: 0,
                hit_hi_se: 0,
            },
            phase: BladePhase::Swing,
            timer: 0.0,
            max_dash_ticks: 0.0,
            entry_speed_tick: 0.0,
            pitch: 0.0,
            target_pos: None,
            air_start: false,
            downward: false,
            trail_spawned: false,
            hit_entities: Vec::new(),
        }
    }

    fn blade_world(max_hits: u8, decay_power: f32, decay_recoil: f32) -> SimWorld {
        let mut world = SimWorld::new();
        let mut attacker = SimAc::new(NetId(1), loadout(), Vec3::ZERO, 0.0, 0);
        attacker.motion.prev_position = Vec3::ZERO;
        attacker.motion.blade = Some(test_blade_action(20.0, 3.0, max_hits, decay_power, decay_recoil, false));
        world.insert(attacker);
        for (i, z) in [-6.0, -8.0, -10.0, -12.0].iter().enumerate() {
            world.insert(SimAc::new(
                NetId(2 + i as u8),
                loadout(),
                Vec3::new(0.0, 0.0, *z),
                0.0,
                1,
            ));
        }
        world
    }

    #[test]
    fn blade_radius_is_capsule_hit_radius_is_broad_phase() {
        let sweep = BladeSweep::new(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -10.0),
            1.0,
            10.0,
            false,
        );
        assert!(sweep.hits_sphere(Vec3::new(0.0, 0.0, -5.0), 0.0).is_some());
        assert!(
            sweep.hits_sphere(Vec3::new(0.0, 3.0, -5.0), 0.5).is_none(),
            "inside HitRadius broad phase but outside BladeRadius capsule"
        );
        assert!(sweep.hits_sphere(Vec3::new(0.0, 1.4, -5.0), 0.5).is_some());

        let near = BladeSweep::new(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -10.0),
            5.0,
            10.0,
            true,
        );
        assert!(near.hits_sphere(Vec3::new(0.0, 3.0, -5.0), 0.5).is_none());
        assert!(near.hits_sphere(Vec3::new(0.0, 2.0, -5.0), 0.5).is_some());
    }

    #[test]
    fn swing_hits_each_target_once() {
        let mut world = blade_world(3, 0.5, 0.5);
        let mut events = Vec::new();
        world.step_melee(Tick(0), &mut events);
        let damaged: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SimEvent::Damaged { target, .. } => Some(*target),
                _ => None,
            })
            .collect();
        assert!(damaged.contains(&NetId(2)));
        let ap_after_first = world.acs[&NetId(2)].ap;
        let hits_after_first = world.acs[&NetId(1)]
            .motion
            .blade
            .as_ref()
            .unwrap()
            .hit_entities
            .len();
        assert!(hits_after_first >= 1);

        events.clear();
        world.step_melee(Tick(1), &mut events);
        assert!(
            !events.iter().any(|e| matches!(e, SimEvent::Damaged { target, .. } if *target == NetId(2))),
            "a target already struck this swing must not be hit again"
        );
        assert_eq!(world.acs[&NetId(2)].ap, ap_after_first);
    }

    #[test]
    fn damage_hit_count_caps_distinct_targets() {
        let mut world = blade_world(2, 0.5, 0.5);
        let mut events = Vec::new();
        world.step_melee(Tick(0), &mut events);
        let damaged: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SimEvent::Damaged { target, .. } => Some(*target),
                _ => None,
            })
            .collect();
        assert_eq!(damaged.len(), 2, "damage_hit_count_chara = 2");
        assert_eq!(
            world.acs[&NetId(1)]
                .motion
                .blade
                .as_ref()
                .unwrap()
                .hit_entities
                .len(),
            2
        );
        events.clear();
        world.step_melee(Tick(1), &mut events);
        assert!(
            !events.iter().any(|e| matches!(e, SimEvent::Damaged { .. })),
            "the per-swing distinct-target cap must stop further hits"
        );
    }

    #[test]
    fn decay_power_and_recoil_after_first_target() {
        let mut world = blade_world(2, 0.5, 0.5);
        let mut events = Vec::new();
        world.step_melee(Tick(0), &mut events);
        let mut hits: Vec<(f32, f32)> = events
            .iter()
            .filter_map(|e| match e {
                SimEvent::Damaged { amount, recoil, .. } => Some((*amount, *recoil)),
                _ => None,
            })
            .collect();
        assert_eq!(hits.len(), 2);
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        assert!(
            hits[0].0 > hits[1].0,
            "second distinct target must take decayed power: {hits:?}"
        );
        assert!(
            hits[0].1 > hits[1].1,
            "second distinct target must take decayed recoil: {hits:?}"
        );
    }

}
