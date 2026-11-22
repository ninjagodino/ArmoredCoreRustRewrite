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

use acvd_data::generated::ac_unit::{AcAssemblyDesignSt, AC_ASSEMBLY_DESIGN_ST_FILES};
use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::enemy::{
    ENEMY_ENEMYBASICPARAM_BIN, ENEMY_ENEMYCOMMON_BIN, ENEMY_ENEMYGRAPHICSPARAM_BIN,
};
use acvd_data::generated::sfx::PARAM_SFXSESETPARAM_BIN;
use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_data::{find, part_field};
use acvd_formats::msb;
use acvd_formats::vfs::{self, Disc};
use acvd_render::Packs;
use anyhow::Result;
use bevy::audio::AudioSource;
use bevy::camera::visibility::NoFrustumCulling;
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
        let destroy_sfx = find(PARAM_SFXSESETPARAM_BIN, 502)
            .map_or(4041, |r| i32::from(r.data.sfx_id));
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
        self.spheres
            .first()
            .map_or(3.0, |s| s.offset_y)
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

    let mut packs = Packs::default();
    let mut missing = 0usize;
    let mut enemy_cache: HashMap<String, Option<Vec<(Handle<Mesh>, Handle<StandardMaterial>)>>> =
        HashMap::new();
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
        let entry = enemy_cache.entry(part.model.clone()).or_insert_with(|| {
            match acvd_render::model(disc, &asset) {
                Ok(list) => Some(
                    list.into_iter()
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
                        .collect(),
                ),
                Err(e) => {
                    warn!("target {} ({asset}): {e:#}", part.name);
                    None
                }
            }
        });
        let Some(list) = entry.as_ref().filter(|l| !l.is_empty()) else {
            continue;
        };
        let health = TargetHealth::from_enemy(&part.model);
        let enemy_ai = ai::EnemyAi::for_enemy(
            &part.model,
            xf.translation,
            ai::yaw_from_quat(xf.rotation),
            enemy_parts,
        );
        commands
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
            .with_children(|c| {
                for (mesh, mat) in list {
                    c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone())));
                }
            });
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
        warn!("target {} ({}): design {design_id} not found", part.name, part.model);
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
        match acvd_render::rigged_model(disc, p.model.path, &|root| assemble::place_of(&places, root))
        {
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
    let (_, skins) = pose::spawn_static(commands, root, &rig, &loaded);
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

/// Steps target stagger timers and handles destruction (explosion FFX/SFX + broken `_b.flv`
/// model swap for enemies, or despawn for destroyed AC targets).
#[allow(clippy::too_many_arguments)]
pub fn tick(
    time: Res<Time>,
    garage: Res<crate::Garage>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut cues: ResMut<sound::Cues>,
    mut audio_sources: ResMut<Assets<AudioSource>>,
    mut targets: Query<(Entity, &Target, &mut TargetHealth, &Transform, Option<&Children>)>,
) {
    let dt = time.delta_secs();
    let mut packs = Packs::default();
    let mut missing = 0usize;
    for (entity, target, mut health, xf, children) in &mut targets {
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
            sound::cue(&mut commands, &mut cues, &mut audio_sources, &health.destroy_se);
        }
        info!(
            "target {} ({}) destroyed at ({:.1}, {:.1}, {:.1})",
            target.name, target.model, center.x, center.y, center.z
        );

        let swapped = health.broken_asset.as_deref().is_some_and(|asset| {
            let Ok(list) = acvd_render::model(&garage.disc, asset) else {
                return false;
            };
            if list.is_empty() {
                return false;
            }
            if let Some(kids) = children {
                for child in kids.iter() {
                    commands.entity(child).try_despawn();
                }
            }
            commands.entity(entity).with_children(|c| {
                for m in list {
                    let mat = acvd_render::material(
                        &garage.disc,
                        &m,
                        &mut packs,
                        &mut images,
                        garage.flat,
                        &mut missing,
                    );
                    c.spawn((Mesh3d(meshes.add(m.mesh)), MeshMaterial3d(materials.add(mat))));
                }
            });
            true
        });
        if !swapped {
            commands.entity(entity).try_despawn();
        }
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
        let parts = msb::parts(&vfs::open(&disc, "model/map/m4000/m4000_actest.msb").unwrap())
            .unwrap();
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
                    acvd_render::model(&disc, &asset)
                        .is_ok_and(|list| !list.is_empty()),
                    "enemy FLVER failed to load: {asset}"
                );
                let broken = enemy_broken_asset(&part.model);
                assert!(
                    acvd_render::model(&disc, &broken)
                        .is_ok_and(|list| !list.is_empty()),
                    "broken enemy FLVER failed to load: {broken}"
                );
                let mut hp = TargetHealth::from_enemy(&part.model);
                assert!(hp.ap_max >= 500.0, "{} AP {}", part.model, hp.ap_max);
                let (res, lethal) = hp.apply_hit(damage::DamageType::Ke, 50000.0, 2000.0);
                assert!(res.effective && lethal && hp.destroyed && hp.just_destroyed);
            }
        }
    }
}
