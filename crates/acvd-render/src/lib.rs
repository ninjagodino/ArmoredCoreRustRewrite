//! Disc -> Bevy assets: FLVER meshes and TPF block-compressed textures, shared by the viewer
//! and the game.
//!
//! FLVER triangles wind clockwise around their normals in a left-handed frame; mirroring X
//! turns them into Bevy's right-handed, counter-clockwise front faces without touching indices.

pub mod app;
pub mod menu;
pub mod text;

use std::collections::HashMap;

use acvd_data::TextureRef;
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{flver, tpf};
use anyhow::{bail, ensure, Context, Result};
use bevy::asset::{Assets, RenderAssetUsages};
use bevy::color::Color;
use bevy::pbr::StandardMaterial;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

pub struct LoadedMesh {
    pub mesh: Mesh,
    pub diffuse: Option<String>,
    /// Bounds of the vertices the main face set uses, already mirrored.
    pub min: [f32; 3],
    pub max: [f32; 3],
}

const DIFFUSE_TYPES: &[&str] = &["g_DiffuseTexture", "g_Diffuse"];

fn mirror(v: [f32; 3]) -> [f32; 3] {
    [-v[0], v[1], v[2]]
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
    let (meshes, f) = load(disc, asset, place, true)?;
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
    Ok((meshes, Rig { bones, unboned: place(None).t, sockets, frames, effects }))
}

fn load(disc: &Disc, asset: &str, place: &dyn Fn(Option<&str>) -> RootPlace, rig: bool) -> Result<(Vec<LoadedMesh>, flver::Flver)> {
    let data = vfs::open(disc, asset)?;
    let f = flver::read(&data)?;
    let world = f.bone_transforms()?;
    let by_bone: Vec<RootPlace> = f.bone_roots().into_iter().map(|r| place(Some(&f.bones[r].name))).collect();
    let unboned = place(None);
    let mut out = Vec::new();
    for (i, m) in f.meshes.iter().enumerate() {
        let Some(fs) = f.main_face_set(m) else { continue };
        let tris = f.triangles(&data, fs).with_context(|| format!("mesh {i}"))?;
        let mut v = f.vertices(&data, m).with_context(|| format!("mesh {i}"))?;
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
        mesh.insert_indices(Indices::U32(tris.into_iter().flatten().collect()));
        if v.normals.len() != v.positions.len() {
            mesh.compute_smooth_normals();
        }
        if let Some((index, weight)) = joints {
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(index));
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weight);
        }
        let diffuse = f.materials.get(m.material.max(0) as usize).and_then(|mat| {
            let start = mat.texture_index.max(0) as usize;
            f.textures
                .iter()
                .skip(start)
                .take(mat.texture_count.max(0) as usize)
                .find(|t| DIFFUSE_TYPES.contains(&t.kind.as_str()) && !t.path.is_empty())
                .map(|t| texture_stem(&t.path))
        });
        out.push(LoadedMesh { mesh, diffuse, min, max });
    }
    Ok((out, f))
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

/// The material for `part`: its diffuse texture, or flat grey when it has none, `flat` is set,
/// or the texture fails to load (reported to stderr and counted in `missing`).
pub fn material(disc: &Disc, part: &LoadedMesh, packs: &mut Packs, images: &mut Assets<Image>, flat: bool, missing: &mut usize) -> StandardMaterial {
    let texture = part.diffuse.as_deref().filter(|_| !flat).and_then(|name| match packs.image(disc, name) {
        Ok(img) => Some(images.add(img)),
        Err(e) => {
            *missing += 1;
            eprintln!("{e:#}");
            None
        }
    });
    StandardMaterial {
        base_color: if texture.is_some() { Color::WHITE } else { Color::srgb(0.6, 0.6, 0.62) },
        base_color_texture: texture,
        perceptual_roughness: 0.7,
        ..Default::default()
    }
}

/// Decoded TPF packs, keyed by pack asset path, so a model's textures open each pack once.
#[derive(Default)]
pub struct Packs(HashMap<String, Vec<u8>>);

impl Packs {
    pub fn image(&mut self, disc: &Disc, name: &str) -> Result<Image> {
        let t: &TextureRef = acvd_data::textures_named(name).next().with_context(|| format!("no texture named `{name}`"))?;
        self.texture(disc, t)
    }

    fn pack(&mut self, disc: &Disc, pack: &str) -> Result<&Vec<u8>> {
        Ok(match self.0.entry(pack.to_string()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(vfs::open(disc, pack)?),
        })
    }

    /// Index of the texture called `name` (ASCII case-insensitive) in the TPF at asset path `pack`.
    pub fn find(&mut self, disc: &Disc, pack: &str, name: &str) -> Result<Option<usize>> {
        let header = tpf::read(self.pack(disc, pack)?)?;
        Ok(header.textures.iter().position(|t| t.name.eq_ignore_ascii_case(name)))
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
        let format = match row.map(|f| f.name) {
            Some("bc1") => TextureFormat::Bc1RgbaUnormSrgb,
            Some("bc3") => TextureFormat::Bc3RgbaUnormSrgb,
            other => bail!("texture `{name}` format {other:?}"),
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
        let mut image = Image::default();
        image.texture_descriptor.size = Extent3d { width: width as u32, height: height as u32, depth_or_array_layers: 1 };
        image.texture_descriptor.dimension = TextureDimension::D2;
        image.texture_descriptor.format = format;
        image.texture_descriptor.mip_level_count = levels;
        image.data = Some(bytes[..end].to_vec());
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });
        Ok(image)
    }
}
