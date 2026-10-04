//! AC assembly: resolves a design's parts through the generated catalogue and moves each part's
//! root bones onto its parent's socket, as `sheets/assembly_slots.csv` lays out.

use acvd_data::{ac_part, Joint, ModelRef, Slot, SocketRef};

/// One model of the assembled AC. `offsets` moves the vertices under each listed root bone, in
/// FLVER space; vertices under any other root follow the first entry.
pub struct Placement {
    pub column: &'static str,
    pub category: u8,
    pub part: u16,
    pub model: &'static ModelRef,
    pub offsets: Vec<(&'static str, [f32; 3])>,
    /// `(root bone, parent placement, socket on the parent's model)` for every root a slot
    /// with a parent moved.
    pub attach: Vec<(&'static str, usize, SocketRef)>,
}

impl Placement {
    pub fn offset(&self, root: Option<&str>) -> [f32; 3] {
        root.and_then(|r| self.offsets.iter().find(|(n, _)| *n == r)).or(self.offsets.first()).map_or([0.0; 3], |(_, o)| *o)
    }
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
        let Some(part) = u16::try_from(id).ok().and_then(|id| ac_part(s.category, id)) else {
            problem("not in the parts catalogue".into());
            continue;
        };
        let Some(model) = part.model(s.prefix) else {
            problem(format!("no {} model {:04}", s.prefix, part.model_id));
            continue;
        };
        let roots: Vec<&Joint> = if s.roots.is_empty() { model.roots.iter().collect() } else { s.roots.iter().filter_map(|r| model.root(r)).collect() };
        if roots.is_empty() || roots.len() < s.roots.len() {
            problem(format!("{} lacks a root bone of {:?}", model.path, s.roots));
            continue;
        }
        let mut centroid = [0f32; 3];
        for r in &roots {
            (0..3).for_each(|k| centroid[k] += r.origin[k] / roots.len() as f32);
        }
        let mut parent_index = None;
        let target = match s.parent {
            None => centroid,
            Some(parent) => {
                let Some(&(_, pi)) = placed.iter().find(|(n, _)| *n == parent) else {
                    problem(format!("parent slot {parent} is empty"));
                    continue;
                };
                parent_index = Some(pi);
                let p = &out.placements[pi];
                let found = match s.socket {
                    SocketRef::Dummy(d) => p.model.socket(d).map(|x| (x.position, x.root)),
                    SocketRef::Bone(b) => p.model.root(b).map(|j| (j.origin, j.name)),
                    SocketRef::None => None,
                };
                let Some((at, carrier)) = found else {
                    problem(format!("{} has no socket {:?}", p.model.path, s.socket));
                    continue;
                };
                let o = p.offset(Some(carrier));
                [at[0] + o[0], at[1] + o[1], at[2] + o[2]]
            }
        };
        let offset = [target[0] - centroid[0], target[1] - centroid[1], target[2] - centroid[2]];
        let moved = roots.iter().map(|r| (r.name, offset));
        let attach = parent_index.into_iter().flat_map(|pi| roots.iter().map(move |r| (r.name, pi, s.socket)));
        let index = match out.placements.iter().position(|p| p.column == s.column) {
            Some(i) => {
                out.placements[i].offsets.extend(moved);
                out.placements[i].attach.extend(attach);
                i
            }
            None => {
                out.placements.push(Placement { column: s.column, category: s.category, part: part.id, model, offsets: moved.collect(), attach: attach.collect() });
                out.placements.len() - 1
            }
        };
        placed.push((s.name, index));
    }
    out
}
