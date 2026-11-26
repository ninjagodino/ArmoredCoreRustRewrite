//! AC TEST targets from `{map}_{layout}.msb` (`model/map/m4000/m4000_actest.msb`).
//!
//! `mission/ch6000.xml` and the 360 garage AC TEST menu (`0x82361b08`, `0x82361ce0`,
//! `0x825dbb70` returning `6000`) select one `LAYER_PARAM_ST` layer of `m4000_actest.msb`:
//! - `0` / `actest1` (`none`): no enemies
//! - `1` / `actest2` (`light`): 7 `e1020` tanks + 5 `e2030` CHheli
//! - `2` / `actest3` (`mt`): 2 `e0010` atk-mt + 2 `e0110` snp-mt + 2 `e0210` def-mt
//! - `3` / `actest4` (`atk`, CBT): `a0000` (`AcAssemblyDesignSt` 5011)
//! - `4` / `actest5` (`def`): `a0001` (`AcAssemblyDesignSt` 5012)
//! - `5` / `actest6` (`snp`): `a0002` (`AcAssemblyDesignSt` 5013)
//! - `6` / `actest7` (`unac`): vs-UNAC (no MSB parts)
//! - `7` / `actest8` (`eirik`, retail): `a0003` (`AcAssemblyDesignSt` 5014)
//!
//! Each `PARTS_PARAM_ST` kind-2 record carries its layer index at `+0x64` (`0x82b912b0` →
//! `0x82b916b8`). Enemy parts (`e####`) draw `model/ene/{m}/{m}_m.bnd.dcx|{m}.flv`; AC parts
//! (`a####`) carry their `AcAssemblyDesignSt` ID in `MODEL_PARAM_ST` `+0x18` (`0x82b91348` →
//! `0x82b91470`) and assemble with `assemble`, `pose::spawn_static` and `paint`.

use std::collections::HashMap;
use std::sync::Arc;

use acvd_data::generated::ac_unit::{AcAssemblyDesignSt, AC_ASSEMBLY_DESIGN_ST_FILES};
use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::enemy::{
    ENEMY_ENEMYBASICPARAM_BIN, ENEMY_ENEMYCOMMON_BIN, ENEMY_ENEMYGRAPHICSPARAM_BIN,
};
use acvd_data::generated::sfx::PARAM_SFXSESETPARAM_BIN;
use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_data::{find, part_field};
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{ani, bnd3, flver, msb};
use acvd_render::Packs;
use anyhow::Result;
use bevy::audio::AudioSource;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::Affine3A;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;

use crate::{ai, assemble, boost, damage, map, paint, pose, sfx, sound, weapons};

/// `PARTS_PARAM_ST` kind for enemies and test ACs.
const KIND_TARGET: u32 = 2;

/// Which `m4000_actest.msb` target layer `--test` activates.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub enum TestMode {
    /// No targets (`actest1` / `0` / `none`).
    None,
    /// One named `LAYER_PARAM_ST` layer (e.g. `actest2`..`actest8`).
    Layer(String),
    /// Every kind-2 part in the layout MSB.
    All,
}

impl Default for TestMode {
    fn default() -> Self {
        Self::Layer("actest2".to_string())
    }
}

impl TestMode {
    /// Parses `--test <arg>` into a target layer selection.
    /// Accepts `0..=7` (`0x82361b08` indices for `actest1..actest8`), short aliases
    /// (`none`, `light`, `mt`, `atk`, `def`, `snp`, `unac`, `eirik`, `all`), or any literal
    /// `LAYER_PARAM_ST` name.
    pub fn parse(arg: &str) -> Self {
        match arg.trim().to_ascii_lowercase().as_str() {
            "0" | "none" | "actest1" => Self::None,
            "1" | "light" | "tank" | "actest2" => Self::Layer("actest2".into()),
            "2" | "mt" | "actest3" => Self::Layer("actest3".into()),
            "3" | "atk" | "actest4" => Self::Layer("actest4".into()),
            "4" | "def" | "actest5" => Self::Layer("actest5".into()),
            "5" | "snp" | "actest6" => Self::Layer("actest6".into()),
            "6" | "unac" | "actest7" => Self::Layer("actest7".into()),
            "7" | "eirik" | "erik" | "actest8" => Self::Layer("actest8".into()),
            "all" => Self::All,
            other => Self::Layer(other.to_string()),
        }
    }
}

/// Marker on every spawned AC TEST target entity.
#[derive(Component, Debug, Clone)]
#[allow(dead_code)]
pub struct Target {
    pub name: String,
    pub model: String,
    pub layer: String,
    pub ac_design_id: Option<u16>,
}

/// Live AP, KE/CE/TE defense profile, stagger state, hit spheres, and destruction config
/// on a target entity.
#[derive(Component, Debug, Clone)]
pub struct TargetHealth {
    pub ap: f32,
    pub ap_max: f32,
    pub defense: damage::DefenseProfile,
    /// Impact recoil threshold that triggers stagger (`small_stagger_threshold` on enemies,
    /// `stagger_penalty_threshold` = `clashThreshold * 0.8` on ACs per `AcCtrlParamCalc.lua` line 337).
    pub stagger_threshold: f32,
    /// Stagger duration in seconds (`EnemyBasicSt.stagger_penalty_time` or 2.0 s on ACs per
    /// `AcCtrlParamCalc.lua` line 338).
    pub stagger_penalty_time: f32,
    /// Remaining stagger seconds (`> 0.0` lowers defense thresholds to `stagger_threshold`).
    pub stagger_timer: f32,
    /// Local-space vertical hit spheres (`EnemyGraphicsSt` or AC `0x82863b60`).
    pub spheres: Vec<damage::HitSphere>,
    /// True once AP has reached 0.
    pub destroyed: bool,
    /// Set on the lethal hit; consumed by `targets::tick` to play explosion + swap `_b.flv`.
    pub just_destroyed: bool,
    /// Broken FLVER asset path (`model/ene/{m}/{m}_m.bnd.dcx|{m}_b.flv`) for enemy targets.
    pub broken_asset: Option<String>,
    /// Destruction explosion FFX id (`param/sfxsesetparam.bin` rows 501/502: 4019 / 4041).
    pub destroy_sfx: i32,
    /// Destruction sound cue name (`w00009600` / `w00009601` / `c00000089`).
    pub destroy_se: String,
    /// World position of the last incoming hit (`IsLastHitRemember` / `GetLastHitAngle`).
    pub last_hit_from: Option<Vec3>,
    /// Seconds remaining before the last-hit alert expires.
    pub last_hit_timer: f32,
}

impl TargetHealth {
    pub fn is_staggered(&self) -> bool {
        self.stagger_timer > 0.0
    }

    /// Remembers the source of an incoming hit for 5 seconds (`IsLastHitRemember`).
    pub fn record_hit_from(&mut self, origin: Vec3) {
        self.last_hit_from = Some(origin);
        self.last_hit_timer = 5.0;
    }

    /// Applies one projectile hit (`0x827d0b50` + `0x82984438` + `0x82837f30`).
    /// Returns the `HitResult` and whether this hit destroyed the target.
    pub fn apply_hit(
        &mut self,
        attr: damage::DamageType,
        power: f32,
        recoil: f32,
    ) -> (damage::HitResult, bool) {
        let res = damage::calculate_damage(attr, power, recoil, &self.defense, self.is_staggered());
        if self.destroyed {
            return (res, false);
        }
        self.ap = (self.ap - res.damage).max(0.0);
        if self.stagger_threshold > 0.0 && res.recoil >= self.stagger_threshold {
            self.stagger_timer = self.stagger_penalty_time;
        }
        let lethal = self.ap <= 0.0;
        if lethal {
            self.destroyed = true;
            self.just_destroyed = true;
        }
        (res, lethal)
    }

    /// Builds `TargetHealth` for an enemy model (`e0010`, `e1020`, `e2030`, ...) from
    /// `enemy/enemycommon.bin` → `enemybasicparam.bin` + `enemygraphicsparam.bin`.
    pub fn from_enemy(model: &str) -> Self {
        let param_id = enemy_param_id(model).unwrap_or(0);
        let common = find(ENEMY_ENEMYCOMMON_BIN, param_id).map(|r| &r.data);
        let basic_id = common.map_or(param_id, |c| c.basic_param_id);
        let gfx_id = common.map_or(param_id, |c| c.graphics_param_id);
        let basic = find(ENEMY_ENEMYBASICPARAM_BIN, basic_id)
            .or_else(|| find(ENEMY_ENEMYBASICPARAM_BIN, 0))
            .map(|r| &r.data)
            .expect("enemybasicparam row 0");
        let gfx = find(ENEMY_ENEMYGRAPHICSPARAM_BIN, gfx_id)
            .or_else(|| find(ENEMY_ENEMYGRAPHICSPARAM_BIN, 0))
            .map(|r| &r.data)
            .expect("enemygraphicsparam row 0");

        // `param/sfxsesetparam.bin` row 501 (中爆発, FFX 4019, w00009600) for light enemies
        // (tanks/helis, AP < 2500), row 502 (大爆発, FFX 4041, w00009601) for heavier MTs.
        let sfx_set_id = if basic.ap >= 2500.0 { 502 } else { 501 };
        let (destroy_sfx, destroy_se) = find(PARAM_SFXSESETPARAM_BIN, sfx_set_id)
            .map(|r| (i32::from(r.data.sfx_id), format!("w{:08}", r.data.sound_id)))
            .unwrap_or_else(|| (4019, "w00009600".to_string()));

        Self {
            ap: basic.ap.max(1.0),
            ap_max: basic.ap.max(1.0),
            defense: damage::DefenseProfile::from_enemy(basic),
            stagger_threshold: f32::from(basic.small_stagger_threshold),
            stagger_penalty_time: basic.stagger_penalty_time,
            stagger_timer: 0.0,
            spheres: damage::enemy_hit_spheres(gfx),
            destroyed: false,
            just_destroyed: false,
            broken_asset: Some(enemy_broken_asset(model)),
            destroy_sfx,
            destroy_se,
            last_hit_from: None,
            last_hit_timer: 0.0,
        }
    }

    /// Builds `TargetHealth` for a test AC (`a0000`–`a0003`, `AcAssemblyDesignSt` 5011–5014).
    pub fn from_ac(design: &AcAssemblyDesignSt) -> Self {
        // Frame AP sum (`0x8286a600`, `sheets/ac_part_fields.csv` +0x138).
        let ap_max = [
            (design.head, 0u8),
            (design.core, 1),
            (design.arms, 2),
            (design.legs, 3),
        ]
        .into_iter()
        .map(|(id, cat)| part_field(id as i64, cat, "ap").max(0.0))
        .sum::<f32>()
        .max(1.0);
        let ctrl = AcCtrlParam::calculate(design);
        let destroy_sfx =
            find(PARAM_SFXSESETPARAM_BIN, 502).map_or(4041, |r| i32::from(r.data.sfx_id));
        let destroy_se = find(PARAM_ACSOUNDPARAM_BIN, 0)
            .filter(|r| r.data.se_destroy > 0)
            .map(|r| format!("c{:08}", r.data.se_destroy))
            .unwrap_or_else(|| "c00000089".to_string());
        Self {
            ap: ap_max,
            ap_max,
            defense: damage::DefenseProfile::from_ac(design),
            stagger_threshold: ctrl.stagger_penalty_threshold,
            // `AcCtrlParamCalc.lua` line 338: `staggerPenaltyTime = 2`.
            stagger_penalty_time: 2.0,
            stagger_timer: 0.0,
            spheres: damage::ac_hit_spheres(),
            destroyed: false,
            just_destroyed: false,
            broken_asset: None,
            destroy_sfx,
            destroy_se,
            last_hit_from: None,
            last_hit_timer: 0.0,
        }
    }

    /// World-space center of the target's primary hit sphere (for explosion VFX placement).
    pub fn center_offset_y(&self) -> f32 {
        self.spheres.first().map_or(3.0, |s| s.offset_y)
    }
}

/// Parses the `EnemyCommonSt` row ID from an enemy model name (`e0010` → `10`, `e1020` → `1020`).
pub fn enemy_param_id(model: &str) -> Option<u32> {
    model
        .strip_prefix('e')
        .or_else(|| model.strip_prefix('E'))
        .and_then(|s| s.parse::<u32>().ok())
}

/// Broken FLVER asset path of an enemy (`model/ene/{m}/{m}_m.bnd.dcx|{m}_b.flv`).
pub fn enemy_broken_asset(model: &str) -> String {
    format!("model/ene/{model}/{model}_m.bnd.dcx|{model}_b.flv")
}

/// Filters `parts` down to the kind-2 targets active under `mode`.
pub fn select<'a>(parts: &'a [msb::Part], mode: &TestMode) -> Vec<&'a msb::Part> {
    parts
        .iter()
        .filter(|p| p.kind == KIND_TARGET)
        .filter(|p| match mode {
            TestMode::None => false,
            TestMode::All => true,
            TestMode::Layer(wanted) => p
                .layer
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(wanted)),
        })
        .collect()
}

/// Asset path of an enemy's main FLVER (`model/ene/{m}/{m}_m.bnd.dcx|{m}.flv`).
pub fn enemy_asset(model: &str) -> String {
    format!("model/ene/{model}/{model}_m.bnd.dcx|{model}.flv")
}

/// Asset path of an enemy's animation binder (`model/ene/{m}/{m}_a.bnd.dcx`).
pub fn enemy_anim_binder(model: &str) -> String {
    format!("model/ene/{model}/{model}_a.bnd.dcx")
}

/// Procedural joint-control spec for one enemy bone (from `param/enemyjcondata.xml`).
#[derive(Clone, Copy, Debug)]
pub struct EnemyJcon {
    pub bone_idx: usize,
    pub min_yaw: f32,
    pub max_yaw: f32,
    pub min_pitch: f32,
    pub max_pitch: f32,
    pub max_vel: f32,
}

/// Shared per-enemy-model animation clips and skeleton mapping loaded from
/// `model/ene/{m}/{m}_a.bnd.dcx` + `model/ene/{m}/{m}_m.bnd.dcx|{m}.flv`.
#[derive(Debug)]
pub struct EnemyAnimBank {
    pub clips: HashMap<u16, ani::Anim>,
    /// Local bind transform (in Bevy axes) for each FLVER rig bone.
    pub rest_locals: Vec<Transform>,
    /// FLVER-space local translation for each rig bone (for delta-translation animation).
    pub bind_t: Vec<Vec3>,
    /// Parent index in `rig.bones`.
    pub parents: Vec<Option<usize>>,
    /// Mapping from FLVER rig bone index → `.ani` clip bone index (matched by bone name).
    pub ani_bone_map: Vec<Option<usize>>,
    /// Whether this rig bone is a root/master bone (held in place when animating).
    pub is_root: Vec<bool>,
    /// Optional turret/upper-body yaw tracking bone (`y` on `e1020`, `body_01` on `e0110`,
    /// `l_arm_03` on `e0210`; `param/enemyjcondata.xml`).
    pub aim_yaw: Option<EnemyJcon>,
    /// Optional gun barrel pitch tracking bone (`x` on `e1020`, `body_01` on `e0110`,
    /// `l_arm_03` on `e0210`; `param/enemyjcondata.xml`).
    pub aim_pitch: Option<EnemyJcon>,
}

pub(crate) fn xform_to_affine(xf: &flver::Xform) -> Affine3A {
    Affine3A::from_cols(
        Vec3::new(xf.m[0][0], xf.m[1][0], xf.m[2][0]).into(),
        Vec3::new(xf.m[0][1], xf.m[1][1], xf.m[2][1]).into(),
        Vec3::new(xf.m[0][2], xf.m[1][2], xf.m[2][2]).into(),
        Vec3::from(xf.t).into(),
    )
}

impl EnemyAnimBank {
    /// Loads every `a00_NNN.ani` clip from `model/ene/{model}/{model}_a.bnd.dcx` and maps its
    /// bone tracks onto `rig`.
    pub fn load(disc: &Disc, model: &str, rig: &acvd_render::Rig) -> Arc<Self> {
        Self::load_from_binder(disc, &enemy_anim_binder(model), rig, true)
    }

    /// Loads every `a00_NNN.ani` clip from a map object's `model/obj/{model}/{model}_a.bnd.dcx`
    /// and maps its bone tracks onto `rig`. Map-object animations carry no `enemyjcondata`
    /// turret/aim bones (`o9310`/`o9330` are formations, not turrets).
    pub fn load_object(disc: &Disc, model: &str, rig: &acvd_render::Rig) -> Arc<Self> {
        Self::load_from_binder(
            disc,
            &format!("model/obj/{model}/{model}_a.bnd.dcx"),
            rig,
            false,
        )
    }

    fn load_from_binder(
        disc: &Disc,
        binder_path: &str,
        rig: &acvd_render::Rig,
        with_jcon: bool,
    ) -> Arc<Self> {
        let mut clips = HashMap::new();
        if let Ok(binder_bytes) = vfs::open(disc, binder_path) {
            if let Ok(bnd) = bnd3::read(&binder_bytes) {
                for (i, entry) in bnd.entries.iter().enumerate() {
                    let Some(name) = entry.name.as_deref() else {
                        continue;
                    };
                    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
                    let lower = base.to_ascii_lowercase();
                    let Some(num_str) = lower
                        .strip_prefix("a00_")
                        .and_then(|s| s.strip_suffix(".ani"))
                    else {
                        continue;
                    };
                    let Ok(id) = num_str.parse::<u16>() else {
                        continue;
                    };
                    if let Ok(raw) = vfs::entry(&binder_bytes, &format!("#{i}")) {
                        if let Ok(clip) = ani::read(&raw) {
                            clips.insert(id, clip);
                        }
                    }
                }
            }
        }

        // Reference skeleton from clip 0 (or any loaded clip) to map bone names -> clip indices.
        let ref_clip = clips.get(&0).or_else(|| clips.values().next());
        let mut name_to_ani: HashMap<String, usize> = HashMap::new();
        if let Some(c) = ref_clip {
            for (idx, b) in c.bones.iter().enumerate() {
                if let Some(r) = &b.rest {
                    name_to_ani.insert(r.name.to_ascii_lowercase(), idx);
                }
            }
        }

        let mut rest_locals = Vec::with_capacity(rig.bones.len());
        let mut bind_t = Vec::with_capacity(rig.bones.len());
        let mut parents = Vec::with_capacity(rig.bones.len());
        let mut ani_bone_map = Vec::with_capacity(rig.bones.len());
        let mut is_root = Vec::with_capacity(rig.bones.len());
        let mut bone_by_name: HashMap<String, usize> = HashMap::new();

        for (i, b) in rig.bones.iter().enumerate() {
            let parent = b.parent;
            let parent_bind =
                parent.map_or(Affine3A::IDENTITY, |p| xform_to_affine(&rig.bones[p].bind));
            let local = parent_bind.inverse() * xform_to_affine(&b.bind);
            let (s, r, t) = local.to_scale_rotation_translation();
            rest_locals.push(pose::to_transform(t, r, s));
            bind_t.push(t);
            parents.push(parent);
            let lower = b.name.to_ascii_lowercase();
            bone_by_name.insert(lower.clone(), i);
            ani_bone_map.push(name_to_ani.get(&lower).copied());
            is_root.push(parent.is_none() || lower == "master");
        }

        // Joint-control specs from `param/enemyjcondata.xml`:
        // - `e1020` (`[03] Cannon`): `y` (±180° yaw, 0.5 rad/s? -> 60 deg/s), `x` (-30..70° pitch).
        // - `e0110` (`[08] e0110`): `body_01` (±60° yaw, -30..40° pitch).
        // - `e0210` (`[07] e0210`): `l_arm_03` (-75..65° yaw, -15..60° pitch).
        let (aim_yaw, aim_pitch) = if !with_jcon {
            (None, None)
        } else if let (Some(&by), Some(&bx)) = (bone_by_name.get("y"), bone_by_name.get("x")) {
            (
                Some(EnemyJcon {
                    bone_idx: by,
                    min_yaw: -180.0_f32.to_radians(),
                    max_yaw: 180.0_f32.to_radians(),
                    min_pitch: 0.0,
                    max_pitch: 0.0,
                    max_vel: 60.0_f32.to_radians(),
                }),
                Some(EnemyJcon {
                    bone_idx: bx,
                    min_yaw: 0.0,
                    max_yaw: 0.0,
                    min_pitch: -30.0_f32.to_radians(),
                    max_pitch: 70.0_f32.to_radians(),
                    max_vel: 45.0_f32.to_radians(),
                }),
            )
        } else if let Some(&b) = bone_by_name.get("body_01") {
            let jc = EnemyJcon {
                bone_idx: b,
                min_yaw: -60.0_f32.to_radians(),
                max_yaw: 60.0_f32.to_radians(),
                min_pitch: -30.0_f32.to_radians(),
                max_pitch: 40.0_f32.to_radians(),
                max_vel: 90.0_f32.to_radians(),
            };
            (Some(jc), Some(jc))
        } else if let Some(&b) = bone_by_name.get("l_arm_03") {
            let jc = EnemyJcon {
                bone_idx: b,
                min_yaw: -75.0_f32.to_radians(),
                max_yaw: 65.0_f32.to_radians(),
                min_pitch: -15.0_f32.to_radians(),
                max_pitch: 60.0_f32.to_radians(),
                max_vel: 90.0_f32.to_radians(),
            };
            (Some(jc), Some(jc))
        } else {
            (None, None)
        };

        Arc::new(Self {
            clips,
            rest_locals,
            bind_t,
            parents,
            ani_bone_map,
            is_root,
            aim_yaw,
            aim_pitch,
        })
    }

    /// Picks the first clip ID in `candidates` that exists in this enemy's `_a.bnd.dcx`.
    pub fn pick_clip(&self, candidates: &[u16]) -> Option<u16> {
        candidates
            .iter()
            .copied()
            .find(|id| self.clips.contains_key(id))
    }
}

/// Per-entity skeletal animation controller for non-AC enemies (`e1020`, `e2030`, `e0010`,
/// `e0110`, `e0210`).
#[derive(Component, Debug)]
pub struct EnemyAnim {
    pub bank: Arc<EnemyAnimBank>,
    pub joints: Vec<Entity>,
    pub clip_id: u16,
    pub frame: f32,
    pub looping: bool,
    pub speed: f32,
    pub blend_t: f32,
    pub blend_secs: f32,
    pub blend_from: Vec<Transform>,
    pub turret_yaw: f32,
    pub turret_pitch: f32,
    pub fire_anim_timer: f32,
    pub prev_yaw: f32,
}

impl EnemyAnim {
    pub fn new(bank: Arc<EnemyAnimBank>, joints: Vec<Entity>, init_yaw: f32) -> Self {
        let blend_from = bank.rest_locals.clone();
        let clip_id = bank.pick_clip(&[0]).unwrap_or(0);
        Self {
            bank,
            joints,
            clip_id,
            frame: 0.0,
            looping: true,
            speed: 1.0,
            blend_t: 1.0,
            blend_secs: 0.0,
            blend_from,
            turret_yaw: 0.0,
            turret_pitch: 0.0,
            fire_anim_timer: 0.0,
            prev_yaw: init_yaw,
        }
    }

    /// Transitions to `clip_id` with a crossfade over `fade_secs` if not already playing it.
    pub fn play(&mut self, clip_id: u16, looping: bool, speed: f32, fade_secs: f32) {
        if !self.bank.clips.contains_key(&clip_id) {
            return;
        }
        if self.clip_id != clip_id {
            self.clip_id = clip_id;
            self.frame = 0.0;
            self.blend_t = 0.0;
            self.blend_secs = fade_secs;
        }
        self.looping = looping;
        self.speed = speed;
    }

    /// Selects the enemy's animation clip from its AI movement/combat state.
    #[allow(clippy::too_many_arguments)]
    pub fn select_state(
        &mut self,
        archetype: ai::Archetype,
        local_vel: Vec3,
        walk_speed: f32,
        yaw_rate: f32,
        sub_state: u8,
        staggered: bool,
        fired: bool,
        dt: f32,
    ) {
        if fired {
            self.fire_anim_timer = 0.45;
        } else if self.fire_anim_timer > 0.0 {
            self.fire_anim_timer = (self.fire_anim_timer - dt).max(0.0);
        }

        if staggered {
            if let Some(id) = self.bank.pick_clip(&[48, 49, 0]) {
                self.play(id, false, 1.0, 0.1);
                return;
            }
        }

        let speed = Vec2::new(local_vel.x, local_vel.z).length();
        // Local forward is -Z in Bevy axes.
        let fwd = -local_vel.z;
        let right = local_vel.x;

        match archetype {
            ai::Archetype::Heli => {
                // `e2030`: all clips include full `pro_l`/`pro_r` rotor spin + body tilt.
                let candidates: &[u16] = if speed < 2.0 {
                    &[0]
                } else if fwd.abs() >= right.abs() {
                    if fwd >= 0.0 {
                        if speed > walk_speed * 0.8 {
                            &[17, 2, 0]
                        } else {
                            &[2, 0]
                        }
                    } else if speed > walk_speed * 0.8 {
                        &[18, 8, 0]
                    } else {
                        &[8, 0]
                    }
                } else if right >= 0.0 {
                    &[12, 22, 2, 0]
                } else {
                    &[10, 21, 2, 0]
                };
                if let Some(id) = self.bank.pick_clip(candidates) {
                    self.play(id, true, 1.0, 0.2);
                }
            }
            ai::Archetype::Tank => {
                // `e1020`: hull clip `0` (idle) or `2`/`8` (drive); turret `y`/`x` tracked procedurally.
                let candidates: &[u16] = if speed > 1.5 {
                    if fwd >= 0.0 {
                        &[2, 0]
                    } else {
                        &[8, 2, 0]
                    }
                } else {
                    &[0]
                };
                if let Some(id) = self.bank.pick_clip(candidates) {
                    self.play(id, true, 1.0, 0.15);
                }
            }
            ai::Archetype::AtkMt | ai::Archetype::SnpMt | ai::Archetype::DefMt => {
                if self.fire_anim_timer > 0.0 && speed < 2.5 {
                    if let Some(id) = self.bank.pick_clip(&[220, 210, 75, 0]) {
                        self.play(id, true, 1.0, 0.12);
                        return;
                    }
                }
                if archetype == ai::Archetype::SnpMt && sub_state == 0 && speed < 1.5 {
                    if let Some(id) = self.bank.pick_clip(&[75, 76, 0]) {
                        self.play(id, true, 1.0, 0.2);
                        return;
                    }
                }
                if speed >= 1.2 {
                    let dashing = speed > walk_speed.max(6.0) * 1.1;
                    let candidates: &[u16] = if dashing {
                        if fwd.abs() >= right.abs() {
                            if fwd >= 0.0 {
                                &[17, 55, 2, 0]
                            } else {
                                &[18, 56, 8, 2, 0]
                            }
                        } else if right >= 0.0 {
                            &[22, 12, 2, 0]
                        } else {
                            &[21, 10, 2, 0]
                        }
                    } else if fwd.abs() >= right.abs() {
                        if fwd >= 0.0 {
                            &[2, 1, 0]
                        } else {
                            &[8, 7, 2, 0]
                        }
                    } else if right >= 0.0 {
                        &[12, 11, 2, 0]
                    } else {
                        &[10, 9, 2, 0]
                    };
                    if let Some(id) = self.bank.pick_clip(candidates) {
                        let rate = (speed / walk_speed.max(6.0)).clamp(0.75, 1.45);
                        self.play(id, true, rate, 0.15);
                    }
                } else if yaw_rate.abs() > 0.2 {
                    let candidates: &[u16] = if yaw_rate > 0.0 {
                        &[33, 34, 0]
                    } else {
                        &[34, 33, 0]
                    };
                    if let Some(id) = self.bank.pick_clip(candidates) {
                        self.play(id, true, 1.0, 0.15);
                    }
                } else if let Some(id) = self.bank.pick_clip(&[0]) {
                    self.play(id, true, 1.0, 0.18);
                }
            }
            _ => {}
        }
    }

    /// Advances the active clip clock and updates turret tracking angles by `dt` seconds.
    pub fn step(&mut self, dt: f32, aim_yaw_pitch: Option<(f32, f32)>) {
        if let Some(clip) = self.bank.clips.get(&self.clip_id) {
            let frames = clip.frames.max(1) as f32;
            let next = self.frame + dt * pose::FRAME_RATE * self.speed;
            self.frame = if self.looping {
                next % frames
            } else {
                next.min(frames - 1.0)
            };
        }
        self.blend_t += dt;

        // Slew procedural turret / upper-body aim angles within `enemyjcondata.xml` limits.
        if let Some(jc) = self.bank.aim_yaw {
            let goal = aim_yaw_pitch
                .map(|(y, _)| y.clamp(jc.min_yaw, jc.max_yaw))
                .unwrap_or(0.0);
            let step = jc.max_vel * dt;
            self.turret_yaw += (goal - self.turret_yaw).clamp(-step, step);
        }
        if let Some(jc) = self.bank.aim_pitch {
            let goal = aim_yaw_pitch
                .map(|(_, p)| p.clamp(jc.min_pitch, jc.max_pitch))
                .unwrap_or(0.0);
            let step = jc.max_vel * dt;
            self.turret_pitch += (goal - self.turret_pitch).clamp(-step, step);
        }
    }

    /// Computes the posed local [`Transform`] for rig bone `i` at the current frame.
    pub fn sample_bone(&self, i: usize) -> Transform {
        let mut out = self.bank.rest_locals[i];
        if let Some(clip) = self.bank.clips.get(&self.clip_id) {
            if let Some(ani_idx) = self.bank.ani_bone_map[i] {
                if !self.bank.is_root[i] {
                    if let Some(cbone) = clip.bones.get(ani_idx) {
                        let rest = cbone.rest.as_ref();
                        let rot = Quat::from_array(
                            cbone
                                .track
                                .rotation(self.frame)
                                .or_else(|| rest.map(|r| ani::euler_quat(r.euler)))
                                .unwrap_or([0.0, 0.0, 0.0, 1.0]),
                        );
                        let rest_t = rest.map_or(Vec3::ZERO, |r| Vec3::from(r.translation));
                        let ct = cbone
                            .track
                            .translation(self.frame)
                            .map(Vec3::from)
                            .unwrap_or(rest_t);
                        let cs = cbone
                            .track
                            .scale(self.frame)
                            .map(Vec3::from)
                            .unwrap_or(Vec3::ONE);
                        let translation = self.bank.bind_t[i] + (ct - rest_t);
                        out = pose::to_transform(translation, rot, cs);
                    }
                }
            }
        }
        if self.blend_secs > 0.0 && self.blend_t < self.blend_secs {
            let w = (self.blend_t / self.blend_secs).clamp(0.0, 1.0);
            let from = self.blend_from[i];
            out = Transform {
                translation: from.translation.lerp(out.translation, w),
                rotation: from.rotation.slerp(out.rotation, w),
                scale: from.scale.lerp(out.scale, w),
            };
        }
        // Apply procedural turret / weapon aim tracking (`enemyjcondata.xml`).
        if let Some(jc) = self.bank.aim_yaw {
            if jc.bone_idx == i && self.turret_yaw.abs() > 1e-4 {
                out.rotation = Quat::from_rotation_y(self.turret_yaw) * out.rotation;
            }
        }
        if let Some(jc) = self.bank.aim_pitch {
            if jc.bone_idx == i && self.turret_pitch.abs() > 1e-4 {
                out.rotation = out.rotation * Quat::from_rotation_x(self.turret_pitch);
            }
        }
        out
    }
}

/// Steps all living [`EnemyAnim`] skeletons and writes their posed joint transforms.
pub fn animate_enemies(
    time: Res<Time>,
    mut enemies: Query<(&mut EnemyAnim, Option<&TargetHealth>)>,
    mut transforms: Query<&mut Transform>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for (mut anim, health) in &mut enemies {
        if health.is_some_and(|h| h.destroyed) {
            continue;
        }
        let was_blending = anim.blend_secs > 0.0 && anim.blend_t == 0.0;
        if was_blending {
            let EnemyAnim {
                joints, blend_from, ..
            } = &mut *anim;
            for (i, &ent) in joints.iter().enumerate() {
                if let Ok(tf) = transforms.get(ent) {
                    blend_from[i] = *tf;
                }
            }
        }
        let n = anim.joints.len();
        for i in 0..n {
            let ent = anim.joints[i];
            if let Ok(mut tf) = transforms.get_mut(ent) {
                *tf = anim.sample_bone(i);
            }
        }
    }
}

fn find_design(id: u16) -> Option<&'static AcAssemblyDesignSt> {
    let id = u32::from(id);
    AC_ASSEMBLY_DESIGN_ST_FILES
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .find(|r| r.id == id)
        .map(|r| &r.data)
}

/// Spawns the active AC TEST targets from `model/map/{map}/{map}_{layout}.msb`.
/// Returns the number of target entities spawned.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    paint_materials: &mut Assets<paint::AcPaintMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    disc: &Disc,
    enemy_parts: &weapons::EnemyParts,
    map_id: &str,
    layout: &str,
    mode: &TestMode,
    flat: bool,
) -> Result<usize> {
    if matches!(mode, TestMode::None) {
        return Ok(0);
    }
    let path = format!("{}/{map_id}_{layout}.msb", map::folder(map_id));
    if !disc.exists(&path) {
        return Ok(0);
    }
    let parts = msb::parts(&vfs::open(disc, &path)?)?;
    let chosen = select(&parts, mode);
    if chosen.is_empty() {
        info!("targets ({path}, {mode:?}): 0 parts");
        return Ok(0);
    }

    struct CachedEnemy {
        meshes: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
        inv_bindposes: Handle<SkinnedMeshInverseBindposes>,
        bank: Arc<EnemyAnimBank>,
    }

    let mut packs = Packs::default();
    let mut missing = 0usize;
    let mut enemy_cache: HashMap<String, Option<CachedEnemy>> = HashMap::new();
    let mut spawned = 0usize;

    for part in chosen {
        let xf = map::bevy_transform(&map::xform(part));
        let layer = part.layer.clone().unwrap_or_default();
        if let Some(design_id) = part.ac_design_id {
            if spawn_ac_target(
                commands,
                meshes,
                materials,
                paint_materials,
                images,
                bindposes,
                disc,
                &mut packs,
                &mut missing,
                part,
                &layer,
                design_id,
                xf,
                flat,
            ) {
                spawned += 1;
            }
            continue;
        }
        let asset = enemy_asset(&part.model);
        let entry =
            enemy_cache.entry(part.model.clone()).or_insert_with(
                || match acvd_render::rigged_model(disc, &asset, &|_| {
                    acvd_render::RootPlace::IDENTITY
                }) {
                    Ok((list, rig)) if !list.is_empty() => {
                        let bank = EnemyAnimBank::load(disc, &part.model, &rig);
                        let unboned = Affine3A::from_translation(Vec3::from(rig.unboned));
                        let binds: Vec<Mat4> = rig
                            .bones
                            .iter()
                            .map(|b| xform_to_affine(&b.bind))
                            .chain([unboned])
                            .map(|b| Mat4::from(pose::mirror(b).inverse()))
                            .collect();
                        let inv_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds));
                        let meshes_mats = list
                            .into_iter()
                            .map(|m| {
                                let mat = acvd_render::material(
                                    disc,
                                    &m,
                                    &mut packs,
                                    images,
                                    flat,
                                    &mut missing,
                                );
                                (meshes.add(m.mesh), materials.add(mat))
                            })
                            .collect();
                        Some(CachedEnemy {
                            meshes: meshes_mats,
                            inv_bindposes,
                            bank,
                        })
                    }
                    Ok(_) => None,
                    Err(e) => {
                        warn!("target {} ({asset}): {e:#}", part.name);
                        None
                    }
                },
            );
        let Some(cached) = entry.as_ref() else {
            continue;
        };
        let health = TargetHealth::from_enemy(&part.model);
        let init_yaw = ai::yaw_from_quat(xf.rotation);
        let enemy_ai = ai::EnemyAi::for_enemy(&part.model, xf.translation, init_yaw, enemy_parts);
        let root = commands
            .spawn((
                Target {
                    name: part.name.clone(),
                    model: part.model.clone(),
                    layer,
                    ac_design_id: None,
                },
                health,
                enemy_ai,
                Name::new(part.name.clone()),
                xf,
                Visibility::default(),
            ))
            .id();

        // Spawn skeletal joint hierarchy for this enemy instance.
        let mut joint_entities: Vec<Entity> = Vec::with_capacity(cached.bank.rest_locals.len());
        for _ in 0..cached.bank.rest_locals.len() {
            joint_entities.push(
                commands
                    .spawn((Transform::default(), Visibility::default()))
                    .id(),
            );
        }
        for (i, &ent) in joint_entities.iter().enumerate() {
            let parent_ent = cached.bank.parents[i].map_or(root, |p| joint_entities[p]);
            commands
                .entity(ent)
                .insert((cached.bank.rest_locals[i], ChildOf(parent_ent)));
        }
        let extra_unboned = commands
            .spawn((Transform::default(), Visibility::default(), ChildOf(root)))
            .id();
        let mut skin_joints = joint_entities.clone();
        skin_joints.push(extra_unboned);

        for (mesh, mat) in &cached.meshes {
            commands.spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                SkinnedMesh {
                    inverse_bindposes: cached.inv_bindposes.clone(),
                    joints: skin_joints.clone(),
                },
                NoFrustumCulling,
                Transform::default(),
                ChildOf(root),
            ));
        }

        commands.entity(root).insert(EnemyAnim::new(
            cached.bank.clone(),
            joint_entities,
            init_yaw,
        ));
        spawned += 1;
    }

    if missing > 0 {
        warn!("targets ({path}): {missing} textures missing");
    }
    info!("targets ({path}, {mode:?}): {spawned} spawned");
    Ok(spawned)
}

#[allow(clippy::too_many_arguments)]
fn spawn_ac_target(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    paint_materials: &mut Assets<paint::AcPaintMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    disc: &Disc,
    packs: &mut Packs,
    missing: &mut usize,
    part: &msb::Part,
    layer: &str,
    design_id: u16,
    xf: Transform,
    flat: bool,
) -> bool {
    let Some(design) = find_design(design_id) else {
        warn!(
            "target {} ({}): design {design_id} not found",
            part.name, part.model
        );
        return false;
    };
    let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, design);
    let accolor = (!flat)
        .then(|| paint::load_accolor(disc, design.coloring_id))
        .flatten();
    let mut loaded = Vec::new();
    let mut known = vec![None; built.placements.len()];
    for (index, p) in built.placements.iter().enumerate() {
        let places = assemble::root_places(p, &built.placements, &known);
        match acvd_render::rigged_model(disc, p.model.path, &|root| {
            assemble::place_of(&places, root)
        }) {
            Ok((part_meshes, rig)) => {
                known[index] = Some(assemble::Stored {
                    places,
                    frames: rig.frames.clone(),
                });
                loaded.push(pose::Loaded {
                    index,
                    placement: p,
                    meshes: part_meshes,
                    rig,
                });
            }
            Err(e) => warn!("target {} part {}: {e:#}", part.name, p.model.path),
        }
    }
    let rig = match pose::build(disc, &loaded) {
        Ok(r) => r,
        Err(e) => {
            warn!("target {} rig: {e:#}", part.name);
            return false;
        }
    };
    let health = TargetHealth::from_ac(design);
    let enemy_ai = ai::EnemyAi::for_ac(
        design_id,
        design,
        xf.translation,
        ai::yaw_from_quat(xf.rotation),
    );
    let root = commands
        .spawn((
            Target {
                name: part.name.clone(),
                model: part.model.clone(),
                layer: layer.to_string(),
                ac_design_id: Some(design_id),
            },
            health,
            enemy_ai,
            Name::new(part.name.clone()),
            xf,
            Visibility::default(),
        ))
        .id();
    let (npc_motion, _, skins) = pose::spawn_npc(commands, root, rig, &loaded);
    if let Some(npc) = npc_motion {
        commands.entity(root).insert(npc);
    }
    for (part_loaded, (joints, binds)) in loaded.into_iter().zip(skins) {
        boost::fold(commands, part_loaded.placement, &part_loaded.rig, &joints);
        let inverse_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds));
        for mesh in part_loaded.meshes {
            let base = acvd_render::material(disc, &mesh, packs, images, flat, missing);
            let painted = accolor.as_ref().and_then(|col| {
                paint::material(
                    disc,
                    &base,
                    &mesh,
                    part_loaded.placement.column,
                    col,
                    None,
                    packs,
                    images,
                )
            });
            let mut e = commands.spawn((
                Mesh3d(meshes.add(mesh.mesh)),
                SkinnedMesh {
                    inverse_bindposes: inverse_bindposes.clone(),
                    joints: joints.clone(),
                },
                NoFrustumCulling,
                Transform::default(),
                ChildOf(root),
            ));
            match painted {
                Some(mat) => {
                    e.insert(MeshMaterial3d(paint_materials.add(mat)));
                }
                None => {
                    e.insert(MeshMaterial3d(materials.add(base)));
                }
            }
        }
    }
    true
}

/// Steps target stagger timers and handles destruction (explosion FFX/SFX + shattered skinned
/// `_b.flv` gib physics via [`crate::debris::spawn`], or despawn for destroyed AC targets).
#[allow(clippy::too_many_arguments)]
pub fn tick(
    time: Res<Time>,
    garage: Res<crate::Garage>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut cues: ResMut<sound::Cues>,
    mut audio_sources: ResMut<Assets<AudioSource>>,
    mut targets: Query<(
        Entity,
        &Target,
        &mut TargetHealth,
        &Transform,
        Option<&ai::EnemyAi>,
    )>,
) {
    let dt = time.delta_secs();
    let mut packs = Packs::default();
    for (entity, target, mut health, xf, enemy_ai) in &mut targets {
        if health.stagger_timer > 0.0 {
            health.stagger_timer = (health.stagger_timer - dt).max(0.0);
        }
        if health.last_hit_timer > 0.0 {
            health.last_hit_timer = (health.last_hit_timer - dt).max(0.0);
        }
        if !health.just_destroyed {
            continue;
        }
        health.just_destroyed = false;
        let center = xf.translation + Vec3::Y * health.center_offset_y();
        if health.destroy_sfx > 0 {
            commands.spawn((
                sfx::Sfx::new(health.destroy_sfx),
                Transform::from_translation(center),
            ));
        }
        if !health.destroy_se.is_empty() {
            sound::cue_at(
                &mut commands,
                &mut cues,
                &mut audio_sources,
                &health.destroy_se,
                center,
            );
        }
        info!(
            "target {} ({}) destroyed at ({:.1}, {:.1}, {:.1})",
            target.name, target.model, center.x, center.y, center.z
        );

        let init_vel = enemy_ai.map_or(Vec3::ZERO, |a| a.velocity);
        if let Some(asset) = health.broken_asset.as_deref() {
            crate::debris::spawn(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut bindposes,
                &garage.disc,
                &mut packs,
                garage.flat,
                asset,
                &target.name,
                crate::debris::DebrisKind::Enemy,
                *xf,
                init_vel,
                Vec3::ZERO,
            );
        }
        commands.entity(entity).try_despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::generated::ctrl::AcCtrlParam;

    #[test]
    fn parse_mode_maps_ch6000_layers_and_aliases() {
        assert_eq!(TestMode::parse("0"), TestMode::None);
        assert_eq!(TestMode::parse("none"), TestMode::None);
        assert_eq!(TestMode::parse("actest1"), TestMode::None);
        assert_eq!(TestMode::parse("1"), TestMode::Layer("actest2".into()));
        assert_eq!(TestMode::parse("light"), TestMode::Layer("actest2".into()));
        assert_eq!(TestMode::parse("2"), TestMode::Layer("actest3".into()));
        assert_eq!(TestMode::parse("mt"), TestMode::Layer("actest3".into()));
        assert_eq!(TestMode::parse("3"), TestMode::Layer("actest4".into()));
        assert_eq!(TestMode::parse("4"), TestMode::Layer("actest5".into()));
        assert_eq!(TestMode::parse("5"), TestMode::Layer("actest6".into()));
        assert_eq!(TestMode::parse("6"), TestMode::Layer("actest7".into()));
        assert_eq!(TestMode::parse("7"), TestMode::Layer("actest8".into()));
        assert_eq!(TestMode::parse("eirik"), TestMode::Layer("actest8".into()));
        assert_eq!(TestMode::parse("all"), TestMode::All);
    }

    #[test]
    fn m4000_actest_targets_resolve_on_disc() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let parts =
            msb::parts(&vfs::open(&disc, "model/map/m4000/m4000_actest.msb").unwrap()).unwrap();
        assert_eq!(select(&parts, &TestMode::parse("0")).len(), 0);
        assert_eq!(select(&parts, &TestMode::parse("1")).len(), 12);
        assert_eq!(select(&parts, &TestMode::parse("2")).len(), 6);
        assert_eq!(select(&parts, &TestMode::parse("3")).len(), 1);
        assert_eq!(select(&parts, &TestMode::parse("4")).len(), 1);
        assert_eq!(select(&parts, &TestMode::parse("5")).len(), 1);
        assert_eq!(select(&parts, &TestMode::parse("6")).len(), 0);
        assert_eq!(select(&parts, &TestMode::parse("7")).len(), 1);
        assert_eq!(select(&parts, &TestMode::parse("all")).len(), 22);

        for part in select(&parts, &TestMode::All) {
            if let Some(id) = part.ac_design_id {
                let d = find_design(id).unwrap_or_else(|| panic!("missing design {id}"));
                assert!(AcCtrlParam::buildable(d), "design {id} not buildable");
                let hp = TargetHealth::from_ac(d);
                assert!(hp.ap_max > 10000.0, "AC design {id} AP {}", hp.ap_max);
                assert_eq!(hp.spheres.len(), 2);
            } else {
                let asset = enemy_asset(&part.model);
                assert!(
                    acvd_render::model(&disc, &asset).is_ok_and(|list| !list.is_empty()),
                    "enemy FLVER failed to load: {asset}"
                );
                let broken = enemy_broken_asset(&part.model);
                assert!(
                    acvd_render::model(&disc, &broken).is_ok_and(|list| !list.is_empty()),
                    "broken enemy FLVER failed to load: {broken}"
                );
                let mut hp = TargetHealth::from_enemy(&part.model);
                assert!(hp.ap_max >= 500.0, "{} AP {}", part.model, hp.ap_max);
                let (res, lethal) = hp.apply_hit(damage::DamageType::Ke, 50000.0, 2000.0);
                assert!(res.effective && lethal && hp.destroyed && hp.just_destroyed);
            }
        }
    }

    #[test]
    fn enemy_anim_banks_and_playback_work_for_all_actest_enemies() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        for (model, arch) in [
            ("e1020", ai::Archetype::Tank),
            ("e2030", ai::Archetype::Heli),
            ("e0010", ai::Archetype::AtkMt),
            ("e0110", ai::Archetype::SnpMt),
            ("e0210", ai::Archetype::DefMt),
        ] {
            let asset = enemy_asset(model);
            let (meshes, rig) =
                acvd_render::rigged_model(&disc, &asset, &|_| acvd_render::RootPlace::IDENTITY)
                    .unwrap_or_else(|e| panic!("{model} rigged_model: {e:#}"));
            assert!(!meshes.is_empty());
            let bank = EnemyAnimBank::load(&disc, model, &rig);
            assert!(
                !bank.clips.is_empty(),
                "{model} should load at least one .ani clip"
            );
            assert!(bank.clips.contains_key(&0), "{model} missing idle clip 0");

            let mut anim = EnemyAnim::new(bank.clone(), Vec::new(), 0.0);
            // Drive forward movement and verify clip advances + bone poses stay finite.
            anim.select_state(
                arch,
                Vec3::new(0.0, 0.0, -12.0),
                10.0,
                0.0,
                0,
                false,
                false,
                0.016,
            );
            for _ in 0..30 {
                anim.step(1.0 / 60.0, Some((0.5, 0.2)));
            }
            for i in 0..bank.rest_locals.len() {
                let tf = anim.sample_bone(i);
                assert!(
                    tf.translation.is_finite(),
                    "{model} bone {i} non-finite translation"
                );
                assert!(
                    tf.rotation.is_finite(),
                    "{model} bone {i} non-finite rotation"
                );
            }

            if model == "e1020" {
                assert!(bank.aim_yaw.is_some() && bank.aim_pitch.is_some());
                assert!(
                    anim.turret_yaw > 0.1,
                    "e1020 turret yaw should track player"
                );
                assert!(
                    anim.turret_pitch > 0.05,
                    "e1020 barrel pitch should track player"
                );
            }
            if model == "e2030" {
                // Verify rotor bone `pro_l` actually rotates across frames.
                let pro_l = rig
                    .bones
                    .iter()
                    .position(|b| b.name.eq_ignore_ascii_case("pro_l"))
                    .expect("e2030 pro_l rotor bone");
                let r0 = anim.sample_bone(pro_l).rotation;
                anim.step(0.1, None);
                let r1 = anim.sample_bone(pro_l).rotation;
                assert!(
                    r0.angle_between(r1) > 0.05,
                    "e2030 pro_l rotor should spin across frames"
                );
            }
        }
    }

    #[test]
    fn test_ac_npc_motion_locomotion_clips_and_core_ctrl() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let design = find_design(5011).expect("design 5011");
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, design);
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            let places = assemble::root_places(p, &built.placements, &known);
            let (part_meshes, rig) = acvd_render::rigged_model(&disc, p.model.path, &|root| {
                assemble::place_of(&places, root)
            })
            .unwrap();
            known[index] = Some(assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(pose::Loaded {
                index,
                placement: p,
                meshes: part_meshes,
                rig,
            });
        }
        let rig = pose::build(&disc, &loaded).unwrap();
        let mut world = World::new();
        let root_ent = world.spawn_empty().id();
        let (npc_opt, _, _) = {
            let mut cmds = world.commands();
            let res = pose::spawn_npc(&mut cmds, root_ent, rig, &loaded);
            world.flush();
            res
        };
        let mut npc = npc_opt.expect("design 5011 should build NpcMotion");
        // Idle -> Walk -> Dash wheel transitions.
        npc.set_locomotion(&disc, Vec3::ZERO, 12.0, Some((0.4, 0.1)));
        assert_eq!(npc.motion.current_anim_id, 0);
        npc.set_locomotion(&disc, Vec3::new(0.0, 0.0, -8.0), 12.0, Some((0.4, 0.1)));
        assert_ne!(
            npc.motion.current_anim_id, 0,
            "walk should switch off idle clip"
        );
        npc.set_locomotion(&disc, Vec3::new(15.0, 0.0, -15.0), 12.0, Some((0.4, 0.1)));
        assert!(
            npc.motion.wheel.is_some(),
            "dash speed should engage 360 wheel"
        );
        let f_before = npc.motion.frame;
        npc.step_clock(0.2);
        assert_ne!(npc.motion.frame, f_before);
        assert!(npc.motion.core_ctrl.yaw > 0.05);
    }
}
