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
use acvd_formats::msb;
use acvd_formats::vfs::{self, Disc};
use acvd_render::Packs;
use anyhow::Result;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;

use crate::{assemble, boost, map, paint, pose};

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
        commands
            .spawn((
                Target {
                    name: part.name.clone(),
                    model: part.model.clone(),
                    layer,
                    ac_design_id: None,
                },
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
    let root = commands
        .spawn((
            Target {
                name: part.name.clone(),
                model: part.model.clone(),
                layer: layer.to_string(),
                ac_design_id: Some(design_id),
            },
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
            } else {
                let asset = enemy_asset(&part.model);
                assert!(
                    acvd_render::model(&disc, &asset)
                        .is_ok_and(|list| !list.is_empty()),
                    "enemy FLVER failed to load: {asset}"
                );
            }
        }
    }
}
