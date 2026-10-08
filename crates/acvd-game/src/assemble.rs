//! AC assembly: resolves a design's parts through the generated catalogue and moves each part's
//! root bones onto its parent's socket, as `sheets/assembly_slots.csv` lays out.
//!
//! Facing comes from `param/acattachinfo.bin`. The flag byte (record +3) is switched at 360
//! `0x8288cea0`: flags 0/1 build a basis on the socket forward against world +Y (`0x8288c0b8`),
//! flags 2/3 against world +Z (`0x8288bef8`). Flag 4 (a hanger weapon on its rack) builds the
//! same kind of basis through `0x8288a058`: local -Z, the barrel, lies along the socket
//! forward, and world +Y fills the remaining axis (`0x827ce358`). Flag 5 negates that forward
//! and is not applied to shoulder weapons.

use acvd_data::{ac_part, Joint, ModelRef, Slot, SocketRef};
use acvd_render::{RootPlace, SocketFrame};

/// Fixed hanger racks. They are not `acvparts` rows: acattachinfo child categories 18 and 17
/// name these two models (360 `0x82c080f0` loads both).
const RACK_L: &str = "model/ac/parts/hanger/hgl0001/hgl0001_m.bnd.dcx|hgl0001.flv";
const RACK_R: &str = "model/ac/parts/hanger/hgr0001/hgr0001_m.bnd.dcx|hgr0001.flv";

/// Front booster sockets. Cores without them (31 of 72, e.g. cr0230) get no attach for
/// `l2_boost` / `r2_boost`, and `0x8288db60` then switches that bone's meshes off (category 6
/// only: model vtable `0x82044bec` slot 8 = `0x8258aed8` → `0x82c92688` clears the per-mesh
/// draw bit, children included).
const SOCKET_L2: u8 = 25;
const SOCKET_R2: u8 = 26;
/// Front socket of each rack, where the hanger weapon's root lands.
const SOCKET_RACK_L: u8 = 85;
const SOCKET_RACK_R: u8 = 87;

/// One root bone moved onto a parent's socket.
pub struct Mount {
    pub root: &'static str,
    pub origin: [f32; 3],
    pub parent: usize,
    pub socket: SocketRef,
    /// Attach-info flag. `None` is a pure translation. `Some(4)` copies the parent's rotation.
    pub orient: Option<u8>,
    /// When `socket` is missing on the parent, copy this earlier root's place.
    pub follow: Option<&'static str>,
}

/// One model of the assembled AC.
pub struct Placement {
    pub column: &'static str,
    pub category: u8,
    pub part: u16,
    pub model: &'static ModelRef,
    pub mounts: Vec<Mount>,
    /// Root bones the parent has no socket for; they and their children are not drawn.
    pub hidden: Vec<&'static str>,
}

/// A parent placement after its model has loaded: `places` are its root poses, `frames` its
/// sockets in model space.
#[derive(Clone)]
pub struct Stored {
    pub places: Vec<(&'static str, RootPlace)>,
    pub frames: Vec<SocketFrame>,
}

#[derive(Default)]
pub struct Assembly {
    pub placements: Vec<Placement>,
    /// Slots the design fills that could not be placed.
    pub problems: Vec<String>,
}

pub fn assemble<T>(slots: &[Slot<T>], design: &T) -> Assembly {
    let mut out = Assembly::default();
    let mut placed: Vec<(&str, usize)> = Vec::new();
    for s in slots {
        let id = (s.part)(design);
        if id <= 0 {
            continue;
        }
        let mut problem = |what: String| out.problems.push(format!("{} {id}: {what}", s.name));
        let Some(part) = u16::try_from(id)
            .ok()
            .and_then(|id| ac_part(s.category, id))
        else {
            problem("not in the parts catalogue".into());
            continue;
        };
        let Some(model) = part.model(s.prefix) else {
            problem(format!("no {} model {:04}", s.prefix, part.model_id));
            continue;
        };
        let roots: Vec<&Joint> = if s.roots.is_empty() {
            model.roots.iter().collect()
        } else {
            s.roots.iter().filter_map(|r| model.root(r)).collect()
        };
        if roots.is_empty() || roots.len() < s.roots.len() {
            problem(format!("{} lacks a root bone of {:?}", model.path, s.roots));
            continue;
        }
        let Some(parent_name) = s.parent else {
            placed.push((s.name, out.placements.len()));
            out.placements.push(Placement {
                column: s.column,
                category: s.category,
                part: part.id,
                model,
                mounts: Vec::new(),
                hidden: Vec::new(),
            });
            continue;
        };
        let Some(&(_, parent_index)) = placed.iter().find(|(n, _)| *n == parent_name) else {
            problem(format!("parent slot {parent_name} is empty"));
            continue;
        };
        if !socket_on(&out.placements[parent_index], s.socket) {
            problem(format!(
                "{} has no socket {:?}",
                out.placements[parent_index].model.path, s.socket
            ));
            continue;
        }

        // Hanger weapons sit on a rack, and the rack sits on the arm socket this slot names.
        let (parent_index, socket, orient) = if s.name == "hanger_l" || s.name == "hanger_r" {
            let left = s.name == "hanger_l";
            let Some(rack) = acvd_data::model(if left { RACK_L } else { RACK_R }) else {
                problem("hanger rack model is not in the catalogue".into());
                continue;
            };
            let rack_root = if left { "80" } else { "81" };
            let Some(joint) = rack.root(rack_root) else {
                problem(format!("{} has no root {rack_root}", rack.path));
                continue;
            };
            let rack_index = out.placements.len();
            out.placements.push(Placement {
                column: if left { "rack_l" } else { "rack_r" },
                category: if left { 18 } else { 17 },
                part: 0,
                model: rack,
                mounts: vec![Mount {
                    root: joint.name,
                    origin: joint.origin,
                    parent: parent_index,
                    socket: s.socket,
                    orient: s.orient,
                    follow: None,
                }],
                hidden: Vec::new(),
            });
            placed.push((if left { "rack_l" } else { "rack_r" }, rack_index));
            (
                rack_index,
                SocketRef::Dummy(if left { SOCKET_RACK_L } else { SOCKET_RACK_R }),
                Some(4),
            )
        } else {
            (parent_index, s.socket, s.orient)
        };

        let mut mounts = Vec::new();
        let mut hidden = Vec::new();
        for (i, r) in roots.iter().enumerate() {
            let (socket, follow) = match (i, r.name) {
                (0, _) => (socket, None),
                (_, "l2_boost") => (SocketRef::Dummy(SOCKET_L2), None),
                (_, "r2_boost") => (SocketRef::Dummy(SOCKET_R2), None),
                _ => (socket, Some(roots[0].name)),
            };
            if i > 0 && follow.is_none() && !socket_on(&out.placements[parent_index], socket) {
                hidden.push(r.name);
                continue;
            }
            mounts.push(Mount {
                root: r.name,
                origin: r.origin,
                parent: parent_index,
                socket,
                orient,
                follow,
            });
        }
        let index = match out.placements.iter().position(|p| p.column == s.column) {
            Some(i) => {
                out.placements[i].mounts.extend(mounts);
                out.placements[i].hidden.extend(hidden);
                i
            }
            None => {
                out.placements.push(Placement {
                    column: s.column,
                    category: s.category,
                    part: part.id,
                    model,
                    mounts,
                    hidden,
                });
                out.placements.len() - 1
            }
        };
        placed.push((s.name, index));
    }
    out
}

fn socket_on(parent: &Placement, socket: SocketRef) -> bool {
    match socket {
        SocketRef::Dummy(id) => parent.model.socket(id).is_some(),
        SocketRef::Bone(name) => parent.model.root(name).is_some(),
        SocketRef::None => false,
    }
}

/// The place of `root` on `placement`, from parents that have already loaded. A root with no
/// mount stays where the model authored it.
pub fn root_places(
    placement: &Placement,
    parents: &[Placement],
    known: &[Option<Stored>],
) -> Vec<(&'static str, RootPlace)> {
    let mut out = Vec::new();
    for mount in &placement.mounts {
        let place = match known.get(mount.parent).and_then(|k| k.as_ref()) {
            Some(parent) => mount_place(mount, &parents[mount.parent], parent, &out),
            None => RootPlace::IDENTITY,
        };
        out.push((mount.root, place));
    }
    out
}

/// `place(None)` follows the first mounted root, so unboned vertices move with the part.
pub fn place_of(places: &[(&str, RootPlace)], root: Option<&str>) -> RootPlace {
    match root {
        Some(name) => places.iter().find(|(n, _)| *n == name).map(|(_, p)| *p),
        None => places.first().map(|(_, p)| *p),
    }
    .unwrap_or(RootPlace::IDENTITY)
}

fn mount_place(
    mount: &Mount,
    parent_model: &Placement,
    parent: &Stored,
    earlier: &[(&str, RootPlace)],
) -> RootPlace {
    let follow = || {
        earlier
            .iter()
            .rev()
            .find(|(n, _)| Some(*n) == mount.follow)
            .map(|(_, p)| *p)
    };
    let (at, forward, carrier) = match mount.socket {
        SocketRef::Dummy(id) => match parent.frames.iter().find(|f| f.id == id) {
            Some(f) => (f.position, f.forward, placed(&parent.places, &f.root)),
            None => match parent_model.model.socket(id) {
                Some(s) => (s.position, [0.0, 1.0, 0.0], placed(&parent.places, s.root)),
                None => return follow().unwrap_or(RootPlace::IDENTITY),
            },
        },
        SocketRef::Bone(name) => {
            let origin = parent_model
                .model
                .root(name)
                .map(|j| j.origin)
                .unwrap_or([0.0; 3]);
            (origin, [0.0, 1.0, 0.0], placed(&parent.places, name))
        }
        SocketRef::None => return RootPlace::IDENTITY,
    };
    let position = carrier.apply_point(at);
    let facing = carrier.apply_dir(forward);
    let columns = match mount.orient {
        Some(flag) => orient_basis(flag, facing),
        None => [
            RootPlace::IDENTITY.x,
            RootPlace::IDENTITY.y,
            RootPlace::IDENTITY.z,
        ],
    };
    let rotated = RootPlace {
        x: columns[0],
        y: columns[1],
        z: columns[2],
        t: [0.0; 3],
    }
    .apply_dir(mount.origin);
    RootPlace {
        x: columns[0],
        y: columns[1],
        z: columns[2],
        t: [
            position[0] - rotated[0],
            position[1] - rotated[1],
            position[2] - rotated[2],
        ],
    }
}

fn placed(places: &[(&str, RootPlace)], name: &str) -> RootPlace {
    places
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| *p)
        .unwrap_or(RootPlace::IDENTITY)
}

/// Columns of the attach basis for `flag`, from the socket's model-space `forward`.
///
/// Flags 0 and 1 (`0x8288c0b8`): local X is the forward, negated again inside that function, so
/// flag 1 keeps it and flag 0 flips it. Local Y is `normalize(-Fx Fy, 1-Fy², -Fy Fz)` of the
/// original forward. Flags 2 and 3 (`0x8288bef8`): local Y is the forward for flag 2 and its
/// opposite for flag 3, so a booster's nozzle (local -Y) lies along the socket forward.
/// Flag 4 (`0x8288a058` → `0x827ce358`): local Z is the negated forward, so a weapon's barrel
/// (local -Z, the muzzle dummy's forward) lies along the socket. World +Y is the hint; when
/// the forward is parallel to it the hint is turned ±90° about X (`0x82d4ee50`, +90° at
/// `0x820146d0`, −90° at `0x82016ec0`).
pub fn orient_basis(flag: u8, forward: [f32; 3]) -> [[f32; 3]; 3] {
    let f = unit(forward);
    match flag {
        4 => rack_basis(f),
        0 | 1 => {
            let x = if flag & 1 == 1 { f } else { neg(f) };
            let y = if f[1].abs() >= 1.0 - 1.0e-4 {
                [0.0, 0.0, 1.0]
            } else {
                unit([-f[0] * f[1], 1.0 - f[1] * f[1], -f[1] * f[2]])
            };
            [x, y, unit(cross(x, y))]
        }
        2 | 3 => {
            let y = if flag == 3 { neg(f) } else { f };
            let z = if y[2].abs() >= 1.0 - 1.0e-4 {
                [0.0, 1.0, 0.0]
            } else {
                unit([-y[0] * y[2], -y[1] * y[2], 1.0 - y[2] * y[2]])
            };
            [unit(cross(y, z)), y, z]
        }
        _ => orient_basis(2, f),
    }
}

/// Flag 4 columns: local X, Y, Z. Local -Z points along `forward`.
fn rack_basis(forward: [f32; 3]) -> [[f32; 3]; 3] {
    let n = neg(forward);
    let hint = [0.0, 1.0, 0.0];
    let along = dot(n, hint);
    let up = if along.abs() >= 1.0 - 1.0e-4 {
        let angle = if along >= 1.0 {
            90.0_f32.to_radians()
        } else {
            -90.0_f32.to_radians()
        };
        rot_x(hint, angle)
    } else {
        unit([
            hint[0] - n[0] * along,
            hint[1] - n[1] * along,
            hint[2] - n[2] * along,
        ])
    };
    [unit(cross(up, n)), up, n]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn rot_x(v: [f32; 3], angle: f32) -> [f32; 3] {
    let (s, c) = angle.sin_cos();
    [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c]
}

fn unit(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1.0e-8 {
        [0.0, 1.0, 0.0]
    } else {
        v.map(|c| c / l)
    }
}

fn neg(v: [f32; 3]) -> [f32; 3] {
    v.map(|c| -c)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) {
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() < 1.0e-4, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn front_boosters_hide_on_cores_without_sockets_25_26() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
        let (mut hidden, mut mounted) = (0, 0);
        for row in AC_ASSEMBLY_DESIGN_ST_FILES.iter().flat_map(|(_, r)| r.iter()) {
            let built = assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &row.data);
            let (Some(core), Some(booster)) = (
                built.placements.iter().find(|p| p.column == "core"),
                built.placements.iter().find(|p| p.column == "booster"),
            ) else {
                continue;
            };
            let fronts = core.model.socket(SOCKET_L2).is_some();
            let has = |root| booster.mounts.iter().any(|m| m.root == root);
            for root in ["l2_boost", "r2_boost"] {
                assert_eq!(has(root), fronts, "design {} {root}", row.id);
                assert_eq!(booster.hidden.contains(&root), !fronts, "design {} {root}", row.id);
            }
            if fronts {
                mounted += 1;
            } else {
                hidden += 1;
                println!("design {} core {} hides its front boosters", row.id, core.model.path);
            }
        }
        assert!(hidden > 0 && mounted > 0, "hidden {hidden} mounted {mounted}");
    }

    #[test]
    fn arm_flags_leave_an_outward_socket_unrotated() {
        let left = orient_basis(1, [1.0, 0.0, 0.0]);
        let right = orient_basis(0, [-1.0, 0.0, 0.0]);
        for basis in [left, right] {
            close(basis[0], [1.0, 0.0, 0.0]);
            close(basis[1], [0.0, 1.0, 0.0]);
            close(basis[2], [0.0, 0.0, 1.0]);
        }
    }

    #[test]
    fn flag1_uses_the_socket_forward_as_local_x() {
        let f = [0.9925, 0.0, 0.1219];
        let basis = orient_basis(1, f);
        close(basis[0], unit(f));
    }

    #[test]
    fn flag2_up_is_identity_and_flag3_aims_the_nozzle_along_the_socket() {
        let up = orient_basis(2, [0.0, 1.0, 0.0]);
        close(up[0], [1.0, 0.0, 0.0]);
        close(up[1], [0.0, 1.0, 0.0]);
        close(up[2], [0.0, 0.0, 1.0]);

        let f = [0.0, -0.9063079, 0.42261815];
        let basis = orient_basis(3, f);
        close(basis[1], unit(neg(f)));
        let nozzle = neg(basis[1]);
        let n = unit(f);
        close(nozzle, n);
        close(basis[0], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn flag4_aims_the_barrel_along_the_socket() {
        // Rack dummies 85/87 point along +Y. The barrel is the weapon's local -Z.
        let up = orient_basis(4, [0.0, 1.0, 0.0]);
        close(neg(up[2]), [0.0, 1.0, 0.0]);
        close(up[0], [-1.0, 0.0, 0.0]);
        close(up[1], [0.0, 0.0, -1.0]);

        let ahead = orient_basis(4, [0.0, 0.0, 1.0]);
        close(neg(ahead[2]), [0.0, 0.0, 1.0]);
        close(ahead[1], [0.0, 1.0, 0.0]);
    }
}
