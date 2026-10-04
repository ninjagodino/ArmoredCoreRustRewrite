//! Game data generated from the sheets by `acvd-sheets gen`, one strut per sheet row.
//! `src/generated` is derived from the owned disc and is never committed.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row<T: 'static> {
    pub id: u32,
    pub name: &'static str,
    pub data: T,
}

/// First row with `id`. Rows keep disc order, and some retail files repeat ids.
pub fn find<T>(rows: &'static [Row<T>], id: u32) -> Option<&'static Row<T>> {
    rows.iter().find(|r| r.id == id)
}

/// One binder entry on the disc. `binder` is an asset path (nested binders join names with `|`)
/// and `entry` addresses the entry inside it: its name, or `#index` when the binder repeats the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    pub binder: &'static str,
    pub entry: &'static str,
    pub name: &'static str,
    pub id: Option<i32>,
    /// Size after removing zlib and DCX layers.
    pub size: usize,
    pub ext: &'static str,
}

impl Asset {
    /// Full asset path, as accepted by `acvd_formats::vfs::open`.
    pub fn path(&self) -> String {
        format!("{}|{}", self.binder, self.entry)
    }
}

/// A TPF format code and the 4x4 block format it holds (`bc1`, `bc3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureFormat {
    pub code: u8,
    pub name: &'static str,
    pub block_bytes: u32,
}

/// One texture inside a TPF. `pack` is the TPF's asset path; `index` is its position in the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureRef {
    pub pack: &'static str,
    pub index: usize,
    pub name: &'static str,
    pub format: u8,
    pub width: u16,
    pub height: u16,
    pub levels: u32,
    pub faces: u32,
    pub size: u32,
}

impl TextureRef {
    pub fn format(&self) -> Option<&'static TextureFormat> {
        generated::textures::TEXTURE_FORMATS.iter().find(|f| f.code == self.format)
    }
}

/// One FLVER model. `path` is its asset path; `triangles` counts each mesh's main face set
/// (highest detail, no motion blur copy); `textures` are the texture names its materials use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRef {
    pub path: &'static str,
    pub version: u32,
    pub meshes: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub textures: &'static [&'static str],
    /// Root bones, in bone order.
    pub roots: &'static [Joint],
    /// Dummies carrying an attach socket id, in dummy order.
    pub sockets: &'static [Socket],
}

impl ModelRef {
    pub fn root(&self, name: &str) -> Option<&'static Joint> {
        self.roots.iter().find(|j| j.name == name)
    }

    /// First dummy with socket `id`: cr03xx/cr05xx repeat every socket on a displaced copy.
    pub fn socket(&self, id: u8) -> Option<&'static Socket> {
        self.sockets.iter().find(|s| s.id == id)
    }
}

/// A bone origin in model space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Joint {
    pub name: &'static str,
    pub origin: [f32; 3],
}

/// A dummy point in model space; `root` is the root bone above the bone carrying it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Socket {
    pub id: u8,
    pub root: &'static str,
    pub position: [f32; 3],
}

/// One `param/acvparts.bin` record, the models (`(prefix, asset path)`) its model id names, and
/// its category's `sheets/ac_part_fields.csv` fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcPart {
    pub category: u8,
    pub id: u16,
    pub model_id: u16,
    pub models: &'static [(&'static str, &'static str)],
    pub fields: &'static [(&'static str, f64)],
}

impl AcPart {
    pub fn model(&self, prefix: &str) -> Option<&'static ModelRef> {
        let (_, path) = self.models.iter().find(|(p, _)| *p == prefix)?;
        model(path)
    }

    pub fn field(&self, name: &str) -> Option<f64> {
        self.fields.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }
}

/// The catalogue record for part `id` in `category`.
pub fn ac_part(category: u8, id: u16) -> Option<&'static AcPart> {
    generated::assembly::AC_PARTS.iter().find(|p| p.category == category && p.id == id)
}

/// Field `name` of part `id` in `category`; 0 for an empty slot (id 0 or below), as the
/// game's part parameter getter returns for one.
pub fn part_field(id: i64, category: u8, name: &str) -> f32 {
    u16::try_from(id).ok().filter(|&id| id > 0).and_then(|id| ac_part(category, id)).and_then(|p| p.field(name)).unwrap_or(0.0) as f32
}

/// Where a slot's root bone lands on its parent's model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SocketRef {
    None,
    Dummy(u8),
    Bone(&'static str),
}

/// One assembly column of `T` (`sheets/assembly_slots.csv`): the part it names (`part`, 0 or
/// below for none) is drawn by the `prefix` model, the centroid of whose `roots` lands on
/// `socket` of the `parent` slot's model. Empty `roots` means every root of the model.
#[derive(Debug, Clone, Copy)]
pub struct Slot<T: 'static> {
    pub name: &'static str,
    pub column: &'static str,
    pub part: fn(&T) -> i64,
    pub category: u8,
    pub prefix: &'static str,
    pub parent: Option<&'static str>,
    pub socket: SocketRef,
    pub roots: &'static [&'static str],
}

/// The motion set part `id` of `category` selects (`sheets/ac_motion.csv`): `set` is the
/// binder's asset path and `skeleton` the entry of its clip carrying the rest skeleton. Clips
/// are authored on one body per set; retarget their rotations onto the part's own bones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcMotion {
    pub category: u8,
    pub id: u16,
    pub set: &'static str,
    pub skeleton: &'static str,
}

pub fn ac_motion(category: u8, id: u16) -> Option<&'static AcMotion> {
    generated::assembly::AC_MOTIONS.iter().find(|m| m.category == category && m.id == id)
}

/// A locomotion state (`sheets/ac_states.csv`) and the `param/acmotion.bin` row that plays it.
/// Directional states have one row per `direction`: 0 forward, then clockwise in eighths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcState {
    pub state: &'static str,
    pub direction: Option<u8>,
    pub row: u32,
}

pub fn ac_state(state: &str, direction: Option<u8>) -> Option<&'static AcState> {
    generated::assembly::AC_STATES.iter().find(|s| s.state == state && s.direction == direction)
}

/// A tuning binary `system/paramlist.xml` lists, with its debug-menu labels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TuningFile {
    pub name: &'static str,
    pub desc: &'static str,
    pub bin: &'static str,
    pub fields: &'static [TuningField],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TuningField {
    pub label: &'static str,
    pub format: &'static str,
    pub value: f64,
}

/// The model at asset path `path`.
pub fn model(path: &str) -> Option<&'static ModelRef> {
    generated::models::ALL.iter().flat_map(|g| g.iter()).find(|m| m.path == path)
}

/// Every model whose entry name (after the last `|`) equals `name`, ASCII case-insensitive.
pub fn models_named(name: &str) -> impl Iterator<Item = &'static ModelRef> + '_ {
    generated::models::ALL.iter().flat_map(|g| g.iter()).filter(move |m| m.path.rsplit('|').next().is_some_and(|n| n.eq_ignore_ascii_case(name)))
}

/// Every texture whose name equals `name` (ASCII case-insensitive), in disc order.
pub fn textures_named(name: &str) -> impl Iterator<Item = &'static TextureRef> + '_ {
    generated::textures::ALL.iter().flat_map(|g| g.iter()).filter(move |t| t.name.eq_ignore_ascii_case(name))
}

/// Every binder entry whose name equals `name` (ASCII case-insensitive), in disc order.
pub fn assets_named(name: &str) -> impl Iterator<Item = &'static Asset> + '_ {
    generated::assets::ALL.iter().flat_map(|g| g.iter()).filter(move |a| a.name.eq_ignore_ascii_case(name))
}

pub mod generated;

#[cfg(test)]
mod tests {
    use crate::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
    use crate::generated::ctrl::AcCtrlParam;

    /// Every preset whose head, core, legs and booster are catalogued moves at the floors
    /// `AcCtrlParamCalc` clamps to or above, and heavier builds on the same legs and head (whose
    /// balance sets the stability) never walk faster.
    #[test]
    fn preset_movement() {
        let mut seen = 0;
        let mut by_legs: Vec<((i64, i64), f32, f32)> = Vec::new();
        for r in AC_ASSEMBLY_DESIGN_ST_FILES.iter().flat_map(|(_, rows)| rows.iter()) {
            let legs = r.data.legs as i64;
            let parts = [(0, r.data.head as i64), (1, r.data.core as i64), (3, legs), (6, r.data.booster as i64)];
            if parts.iter().any(|&(category, id)| part_known(category, id).is_none()) {
                continue;
            }
            let c = AcCtrlParam::calculate(&r.data);
            seen += 1;
            assert!(c.walk_speed >= 15.0 && c.boost_max_speed >= 20.0 && c.turn_speed >= 10.0, "{} {c:?}", r.id);
            assert!(c.walk_acc_tick > 0.0 && c.boost_acc_tick > 0.0 && c.jump_rise_tick > 0.0, "{} {c:?}", r.id);
            by_legs.push(((legs, r.data.head as i64), c.leg_capability, c.walk_speed));
            if seen <= 6 {
                println!(
                    "{} {}: weight {} (legs carry {} / {}), walk {:.0} km/h, boost {:.0} km/h, turn {:.0} deg/s, jump {:.2} m/tick x{} ticks",
                    r.id,
                    r.name,
                    c.total_weight,
                    c.leg_capability,
                    c.leg_capability / c.weight_ratio,
                    c.walk_speed,
                    c.boost_max_speed,
                    c.turn_speed,
                    c.jump_rise_tick,
                    c.jump_frames
                );
            }
        }
        assert!(seen > 0);
        for a in &by_legs {
            for b in by_legs.iter().filter(|b| b.0 == a.0 && b.1 > a.1) {
                assert!(b.2 <= a.2 + 1e-3, "legs and head {:?}: {a:?} vs {b:?}", a.0);
            }
        }
    }

    fn part_known(category: u8, id: i64) -> Option<&'static crate::AcPart> {
        crate::ac_part(category, u16::try_from(id).ok()?)
    }

    /// Category-10 firing fields match the grow-table org values used as evidence
    /// (`sheets/ac_part_fields.csv`): machine gun 1010 and EN machine gun 810.
    #[test]
    fn hand_weapon_fields() {
        use crate::generated::ac_unit::PARAM_GROWPARTSARMUNITPARAM_BIN;
        use crate::generated::bullet::{BULLET_BULLETENERGY_BIN, BULLET_BULLETRIGID_BIN};
        let mg = crate::ac_part(10, 1010).expect("part 1010");
        let g = crate::find(PARAM_GROWPARTSARMUNITPARAM_BIN, 1010).expect("grow 1010");
        assert_eq!(mg.field("magazine"), Some(520.0));
        assert_eq!(mg.field("reload_time"), Some(g.data.reload_time_org as f64));
        assert_eq!(mg.field("init_speed"), Some(g.data.init_speed_org as f64));
        assert_eq!(mg.field("damage_power"), Some(g.data.damage_power_org as f64));
        assert_eq!(mg.field("bullet_id"), Some(10003.0));
        assert!(crate::find(BULLET_BULLETRIGID_BIN, 10003).is_some());
        let en = crate::ac_part(10, 810).expect("part 810");
        let ge = crate::find(PARAM_GROWPARTSARMUNITPARAM_BIN, 810).expect("grow 810");
        assert_eq!(en.field("reload_time"), Some(ge.data.reload_time_org as f64));
        assert_eq!(en.field("en_drain"), Some(ge.data.en_drain_org as f64));
        assert_eq!(en.field("bullet_id"), Some(10006.0));
        assert!(crate::find(BULLET_BULLETENERGY_BIN, 10006).is_some());
    }
}
