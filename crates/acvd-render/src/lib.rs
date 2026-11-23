//! Disc -> Bevy assets: FLVER meshes and TPF block-compressed textures, shared by the viewer
//! and the game.
//!
//! FLVER triangles wind clockwise around their normals in a left-handed frame; mirroring X
//! turns them into Bevy's right-handed, counter-clockwise front faces without touching indices.

pub mod app;
pub mod menu;
pub mod text;
pub mod ui_kit;

use std::collections::HashMap;

use acvd_data::TextureRef;
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{flver, tpf};
use anyhow::{bail, ensure, Context, Result};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::color::Color;
use bevy::pbr::StandardMaterial;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

pub struct LoadedMesh {
    pub mesh: Mesh,
    pub diffuse: Option<String>,
    pub diffuse2: Option<String>,
    pub bump: Option<String>,
    pub bump2: Option<String>,
    pub specular: Option<String>,
    pub emission: Option<String>,
    /// FLVER material MTD name (e.g. `ACParts`, `ACParts_Glow`, `ACParts_Rim_ex`, `Map_Diffuse_Multi`, `Water`).
    pub mtd: String,
    /// Root bone above the first boned vertex of this mesh (`None` when unboned).
    pub root: Option<String>,
    /// Bounds of the vertices the main face set uses, already mirrored.
    pub min: [f32; 3],
    pub max: [f32; 3],
}

const DIFFUSE_TYPES: &[&str] = &["g_DiffuseTexture", "g_Diffuse"];
const DIFFUSE2_TYPES: &[&str] = &["g_DiffuseTexture2", "g_Diffuse2"];
const BUMP_TYPES: &[&str] = &["g_BumpmapTexture", "g_Bumpmap"];
const BUMP2_TYPES: &[&str] = &["g_BumpmapTexture2", "g_Bumpmap2"];
const SPECULAR_TYPES: &[&str] = &["g_SpecularTexture", "g_Specular"];
const EMISSION_TYPES: &[&str] = &["g_EmissionTexture", "g_Emission"];

fn mirror(v: [f32; 3]) -> [f32; 3] {
    [-v[0], v[1], v[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [1.0, 0.0, 0.0]
    }
}

/// Converts a FLVER `(normal, stored_tangent)` pair in model space into Bevy's X-mirrored
/// `ATTRIBUTE_TANGENT` (`[Tx, Ty, Tz, w]`).
///
/// In ACVD FLVERs (`Flver_ColDifBump.vpo` / `.fpo`), semantic 6 (`MemberKind::TangentS8`) stores
/// the bitangent `+dP/dv` with handedness `w = ±1`, and `cross(N, v_stored) * w` is the tangent
/// `+dP/du`. Under X-mirroring `S = diag(-1, 1, 1)` (`det(S) = -1`):
/// `T_bevy = S * (cross(N, v_stored) * w)`, and `w * cross(S * N, T_bevy) = S * v_stored`,
/// matching Bevy's `apply_normal_mapping` with `flip_normal_map_y: false`.
fn flver_tangent_to_bevy(n: [f32; 3], stored: [f32; 4]) -> [f32; 4] {
    let w = if stored[3] < 0.0 { -1.0 } else { 1.0 };
    let b = [stored[0], stored[1], stored[2]];
    let c = cross(n, b);
    let t_flver = normalize3([c[0] * w, c[1] * w, c[2] * w]);
    let t_bevy = mirror(t_flver);
    [t_bevy[0], t_bevy[1], t_bevy[2], w]
}

pub fn texture_stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.split('.').next().unwrap_or(file).to_owned()
}

pub fn model(disc: &Disc, asset: &str) -> Result<Vec<LoadedMesh>> {
    model_with_offsets(disc, asset, &|_| [0.0; 3])
}

/// Like [`model`], moving every vertex by `offset(root)` once it is in model space, in FLVER
/// axes: `root` names the root bone above the vertex's bone, `None` when it has no bone.
pub fn model_with_offsets(disc: &Disc, asset: &str, offset: &dyn Fn(Option<&str>) -> [f32; 3]) -> Result<Vec<LoadedMesh>> {
    Ok(load(disc, asset, &|root| RootPlace::translate(offset(root)), false)?.0)
}

/// Where one root bone of a part sits on the assembled AC, in FLVER axes. Columns are the
/// images of the model's local axes; `IDENTITY` leaves the model where it was authored.
#[derive(Debug, Clone, Copy)]
pub struct RootPlace {
    pub x: [f32; 3],
    pub y: [f32; 3],
    pub z: [f32; 3],
    pub t: [f32; 3],
}

impl RootPlace {
    pub const IDENTITY: Self = Self { x: [1.0, 0.0, 0.0], y: [0.0, 1.0, 0.0], z: [0.0, 0.0, 1.0], t: [0.0; 3] };

    pub fn translate(t: [f32; 3]) -> Self {
        Self { t, ..Self::IDENTITY }
    }

    pub fn apply_point(&self, p: [f32; 3]) -> [f32; 3] {
        let d = self.apply_dir(p);
        [d[0] + self.t[0], d[1] + self.t[1], d[2] + self.t[2]]
    }

    pub fn apply_dir(&self, p: [f32; 3]) -> [f32; 3] {
        [
            self.x[0] * p[0] + self.y[0] * p[1] + self.z[0] * p[2],
            self.x[1] * p[0] + self.y[1] * p[1] + self.z[1] * p[2],
            self.x[2] * p[0] + self.y[2] * p[1] + self.z[2] * p[2],
        ]
    }

    /// `self * inner` once `inner` is an [`flver::Xform`]: the model's bind, then this place.
    pub fn then_bind(&self, bind: &flver::Xform) -> flver::Xform {
        flver::Xform {
            m: [
                [self.x[0], self.y[0], self.z[0]],
                [self.x[1], self.y[1], self.z[1]],
                [self.x[2], self.y[2], self.z[2]],
            ],
            t: self.t,
        }
        .then(bind)
    }
}

/// One FLVER bone at rest, in FLVER axes. `bind` is its model-space transform with its root's
/// [`RootPlace`] applied (`place`, then the bind).
pub struct RigBone {
    pub name: String,
    pub parent: Option<usize>,
    pub bind: flver::Xform,
}

/// The skeleton a [`rigged_model`]'s meshes are skinned to: joint `i < bones.len()` is bone `i`;
/// joint `bones.len()` carries vertices without a bone and rests at `unboned`.
pub struct Rig {
    pub bones: Vec<RigBone>,
    pub unboned: [f32; 3],
    /// `(socket id, bone carrying the dummy)` for every dummy with a socket id.
    pub sockets: Vec<(u8, Option<usize>)>,
    /// Attach sockets in model space, before this part's own [`RootPlace`]. `forward` is the
    /// dummy's forward through its bone; `root` is the root bone above that bone.
    pub frames: Vec<SocketFrame>,
    /// Every dummy with an effect point id (colour byte 1).
    pub effects: Vec<RigEffect>,
}

/// One attach socket of a loaded model, in that model's space.
#[derive(Clone)]
pub struct SocketFrame {
    pub id: u8,
    pub root: String,
    pub position: [f32; 3],
    pub forward: [f32; 3],
}

/// An FFX effect point, in FLVER axes: `position` relative to `bone` and `forward` in its frame.
pub struct RigEffect {
    pub id: u8,
    pub bone: Option<usize>,
    pub position: [f32; 3],
    pub forward: [f32; 3],
}

/// Like [`model_with_offsets`], with joint indices and weights on every mesh for GPU skinning.
/// `place(root)` is where that root sits on the assembled AC.
pub fn rigged_model(disc: &Disc, asset: &str, place: &dyn Fn(Option<&str>) -> RootPlace) -> Result<(Vec<LoadedMesh>, Rig)> {
    let data = vfs::open(disc, asset)?;
    let f = flver::read(&data)?;
    let meshes = load_from_flver(&data, &f, place, true)?;
    let rig = build_rig(&f, place)?;
    Ok((meshes, rig))
}

/// Cache of decompressed FLVER bytes + parsed headers keyed by disc asset path so repeated AC
/// rebuilds and 3D part previews skip ISO reads, DCX decompression, and BND3/FLVER header parsing.
#[derive(Default)]
pub struct FlverCache {
    entries: HashMap<String, (Vec<u8>, flver::Flver)>,
}

impl FlverCache {
    fn get_or_load(&mut self, disc: &Disc, asset: &str) -> Result<&(Vec<u8>, flver::Flver)> {
        if !self.entries.contains_key(asset) {
            let data = vfs::open(disc, asset)?;
            let f = flver::read(&data)?;
            self.entries.insert(asset.to_string(), (data, f));
        }
        Ok(&self.entries[asset])
    }

    pub fn model(&mut self, disc: &Disc, asset: &str) -> Result<Vec<LoadedMesh>> {
        let (data, f) = self.get_or_load(disc, asset)?;
        load_from_flver(data, f, &|_| RootPlace::IDENTITY, false)
    }

    pub fn rigged_model(
        &mut self,
        disc: &Disc,
        asset: &str,
        place: &dyn Fn(Option<&str>) -> RootPlace,
    ) -> Result<(Vec<LoadedMesh>, Rig)> {
        let (data, f) = self.get_or_load(disc, asset)?;
        let meshes = load_from_flver(data, f, place, true)?;
        let rig = build_rig(f, place)?;
        Ok((meshes, rig))
    }
}

fn build_rig(f: &flver::Flver, place: &dyn Fn(Option<&str>) -> RootPlace) -> Result<Rig> {
    let world = f.bone_transforms()?;
    let roots = f.bone_roots();
    let bones = f
        .bones
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let placed = place(Some(&f.bones[roots[i]].name)).then_bind(&world[i]);
            RigBone { name: b.name.clone(), parent: usize::try_from(b.parent).ok().filter(|&p| p < f.bones.len()), bind: placed }
        })
        .collect();
    let bone_of = |d: &flver::Dummy| {
        usize::try_from(d.parent_bone).ok().or(usize::try_from(d.attach_bone).ok()).filter(|&b| b < f.bones.len())
    };
    let sockets = f.dummies.iter().filter(|d| d.color[0] != 0).map(|d| (d.color[0], bone_of(d))).collect();
    let frames = f
        .dummies
        .iter()
        .filter(|d| d.color[0] != 0)
        .filter_map(|d| {
            let b = bone_of(d)?;
            let m = &world[b].m;
            let forward = [0, 1, 2].map(|r| m[r][0] * d.forward[0] + m[r][1] * d.forward[1] + m[r][2] * d.forward[2]);
            Some(SocketFrame { id: d.color[0], root: f.bones[roots[b]].name.clone(), position: world[b].apply(d.position), forward })
        })
        .collect();
    let effects = f
        .dummies
        .iter()
        .filter(|d| d.color[1] != 0)
        .map(|d| RigEffect {
            id: d.color[1],
            bone: usize::try_from(d.parent_bone).ok().filter(|&b| b < f.bones.len()),
            position: d.position,
            forward: d.forward,
        })
        .collect();
    Ok(Rig { bones, unboned: place(None).t, sockets, frames, effects })
}

fn load(disc: &Disc, asset: &str, place: &dyn Fn(Option<&str>) -> RootPlace, rig: bool) -> Result<(Vec<LoadedMesh>, flver::Flver)> {
    let data = vfs::open(disc, asset)?;
    let f = flver::read(&data)?;
    let out = load_from_flver(&data, &f, place, rig)?;
    Ok((out, f))
}

fn load_from_flver(data: &[u8], f: &flver::Flver, place: &dyn Fn(Option<&str>) -> RootPlace, rig: bool) -> Result<Vec<LoadedMesh>> {
    let world = f.bone_transforms()?;
    let bone_roots = f.bone_roots();
    let by_bone: Vec<RootPlace> = bone_roots.iter().map(|&r| place(Some(&f.bones[r].name))).collect();
    let unboned = place(None);
    let mut out = Vec::new();
    for (i, m) in f.meshes.iter().enumerate() {
        let Some(fs) = f.main_face_set(m) else { continue };
        let tris = f.triangles(data, fs).with_context(|| format!("mesh {i}"))?;
        let mut v = f.vertices(data, m).with_context(|| format!("mesh {i}"))?;
        if tris.is_empty() || v.positions.is_empty() {
            continue;
        }
        let bones = f.vertex_bones(m, &v);
        let joints = rig.then(|| vertex_joints(&f, m, &v, &bones));
        f.to_model_space(m, &mut v, &bones, &world);
        let placed_at = |i: usize| bones.get(i).and_then(|b| b.and_then(|b| by_bone.get(b))).copied().unwrap_or(unboned);
        for (i, p) in v.positions.iter_mut().enumerate() {
            *p = placed_at(i).apply_point(*p);
        }
        if v.normals.len() == v.positions.len() {
            for (i, n) in v.normals.iter_mut().enumerate() {
                *n = placed_at(i).apply_dir(*n);
            }
        }
        for ts in &mut v.tangents {
            if ts.len() == v.positions.len() {
                for (i, t) in ts.iter_mut().enumerate() {
                    let d = normalize3(placed_at(i).apply_dir([t[0], t[1], t[2]]));
                    *t = [d[0], d[1], d[2], t[3]];
                }
            }
        }
        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for &i in tris.iter().flatten() {
            let p = v.positions.get(i as usize).copied().map(mirror).context("index past the mesh's vertices")?;
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, v.positions.iter().copied().map(mirror).collect::<Vec<_>>());
        if v.normals.len() == v.positions.len() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, v.normals.iter().copied().map(mirror).collect::<Vec<_>>());
        }
        match v.uvs.first().filter(|u| u.len() == v.positions.len()) {
            Some(uv) => mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv.clone()),
            None => mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; v.positions.len()]),
        }
        if let Some(uv1) = v.uvs.get(1).filter(|u| u.len() == v.positions.len()) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1.clone());
        }
        if let Some(col) = v.colors.first().filter(|c| c.len() == v.positions.len()) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col.clone());
        }
        mesh.insert_indices(Indices::U32(tris.into_iter().flatten().collect()));
        if v.normals.len() != v.positions.len() {
            mesh.compute_smooth_normals();
        }
        let has_flver_tangents = v.normals.len() == v.positions.len()
            && v.tangents.first().is_some_and(|t| t.len() == v.positions.len());
        if has_flver_tangents {
            let tangents: Vec<[f32; 4]> = v
                .normals
                .iter()
                .zip(v.tangents[0].iter())
                .map(|(&n, &t)| flver_tangent_to_bevy(n, t))
                .collect();
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
        }
        if let Some((index, weight)) = joints {
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(index));
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weight);
        }
        let mat = f.materials.get(m.material.max(0) as usize);
        let mat_tex = |kinds: &[&str]| {
            mat.and_then(|mat| {
                let start = mat.texture_index.max(0) as usize;
                f.textures
                    .iter()
                    .skip(start)
                    .take(mat.texture_count.max(0) as usize)
                    .find(|t| kinds.contains(&t.kind.as_str()) && !t.path.is_empty())
                    .map(|t| texture_stem(&t.path))
            })
        };
        let diffuse = mat_tex(DIFFUSE_TYPES);
        let diffuse2 = mat_tex(DIFFUSE2_TYPES);
        let bump = mat_tex(BUMP_TYPES);
        let bump2 = mat_tex(BUMP2_TYPES);
        let specular = mat_tex(SPECULAR_TYPES);
        let emission = mat_tex(EMISSION_TYPES);
        let mtd = mat.map(|m| m.mtd.clone()).unwrap_or_default();
        if !has_flver_tangents && bump.is_some() {
            if mesh.generate_tangents().is_ok() {
                // MikkTSpace aligns bitangent with -dP/dv (OpenGL); flip w so B aligns with
                // +dP/dv like FLVER tangents and every normal map uses flip_normal_map_y: false.
                if let Some(VertexAttributeValues::Float32x4(ts)) = mesh.attribute_mut(Mesh::ATTRIBUTE_TANGENT) {
                    for t in ts.iter_mut() {
                        t[3] = -t[3];
                    }
                }
            }
        }
        let root = bones
            .iter()
            .flatten()
            .next()
            .and_then(|&b| bone_roots.get(b))
            .and_then(|&r| f.bones.get(r))
            .map(|b| b.name.clone());
        out.push(LoadedMesh { mesh, diffuse, diffuse2, bump, bump2, specular, emission, mtd, root, min, max });
    }
    Ok(out)
}

/// Joint indices and weights per vertex. Dynamic meshes keep all four weighted bones (their
/// vertices are already in model space); the others follow their single bone. Vertices without
/// a bone use the extra joint `bones.len()`.
fn vertex_joints(f: &flver::Flver, m: &flver::Mesh, v: &flver::Vertices, bones: &[Option<usize>]) -> (Vec<[u16; 4]>, Vec<[f32; 4]>) {
    let extra = f.bones.len() as u16;
    let global = |l: u8| -> Option<u16> {
        let b = if m.bone_indices.is_empty() { i32::from(l) } else { *m.bone_indices.get(usize::from(l))? };
        usize::try_from(b).ok().filter(|&b| b < f.bones.len()).map(|b| b as u16)
    };
    bones
        .iter()
        .enumerate()
        .map(|(k, bone)| {
            if m.dynamic != 0 {
                if let (Some(ix), Some(w)) = (v.bone_indices.get(k), v.bone_weights.get(k)) {
                    let (mut index, mut weight) = ([0u16; 4], [0f32; 4]);
                    for j in 0..4 {
                        if let Some(g) = global(ix[j]).filter(|_| w[j] > 0.0) {
                            (index[j], weight[j]) = (g, w[j]);
                        }
                    }
                    let sum: f32 = weight.iter().sum();
                    if sum > 0.0 {
                        return (index, weight.map(|x| x / sum));
                    }
                }
            }
            ([bone.map_or(extra, |b| b as u16), 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
        })
        .unzip()
}

/// Strips sRGB from block-compressed or RGBA8 formats so data textures (normal, specular, color
/// mask, camo) sample in linear space.
pub fn to_linear(img: &mut Image) {
    img.texture_descriptor.format = match img.texture_descriptor.format {
        TextureFormat::Bc1RgbaUnormSrgb => TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc3RgbaUnormSrgb => TextureFormat::Bc3RgbaUnorm,
        TextureFormat::Rgba8UnormSrgb => TextureFormat::Rgba8Unorm,
        other => other,
    };
}

/// Best-resolution [`TextureRef`] for `name`: prefers high-res `*_gt.tpf.dcx` (360 garage
/// showcase pack, `0x82b50540`) over combat `*.tpf.dcx` and low-LOD `*_l.tpf.dcx`, breaking ties
/// by pixel area descending.
pub fn best_texture_ref(name: &str) -> Option<&'static TextureRef> {
    let rank = |t: &TextureRef| -> (u8, u64) {
        let p = t.pack;
        let tier = if p.ends_with("_gt.tpf.dcx") || p.ends_with("_gt.tpf") {
            2
        } else if p.ends_with("_l.tpf.dcx") || p.ends_with("_l.tpf") {
            0
        } else {
            1
        };
        (tier, t.width as u64 * t.height as u64)
    };
    let mut best: Option<&'static TextureRef> = None;
    for t in acvd_data::textures_named(name) {
        if best.is_none_or(|b| rank(t) > rank(b)) {
            best = Some(t);
        }
    }
    best
}

/// The material for `part`: its diffuse and normal-map textures, or flat grey when it has none,
/// `flat` is set, or the texture fails to load (reported to stderr and counted in `missing`).
pub fn material(disc: &Disc, part: &LoadedMesh, packs: &mut Packs, images: &mut Assets<Image>, flat: bool, missing: &mut usize) -> StandardMaterial {
    let texture = part.diffuse.as_deref().filter(|_| !flat).and_then(|name| match packs.image_handle(disc, images, name) {
        Ok(h) => Some(h),
        Err(e) => {
            *missing += 1;
            eprintln!("{e:#}");
            None
        }
    });
    let normal_map = part.bump.as_deref().filter(|_| !flat).and_then(|name| {
        packs.linear_image_handle(disc, images, name).ok()
    });
    StandardMaterial {
        base_color: if texture.is_some() { Color::WHITE } else { Color::srgb(0.6, 0.6, 0.62) },
        base_color_texture: texture,
        normal_map_texture: normal_map,
        flip_normal_map_y: false,
        perceptual_roughness: 0.7,
        ..Default::default()
    }
}

/// Decoded TPF packs and GPU texture handles, keyed by pack asset path and `(pack, index, linear)`
/// so repeated AC rebuilds and shared submesh textures decode and untile each texture once.
#[derive(Default)]
pub struct Packs {
    packs: HashMap<String, Vec<u8>>,
    index_by_name: HashMap<String, HashMap<String, usize>>,
    handles: HashMap<(String, usize, bool), Handle<Image>>,
    utility: HashMap<&'static str, Handle<Image>>,
}

impl Packs {
    pub fn image(&mut self, disc: &Disc, name: &str) -> Result<Image> {
        let t: &TextureRef = best_texture_ref(name).with_context(|| format!("no texture named `{name}`"))?;
        self.texture(disc, t)
    }

    /// Like [`Packs::image`], forcing a linear (non-sRGB) texture format for normal/specular/mask maps.
    pub fn linear_image(&mut self, disc: &Disc, name: &str) -> Result<Image> {
        let mut img = self.image(disc, name)?;
        to_linear(&mut img);
        Ok(img)
    }

    /// Cached GPU [`Handle<Image>`] for texture `name` (sRGB). Decodes/untiles once per `(pack, index, false)`.
    pub fn image_handle(&mut self, disc: &Disc, images: &mut Assets<Image>, name: &str) -> Result<Handle<Image>> {
        let t: &TextureRef = best_texture_ref(name).with_context(|| format!("no texture named `{name}`"))?;
        self.texture_handle_at(disc, images, t.pack, t.index, t.name, false)
    }

    /// Cached GPU [`Handle<Image>`] for texture `name` in linear (non-sRGB) format.
    pub fn linear_image_handle(&mut self, disc: &Disc, images: &mut Assets<Image>, name: &str) -> Result<Handle<Image>> {
        let t: &TextureRef = best_texture_ref(name).with_context(|| format!("no texture named `{name}`"))?;
        self.texture_handle_at(disc, images, t.pack, t.index, t.name, true)
    }

    /// Cached GPU [`Handle<Image>`] for texture `index` of TPF `pack`.
    pub fn texture_handle_at(
        &mut self,
        disc: &Disc,
        images: &mut Assets<Image>,
        pack: &str,
        index: usize,
        name: &str,
        linear: bool,
    ) -> Result<Handle<Image>> {
        let key = (pack.to_string(), index, linear);
        if let Some(h) = self.handles.get(&key) {
            if images.contains(h) {
                return Ok(h.clone());
            }
        }
        let img = if linear {
            self.linear_texture_at(disc, pack, index, name)?
        } else {
            self.texture_at(disc, pack, index, name)?
        };
        let handle = images.add(img);
        self.handles.insert(key, handle.clone());
        Ok(handle)
    }

    /// Cached 1×1 utility texture handle (e.g. empty color mask or empty decal map).
    pub fn utility_handle(
        &mut self,
        images: &mut Assets<Image>,
        key: &'static str,
        make: impl FnOnce() -> Image,
    ) -> Handle<Image> {
        if let Some(h) = self.utility.get(key) {
            if images.contains(h) {
                return h.clone();
            }
        }
        let h = images.add(make());
        self.utility.insert(key, h.clone());
        h
    }

    /// Like [`Packs::texture_at`], forcing a linear (non-sRGB) texture format.
    pub fn linear_texture_at(&mut self, disc: &Disc, pack: &str, index: usize, name: &str) -> Result<Image> {
        let mut img = self.texture_at(disc, pack, index, name)?;
        to_linear(&mut img);
        Ok(img)
    }

    fn pack(&mut self, disc: &Disc, pack: &str) -> Result<&Vec<u8>> {
        Ok(match self.packs.entry(pack.to_string()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(vfs::open(disc, pack)?),
        })
    }

    /// Index of the texture called `name` (ASCII case-insensitive) in the TPF at asset path `pack`.
    pub fn find(&mut self, disc: &Disc, pack: &str, name: &str) -> Result<Option<usize>> {
        if !self.index_by_name.contains_key(pack) {
            let header = tpf::read(self.pack(disc, pack)?)?;
            let mut map = HashMap::with_capacity(header.textures.len());
            for (i, t) in header.textures.iter().enumerate() {
                map.entry(t.name.to_ascii_lowercase()).or_insert(i);
            }
            self.index_by_name.insert(pack.to_string(), map);
        }
        Ok(self.index_by_name[pack].get(&name.to_ascii_lowercase()).copied())
    }

    /// `t` comes from the extracted sheets; the format, size and levels are read from the pack on
    /// `disc` itself.
    pub fn texture(&mut self, disc: &Disc, t: &TextureRef) -> Result<Image> {
        self.texture_at(disc, t.pack, t.index, t.name)
    }

    /// Texture `index` of the TPF at asset path `pack`, `name` only labelling errors.
    pub fn texture_at(&mut self, disc: &Disc, pack: &str, index: usize, name: &str) -> Result<Image> {
        let pack = self.pack(disc, pack)?;
        let header = tpf::read(pack)?;
        let tex = header.textures.get(index).context("texture index past the pack")?;
        let row = acvd_data::texture_format(tex.format);
        let is_ctx1 = matches!(row.map(|f| f.name), Some("ctx1"));
        // Format 33 (HT-BC3, `floats_unk == 1010`, `floats == [scale]`): normalized RGB * A *
        // `FC_HTTextureRenge_0` (`Normal/Flver_ACColorDecalColDifSpcBumpIbl.fpo` insns 89-102).
        let is_ht_bc3 = tex.format == 33;
        let format = if is_ht_bc3 {
            TextureFormat::Rgba8UnormSrgb
        } else {
            match row.map(|f| f.name) {
                Some("bc1") => TextureFormat::Bc1RgbaUnormSrgb,
                Some("bc3") => TextureFormat::Bc3RgbaUnormSrgb,
                // DXN is two BC4 blocks, x then y (sheets/texture_formats.csv code 23). Bevy treats
                // Bc5RgUnorm as a two-component normal map.
                Some("dxn") => TextureFormat::Bc5RgUnorm,
                // CTX1 (sheets/texture_formats.csv codes 24 and 25) is CPU-expanded to Rg8Unorm,
                // which Bevy also flags as TWO_COMPONENT_NORMAL_MAP.
                Some("ctx1") => TextureFormat::Rg8Unorm,
                other => bail!("texture `{name}` format {other:?}"),
            }
        };
        ensure!(tex.faces() == 1, "texture `{name}` is a cube map");
        let (width, height) = (tex.width, tex.height);
        ensure!(width % 4 == 0 && height % 4 == 0, "texture `{name}` is {width}x{height}");
        let block = row.map_or(16, |f| f.block_bytes);
        let bytes = tex.linear(header.platform, pack, block)?;
        let mut levels = tex.levels();
        let (start, last) = tpf::block_level_span(width, height, levels, 0, levels - 1, block);
        let mut end = (start + last) as usize;
        if end > bytes.len() {
            levels = 1;
            end = tpf::block_level_span(width, height, 1, 0, 0, block).1 as usize;
            ensure!(end <= bytes.len(), "texture `{name}` is shorter than its first level");
        }
        let data = if is_ctx1 {
            tpf::decode_ctx1(&bytes[..end], width, height, levels, 1)?
        } else if is_ht_bc3 {
            let scale = tex.floats.first().copied().unwrap_or(1.0);
            tpf::decode_bc3_ht(&bytes[..end], width, height, levels, 1, scale)?
        } else {
            bytes[..end].to_vec()
        };
        let mut image = Image::default();
        image.texture_descriptor.size = Extent3d { width: width as u32, height: height as u32, depth_or_array_layers: 1 };
        image.texture_descriptor.dimension = TextureDimension::D2;
        image.texture_descriptor.format = format;
        image.texture_descriptor.mip_level_count = levels;
        image.data = Some(data);
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            anisotropy_clamp: 8,
            ..ImageSamplerDescriptor::linear()
        });
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flver_tangent_conversion_preserves_mirrored_tbn_for_both_handednesses() {
        // FLVER N = (0, 0, 1), v_stored (+dP/dv) = (0, 1, 0), w = ±1.
        // 360: T_360 = cross(N, v_stored) * w = (-w, 0, 0), B_360 = (0, 1, 0).
        // Mirrored into Bevy by S = diag(-1, 1, 1):
        //   N_bevy = (0, 0, 1), T_bevy = (w, 0, 0), B_bevy = (0, 1, 0).
        // Bevy computes B = tangent.w * cross(N_bevy, T_bevy).
        let n_flver = [0.0, 0.0, 1.0];
        let n_bevy = mirror(n_flver);
        for w in [1.0f32, -1.0] {
            let t = flver_tangent_to_bevy(n_flver, [0.0, 1.0, 0.0, w]);
            let t_xyz = [t[0], t[1], t[2]];
            let b_raw = cross(n_bevy, t_xyz);
            let b_bevy = [b_raw[0] * t[3], b_raw[1] * t[3], b_raw[2] * t[3]];
            assert!((t_xyz[0] - w).abs() < 1e-5 && t_xyz[1].abs() < 1e-5 && t_xyz[2].abs() < 1e-5, "w={w} t={t:?}");
            assert!(b_bevy[0].abs() < 1e-5 && (b_bevy[1] - 1.0).abs() < 1e-5 && b_bevy[2].abs() < 1e-5, "w={w} b={b_bevy:?}");
        }
    }

    #[test]
    fn x360_normal_maps_and_tangents_load_on_ac_part() {
        let iso = vfs::repo_root().join(vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let meshes = model(&disc, "model/ac/parts/arm/am0010/am0010_m.bnd.dcx|am0010.flv").unwrap();
        assert!(!meshes.is_empty());
        let first = &meshes[0];
        assert!(first.bump.is_some(), "am0010 should expose g_Bumpmap");
        assert!(first.specular.is_some(), "am0010 should expose g_Specular");
        assert!(first.mesh.attribute(Mesh::ATTRIBUTE_TANGENT).is_some(), "am0010 should carry ATTRIBUTE_TANGENT");

        let mut packs = Packs::default();
        // CTX1 (code 25) normal map -> Rg8Unorm
        let ctx1_img = packs.linear_image(&disc, first.bump.as_deref().unwrap()).unwrap();
        assert_eq!(ctx1_img.texture_descriptor.format, TextureFormat::Rg8Unorm);
        assert!(ctx1_img.texture_descriptor.mip_level_count > 1);
        // DXN (code 23) normal map -> Bc5RgUnorm
        let dxn_img = packs.linear_image(&disc, "lg3041_n").unwrap();
        assert_eq!(dxn_img.texture_descriptor.format, TextureFormat::Bc5RgUnorm);
        // Specular map -> linear BC1/BC3
        let spc_img = packs.linear_image(&disc, first.specular.as_deref().unwrap()).unwrap();
        assert!(
            matches!(spc_img.texture_descriptor.format, TextureFormat::Bc1RgbaUnorm | TextureFormat::Bc3RgbaUnorm),
            "unexpected specular format {:?}",
            spc_img.texture_descriptor.format
        );
    }

    #[test]
    fn x360_all_normal_maps_untile_and_decode() {
        let iso = vfs::repo_root().join(vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let mut packs = Packs::default();
        let (mut dxn_n, mut ctx24_n, mut ctx25_n) = (0usize, 0usize, 0usize);
        for t in acvd_data::generated::textures::ALL.iter().flat_map(|g| g.iter()) {
            if !matches!(t.format, 23 | 24 | 25) {
                continue;
            }
            let img = packs
                .texture(&disc, t)
                .unwrap_or_else(|e| panic!("{} ({}, fmt {}): {e:#}", t.name, t.pack, t.format));
            match t.format {
                23 => {
                    assert_eq!(img.texture_descriptor.format, TextureFormat::Bc5RgUnorm);
                    dxn_n += 1;
                }
                24 => {
                    assert_eq!(img.texture_descriptor.format, TextureFormat::Rg8Unorm);
                    ctx24_n += 1;
                }
                25 => {
                    assert_eq!(img.texture_descriptor.format, TextureFormat::Rg8Unorm);
                    ctx25_n += 1;
                }
                _ => unreachable!(),
            }
            // Release cached pack once we move on so memory stays tiny across 3,933 textures.
            if packs.packs.len() > 64 {
                packs.packs.clear();
            }
        }
        assert_eq!(dxn_n, 162, "expected 162 DXN (code 23) normal maps");
        assert_eq!(ctx24_n, 235, "expected 235 CTX1 (code 24) normal maps");
        assert_eq!(ctx25_n, 3536, "expected 3536 CTX1 (code 25) normal maps");
    }

    #[test]
    fn x360_map_diffuse_multi_exposes_diffuse2_uv1_and_vertex_color() {
        let iso = vfs::repo_root().join(vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let meshes = model(&disc, "model/map/m4000/m4000_m.dcx.bnd|m9000.flv").unwrap();
        let multi = meshes
            .iter()
            .find(|m| m.mtd.contains("Map_Diffuse_Multi"))
            .expect("m9000.flv should have a Map_Diffuse_Multi submesh");
        assert_eq!(multi.diffuse.as_deref(), Some("m3110_m1000_0001"));
        assert_eq!(multi.diffuse2.as_deref(), Some("m3110_m1000_0002"));
        assert!(
            multi.mesh.attribute(Mesh::ATTRIBUTE_UV_1).is_some(),
            "Map_Diffuse_Multi should carry ATTRIBUTE_UV_1"
        );
        let Some(VertexAttributeValues::Float32x4(colors)) =
            multi.mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("Map_Diffuse_Multi should carry Float32x4 ATTRIBUTE_COLOR");
        };
        assert!(!colors.is_empty());
        // Big-endian D3DCOLOR [A, R, G, B] -> [R, G, B, A]: RGB should be ~1.0, alpha blends 0..1.
        assert!(colors.iter().all(|c| c[0] > 0.99 && c[1] > 0.99 && c[2] > 0.99));
        let min_a = colors.iter().map(|c| c[3]).fold(1.0f32, f32::min);
        let max_a = colors.iter().map(|c| c[3]).fold(0.0f32, f32::max);
        assert!(min_a < 0.1 && max_a > 0.9, "alpha range {min_a}..{max_a}");
    }

    #[test]
    fn x360_best_texture_ref_prefers_gt_pack_and_decodes_ht_bc3() {
        let iso = vfs::repo_root().join(vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        // AC frame parts should pick the high-res *_gt.tpf.dcx pack (0x82b50540) over combat/low-LOD.
        let lg = best_texture_ref("lg0010").expect("lg0010");
        assert!(lg.pack.ends_with("lg0010_gt.tpf.dcx"), "picked {}", lg.pack);
        assert_eq!((lg.width, lg.height, lg.format), (1024, 512, 33));

        // Format-33 HT-BC3 (hand weapon hl0110) must unpack rgb * alpha * floats[0] -> Rgba8UnormSrgb
        // with real luminance contrast (raw RGB alone is flat ~147 grey).
        let mut packs = Packs::default();
        let img = packs.image(&disc, "hl0110").unwrap();
        assert_eq!(img.texture_descriptor.format, TextureFormat::Rgba8UnormSrgb);
        assert_eq!(img.texture_descriptor.size.width, 1024);
        assert_eq!(img.texture_descriptor.size.height, 512);
        assert!(img.texture_descriptor.mip_level_count > 1);
        let data = img.data.as_ref().unwrap();
        let l0 = &data[..1024 * 512 * 4];
        let mut min_r = 255u8;
        let mut max_r = 0u8;
        for px in l0.chunks_exact(4) {
            assert_eq!(px[3], 255, "HT-BC3 output alpha must be opaque 255");
            min_r = min_r.min(px[0]);
            max_r = max_r.max(px[0]);
        }
        assert!(max_r - min_r > 80, "HT-BC3 decoded hl0110 should have real panel contrast, got {min_r}..{max_r}");
    }
}
