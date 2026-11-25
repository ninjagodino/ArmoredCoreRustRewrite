//! FFX effects (`sfx/acv_commoneffects.ffxbnd`, read by `acvd_formats::ffx`) played in the
//! world: billboards and `model/sfx/sfx_m` models on static nodes, point lights, and particle
//! clusters drawn as one camera-facing batch mesh each. Which node templates and appearance
//! actions are drawn, and how each param slot is read, is in `sheets/ffx_actions.csv`; nearly
//! all of it is inferred from the effect files, not traced in the 360 code.
//!
//! An effect plays at its [`Sfx`] entity, effect +Z along the entity's +Z. Effect-space
//! offsets and angles are in FLVER axes and mirror on X like the models. The AC's FLVER effect
//! points become [`EffectPoint`] entities; [`boosters`] lights the `param/mapsfxparam.bin`
//! booster effects on them the way the 360 dispatcher `0x828932a0` does.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use acvd_data::find;
use acvd_data::generated::sfx::{
    MapSfxparamSt, PARAM_ENVSFXPARAM_BIN, PARAM_GROUNDSFXPARAM_BIN, PARAM_MAPSFXPARAM_BIN,
};
use acvd_formats::ffx::{self, Param, ParamList, Value};
use acvd_formats::vfs::{self, Disc};
use bevy::asset::{AssetId, RenderAssetUsages};
use bevy::camera::visibility::{NoFrustumCulling, VisibilitySystems};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::math::{Affine2, Affine3A};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::transform::TransformSystems;

use acvd_render::app::ShotMark;

use crate::collision::{Collision, GroundSurface, Layer};
use crate::control::{self, Pilot};

const EFFECTS: &str = "sfx/acv_commoneffects.ffxbnd";
const MODELS: &str = "model/sfx/sfx_m.bnd.dcx";
/// Point-light lumens at colour alpha 1; not game data (the FFX light has colour and radius only).
const LIGHT_LUMENS: f32 = 400_000.0;
/// Action 82 (point sprite) size 1 drawn as this many metres; not game data.
const POINT_SPRITE_METRES: f32 = 0.12;
/// Action 82 sparks stretch along their velocity over this many seconds; not game data.
const POINT_SPRITE_STREAK: f32 = 0.02;
/// Horizontal speed, metres per tick, above which a grounded boost still counts as moving
/// when the stick is neutral. The 360 ground-boost slot (`0x828225e8`) simply is not called
/// while standing; this threshold is the stand-in.
const BOOST_MOVING: f32 = 0.05;
/// Stick component past which a side booster group fires (`0x82013518`, 0.1).
const SIDE_STICK: f32 = 0.1;
/// Full-stick glide effect scale. Slots `+0x34` / `+0x38` clamp the stick length to global
/// param 0 `+0x498` and pass that as both intensities (mode 1). A full stick measured 0.6
/// (`private/xenia/boost_vfx.txt`).
const GLIDE_SFX_SCALE: f32 = 0.6;
/// Glide main and foot effects: slots `+0x744` / `+0x748` of the sfx id table at object
/// `+0x740`. `0x82891e18` sets them to 131 / 120 and the `MAP_SFXPARAM_ST` copy `0x82891e68`
/// skips them. The 360 starts 131 four times and 120 twice when a glide begins, and never 1379
/// or 303 (`private/xenia/glide_ids.jsonl`).
const GLIDE_MAIN: i32 = 131;
const GLIDE_FOOT: i32 = 120;
/// Air-glide ribbon trails: `f0000134` (action 40) fires on the air-entry dummies from TAE
/// anims 63..70 frame 15-18 (`tae/ac/motion_ow_disarm/own*.tae`), then follows the glide until
/// it ends. Design 5010 has three of the six sockets (core 213/218, arm 223), matching the three
/// starts in `private/xenia/glide_ids.jsonl`.
const GLIDE_TRAIL: i32 = 134;
/// Main nozzles, the booster light on them, leg back nozzles, and the side-booster pairs.
/// Built in `0x82893ec0` and selected by `0x82892f08`.
const MAIN_NOZZLES: std::ops::RangeInclusive<u8> = 31..=34;
const LEG_NOZZLES: std::ops::RangeInclusive<u8> = 21..=24;
/// Quick Boost effect scales fed to `0x82891ce8` (`> 1.0 => ac_qb_large 305`, `< 1.0 =>
/// ac_qb_shortage 304`, `== 1.0 => ac_qb_normal 303`).
const QB_SHORTAGE_SCALE: f32 = 0.75;
const QB_LARGE_SCALE: f32 = 1.5;
/// `param/groundsfxparam.bin` row IDs (`0x82835570` -> `0x82833d54`, `0x829ee1d0`).
const GROUND_ROW_WALK: u32 = 1;
const GROUND_ROW_LAND: u32 = 2;
const GROUND_ROW_SLIDE: u32 = 3;
const GROUND_ROW_KICK: u32 = 7;
/// Minimum horizontal speed (m/tick) for grounded boost/slide dust emission (`groundsfxparam` row 3).
const GROUND_SLIDE_SPEED: f32 = 0.25;
/// Height clearance (m, `0x82382688` `water_y - 5.0 < ac_y`) for near-water continuous spray
/// (`envsfxparam.bin` row 0 `water_spray_sfx_id = 1420`).
const WATER_SPRAY_CLEARANCE: f32 = 6.0;

pub struct SfxPlugin {
    pub disc: Disc,
}

impl Plugin for SfxPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Dir(self.disc.clone()))
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    boosters.after(control::pilot),
                    qb_sfx.after(control::pilot),
                    ground_and_water_sfx
                        .after(control::pilot)
                        .after(crate::pose::animate),
                    tae_events.after(crate::pose::animate),
                    preview,
                ),
            )
            .add_systems(
                PostUpdate,
                (simulate, probe_shot, sweep)
                    .chain()
                    .after(TransformSystems::Propagate)
                    .before(VisibilitySystems::CheckVisibility),
            );
    }
}

#[derive(Resource)]
struct Dir(Disc);

/// A playing effect. Effects end on their own when every node's life is over; [`Sfx::stop`]
/// ends the nodes that never would. The entity despawns when its effect ends.
#[derive(Component)]
pub struct Sfx {
    pub id: i32,
    stopping: bool,
    run: Option<Run>,
}

impl Sfx {
    pub fn new(id: i32) -> Self {
        Self {
            id,
            stopping: false,
            run: None,
        }
    }

    pub fn stop(&mut self) {
        self.stopping = true;
    }
}

/// An FFX effect point of a part: dummy colour byte 1, local +Z along the dummy's forward.
#[derive(Component)]
pub struct EffectPoint {
    pub id: u8,
    pub column: &'static str,
    /// Effects currently parented here, with the uniform scale last applied.
    playing: Vec<(i32, Entity, f32)>,
}

/// Spawns FFX effects triggered by animation TAE kind `0` events (360 `0x8289acd0`) on the
/// matching [`EffectPoint`] dummy socket.
fn tae_events(
    mut commands: Commands,
    motion: Option<Res<crate::pose::Motion>>,
    points: Query<(Entity, &EffectPoint)>,
) {
    let Some(motion) = motion else { return };
    for ev in &motion.tae_events {
        if let crate::pose::TaeFired::Ffx {
            ffx_id,
            dummy_id,
            scale,
        } = ev
        {
            if *ffx_id > 0 {
                // Prefer AC body/arm/rack sockets over weapon sockets when dummy IDs overlap
                // (e.g. bay-shift dummies 1/2 live on `arms`/`rack_*`).
                let target = points
                    .iter()
                    .find(|(_, p)| {
                        p.id == *dummy_id
                            && matches!(p.column, "arms" | "core" | "legs" | "rack_r" | "rack_l")
                    })
                    .or_else(|| points.iter().find(|(_, p)| p.id == *dummy_id));
                if let Some((entity, _)) = target {
                    commands.spawn((
                        Sfx::new(*ffx_id),
                        Transform::from_scale(Vec3::splat(*scale)),
                        Visibility::default(),
                        ChildOf(entity),
                    ));
                }
            }
        }
    }
}

/// Plays effect `.0` in front of the AC, again each time it ends (`--sfx <id>`).
#[derive(Resource)]
pub struct Preview(pub i32);

/// Spawns `rig`'s effect points under the part's `joints` (bone `i` is `joints[i]`, the last
/// joint carries dummies without a bone).
pub fn effect_points(
    commands: &mut Commands,
    rig: &acvd_render::Rig,
    joints: &[Entity],
    column: &'static str,
) {
    for e in &rig.effects {
        let Some(&joint) = e.bone.and_then(|b| joints.get(b)).or(joints.last()) else {
            continue;
        };
        let forward = mirror(Vec3::from(e.forward)).normalize_or(Vec3::Z);
        let transform = Transform::from_translation(mirror(Vec3::from(e.position)))
            .with_rotation(Quat::from_rotation_arc(Vec3::Z, forward));
        commands.spawn((
            EffectPoint {
                id: e.id,
                column,
                playing: Vec::new(),
            },
            transform,
            Visibility::default(),
            ChildOf(joint),
        ));
    }
}

fn mirror(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, v.z)
}

// ---------------------------------------------------------------------------------------------
// Library: effect definitions, textures and models, loaded on first use.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MaterialKey {
    texture: Option<AssetId<Image>>,
    blend: Blend,
    color: [u8; 4],
}

#[derive(Resource)]
struct Library {
    disc: Disc,
    effects: HashMap<i32, Option<Arc<Vec<Arc<NodeDef>>>>>,
    textures: HashMap<String, Option<Handle<Image>>>,
    models: HashMap<i32, Option<Arc<Vec<Part>>>>,
    /// Shared [`StandardMaterial`] cache keyed by `(texture, blend, quantized_srgba)` so particle
    /// clusters and static billboards/models reuse identical materials instead of allocating a
    /// new GPU material per spawned effect.
    shared_materials: HashMap<MaterialKey, Handle<StandardMaterial>>,
    packs: acvd_render::Packs,
    quad: Handle<Mesh>,
    /// Billboard quad with tangents, so a normal map can refract (action 43).
    #[allow(dead_code)]
    haze: Handle<Mesh>,
    dot: Handle<Image>,
    seed: u32,
}

/// One mesh of an sfx model, with a CPU copy for particle batches.
struct Part {
    mesh: Handle<Mesh>,
    texture: Option<Handle<Image>>,
    positions: Vec<Vec3>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

fn setup(
    mut commands: Commands,
    dir: Res<Dir>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
) {
    let n = 32u32;
    let mut dot = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let d = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / n as f32 * 2.0 - Vec2::ONE;
            let a = (1.0 - d.length()).clamp(0.0, 1.0);
            dot.extend([255, 255, 255, (a * a * 255.0) as u8]);
        }
    }
    let dot = Image::new(
        Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        dot,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let mut haze = Mesh::from(Rectangle::new(1.0, 1.0));
    // A camera-facing quad with a flat normal does not bend a head-on view. The normal map
    // only applies when the mesh has tangents.
    if let Err(err) = haze.generate_tangents() {
        warn!("heat-haze tangents: {err}");
    }
    commands.insert_resource(Library {
        disc: dir.0.clone(),
        effects: HashMap::new(),
        textures: HashMap::new(),
        models: HashMap::new(),
        shared_materials: HashMap::new(),
        packs: acvd_render::Packs::default(),
        quad: meshes.add(Rectangle::new(1.0, 1.0)),
        haze: meshes.add(haze),
        dot: images.add(dot),
        seed: 0x2545_f491,
    });
}

fn quantize_rgba(c: Vec4) -> [u8; 4] {
    [
        (c.x.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.y.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.z.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.w.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn cached_material(
    cache: &mut HashMap<MaterialKey, Handle<StandardMaterial>>,
    texture: Option<Handle<Image>>,
    blend: Blend,
    color: Vec4,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    let key = MaterialKey {
        texture: texture.as_ref().map(|h| h.id()),
        blend,
        color: quantize_rgba(color),
    };
    cache
        .entry(key)
        .or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: srgba(color),
                base_color_texture: texture,
                unlit: true,
                alpha_mode: match blend {
                    Blend::Alpha => AlphaMode::Blend,
                    Blend::Add => AlphaMode::Add,
                },
                double_sided: true,
                cull_mode: None,
                fog_enabled: false,
                ..default()
            })
        })
        .clone()
}

impl Library {
    fn effect(&mut self, id: i32) -> Option<Arc<Vec<Arc<NodeDef>>>> {
        let disc = &self.disc;
        self.effects
            .entry(id)
            .or_insert_with(|| {
                let read = || -> anyhow::Result<ffx::Effect> {
                    ffx::read(&vfs::open(disc, &format!("{EFFECTS}|f{id:07}.ffx"))?)
                };
                match read() {
                    Ok(e) => {
                        let roots = e
                            .states
                            .first()
                            .into_iter()
                            .flat_map(|s| &s.actions)
                            .flat_map(|a| nodes(&a.params))
                            .collect();
                        Some(Arc::new(roots))
                    }
                    Err(err) => {
                        warn!("sfx {id}: {err:#}");
                        None
                    }
                }
            })
            .clone()
    }

    fn texture(&mut self, id: i32, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        if id <= 0 {
            return None;
        }
        // Distortion textures are stored as `s1206_n` (the normal map), not `s1206`.
        self.texture_named(&format!("s{id:04}"), images)
            .or_else(|| self.texture_named(&format!("s{id:04}_n"), images))
    }

    fn texture_named(&mut self, name: &str, images: &mut Assets<Image>) -> Option<Handle<Image>> {
        if let Some(t) = self.textures.get(name) {
            return t.clone();
        }
        let found = acvd_data::textures_named(name)
            .find(|t| t.pack.starts_with("model/sfx/"))
            .or_else(|| acvd_data::textures_named(name).next());
        let handle = match found.map(|t| self.packs.texture(&self.disc, t)) {
            Some(Ok(img)) => Some(images.add(img)),
            Some(Err(e)) => {
                warn!("sfx texture {name}: {e:#}");
                None
            }
            None => {
                warn!("sfx texture {name}: not on the disc");
                None
            }
        };
        self.textures.insert(name.to_owned(), handle.clone());
        handle
    }

    fn model(
        &mut self,
        id: i32,
        meshes: &mut Assets<Mesh>,
        images: &mut Assets<Image>,
    ) -> Option<Arc<Vec<Part>>> {
        if let Some(m) = self.models.get(&id) {
            return m.clone();
        }
        let loaded = match acvd_render::model(&self.disc, &format!("{MODELS}|s{id:04}.flv")) {
            Ok(l) => l,
            Err(e) => {
                warn!("sfx model {id}: {e:#}");
                self.models.insert(id, None);
                return None;
            }
        };
        let mut parts = Vec::new();
        for m in loaded {
            let positions: Vec<Vec3> = match m.mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
                Some(VertexAttributeValues::Float32x3(p)) => {
                    p.iter().map(|&p| Vec3::from(p)).collect()
                }
                _ => continue,
            };
            let uvs = match m.mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
                Some(VertexAttributeValues::Float32x2(u)) => u.clone(),
                _ => vec![[0.0; 2]; positions.len()],
            };
            let indices = match m.mesh.indices() {
                Some(i) => i.iter().map(|i| i as u32).collect(),
                None => continue,
            };
            let texture = m
                .diffuse
                .as_deref()
                .and_then(|name| self.texture_named(name, images));
            parts.push(Part {
                mesh: meshes.add(m.mesh),
                texture,
                positions,
                uvs,
                indices,
            });
        }
        let parts = Some(Arc::new(parts));
        self.models.insert(id, parts.clone());
        parts
    }
}

// ---------------------------------------------------------------------------------------------
// Curves and param readers.

#[derive(Clone, Debug, Default)]
struct Curve {
    keys: Vec<(f32, f32)>,
    step: bool,
    cyclic: bool,
}

impl Curve {
    fn constant(v: f32) -> Self {
        Self {
            keys: vec![(0.0, v)],
            ..default()
        }
    }

    fn at(&self, t: f32) -> f32 {
        sample(&self.keys, t, self.step, self.cyclic, |a, b, f| {
            a + (b - a) * f
        })
        .unwrap_or(0.0)
    }
}

#[derive(Clone, Debug)]
struct ColorCurve {
    keys: Vec<(f32, Vec4)>,
    step: bool,
    cyclic: bool,
}

impl ColorCurve {
    fn at(&self, t: f32) -> Vec4 {
        sample(&self.keys, t, self.step, self.cyclic, |a, b, f| {
            a.lerp(b, f)
        })
        .unwrap_or(Vec4::ONE)
    }
}

/// Keyframes at seconds `keys[i].0`; `cyclic` repeats them over the last key's time.
fn sample<T: Copy>(
    keys: &[(f32, T)],
    t: f32,
    step: bool,
    cyclic: bool,
    lerp: impl Fn(T, T, f32) -> T,
) -> Option<T> {
    let (first, last) = (keys.first()?, keys.last()?);
    let t = if cyclic && last.0 > 0.0 {
        t.rem_euclid(last.0)
    } else {
        t
    };
    if t <= first.0 {
        return Some(first.1);
    }
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t < b.0 {
            return Some(if step || b.0 <= a.0 {
                a.1
            } else {
                lerp(a.1, b.1, (t - a.0) / (b.0 - a.0))
            });
        }
    }
    Some(last.1)
}

/// Sequence modes follow the runtime class order Step, Cyclic, Linear, LinearCyclic, Cubic,
/// CubicCyclic: float kinds 9..14, colour kinds 17..22; int kinds 3 step, 5 linear, 6 cyclic.
fn float_curve(p: Option<&Param>) -> Curve {
    let Some(p) = p else {
        return Curve::constant(0.0);
    };
    match &p.value {
        Value::Float(v) | Value::Tick(v) => Curve::constant(*v),
        Value::Int(v) => Curve::constant(*v as f32),
        Value::FloatKeys(k) => Curve {
            keys: k.clone(),
            step: matches!(p.kind, 9 | 10),
            cyclic: matches!(p.kind, 10 | 12),
        },
        Value::CubicKeys(k) => Curve {
            keys: k.iter().map(|(t, v)| (*t, v[0])).collect(),
            step: false,
            cyclic: p.kind == 14,
        },
        Value::IntKeys(k) => Curve {
            keys: k.iter().map(|(t, v)| (*t, *v as f32)).collect(),
            step: p.kind == 3,
            cyclic: p.kind == 6,
        },
        Value::FloatPair(a, b) | Value::TickPair(a, b) => Curve::constant((a + b) / 2.0),
        Value::Scaled(inner, _) => float_curve(Some(inner)),
        _ => Curve::constant(0.0),
    }
}

fn color_curve(p: Option<&Param>) -> ColorCurve {
    let one = |keys| ColorCurve {
        keys,
        step: false,
        cyclic: false,
    };
    let Some(p) = p else {
        return one(vec![(0.0, Vec4::ONE)]);
    };
    match &p.value {
        Value::Color(c) => one(vec![(0.0, Vec4::from(*c))]),
        Value::ColorKeys(k) => ColorCurve {
            keys: k.iter().map(|(t, c)| (*t, Vec4::from(*c))).collect(),
            step: matches!(p.kind, 17 | 18),
            cyclic: matches!(p.kind, 18 | 20),
        },
        Value::CubicColorKeys(k) => ColorCurve {
            keys: k.iter().map(|(t, c)| (*t, Vec4::from(c[0]))).collect(),
            step: false,
            cyclic: p.kind == 22,
        },
        Value::ScaledColor(inner, _) => color_curve(Some(inner)),
        _ => one(vec![(0.0, Vec4::ONE)]),
    }
}

/// A value sampled once per particle: a curve at the emitter's age times `1 ± spread`, or a
/// uniform range.
#[derive(Clone, Debug)]
enum Spread {
    Curve(Curve, f32),
    Range(f32, f32),
}

impl Spread {
    fn read(p: Option<&Param>) -> Self {
        match p.map(|p| &p.value) {
            Some(Value::Scaled(inner, s)) => Spread::Curve(float_curve(Some(inner)), *s),
            Some(Value::FloatPair(a, b) | Value::TickPair(a, b)) => Spread::Range(*a, *b),
            None => Spread::Curve(Curve::constant(1.0), 0.0),
            _ => Spread::Curve(float_curve(p), 0.0),
        }
    }

    fn sample(&self, t: f32, rng: &mut Rng) -> f32 {
        match self {
            Spread::Curve(c, s) => c.at(t) * (1.0 + s * rng.signed()),
            Spread::Range(a, b) => rng.range((*a, *b)),
        }
    }
}

fn param(l: &ParamList, i: usize) -> Option<&Param> {
    l.params.get(i)
}

fn tick(l: &ParamList, i: usize) -> f32 {
    match l.get(i) {
        Some(Value::Tick(t) | Value::Float(t) | Value::TickPair(t, _) | Value::FloatPair(t, _)) => {
            *t
        }
        Some(Value::Int(i)) => *i as f32,
        _ => 0.0,
    }
}

fn int(l: &ParamList, i: usize) -> i32 {
    match l.get(i) {
        Some(Value::Int(v) | Value::Id(v)) => *v,
        Some(Value::IntKeys(k)) => k.first().map_or(0, |k| k.1),
        _ => 0,
    }
}

fn pair(l: &ParamList, i: usize) -> (f32, f32) {
    match l.get(i) {
        Some(Value::FloatPair(a, b) | Value::TickPair(a, b)) => (*a, *b),
        Some(Value::IntPair(a, b)) => (*a as f32, *b as f32),
        Some(Value::Float(v) | Value::Tick(v)) => (*v, *v),
        _ => (0.0, 0.0),
    }
}

fn tint(l: &ParamList, i: usize) -> (Vec4, Vec4) {
    match l.get(i) {
        Some(Value::ColorPair(a, b)) => (Vec4::from(*a), Vec4::from(*b)),
        Some(Value::Color(c)) => (Vec4::from(*c), Vec4::from(*c)),
        _ => (Vec4::ONE, Vec4::ONE),
    }
}

fn action(l: &ParamList, i: usize) -> Option<(i32, &ParamList)> {
    match l.get(i) {
        Some(Value::Node(id, list)) if *id != 0 => Some((*id, list)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Definitions read from the effect tree.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Blend {
    Add,
    Alpha,
}

/// Blend slot values: 2 on smoke (alpha), 4 on flames and flashes (additive). Others are drawn
/// additive.
fn blend(v: i32) -> Blend {
    if v == 2 {
        Blend::Alpha
    } else {
        Blend::Add
    }
}

/// Node transform action 36 (offset xyz, rotation xyz in degrees, then their random ranges) or
/// 35 (offset, rotation).
#[derive(Clone, Debug, Default)]
struct Xf {
    offset: Vec3,
    rotation: Vec3,
    offset_rand: Vec3,
    rotation_rand: Vec3,
}

impl Xf {
    fn read(a: Option<(i32, &ParamList)>) -> Self {
        let Some((id, l)) = a else {
            return Self::default();
        };
        let v3 = |i| Vec3::new(tick(l, i), tick(l, i + 1), tick(l, i + 2));
        match id {
            36 => Self {
                offset: v3(0),
                rotation: v3(3),
                offset_rand: v3(6),
                rotation_rand: v3(9),
            },
            35 => Self {
                offset: v3(0),
                rotation: v3(3),
                ..default()
            },
            _ => Self::default(),
        }
    }

    fn sample(&self, rng: &mut Rng) -> Affine3A {
        let mut r3 = || Vec3::new(rng.signed(), rng.signed(), rng.signed());
        let o = self.offset + self.offset_rand * r3();
        let r = (self.rotation + self.rotation_rand * r3()) * (PI / 180.0);
        Affine3A::from_rotation_translation(
            Quat::from_euler(EulerRot::YXZ, -r.y, r.x, -r.z),
            mirror(o),
        )
    }
}

/// Flipbook: `cols` columns, `total` frames row by row, frame index from `key` (wrapping).
#[derive(Clone, Debug, Default)]
struct Frames {
    cols: u32,
    total: u32,
    key: Curve,
}

impl Frames {
    fn new(cols: i32, total: i32, key: Option<&Param>) -> Self {
        Self {
            cols: cols.max(1) as u32,
            total: total.max(1) as u32,
            key: float_curve(key),
        }
    }

    fn rect(&self, t: f32, start: f32) -> (Vec2, Vec2) {
        if self.total <= 1 {
            return (Vec2::ZERO, Vec2::ONE);
        }
        let rows = self.total.div_ceil(self.cols);
        let f = ((self.key.at(t) + start).floor().max(0.0) as u32) % self.total;
        let scale = Vec2::new(1.0 / self.cols as f32, 1.0 / rows as f32);
        (
            Vec2::new((f % self.cols) as f32, (f / self.cols) as f32) * scale,
            scale,
        )
    }
}

/// What a static node draws.
#[derive(Clone, Debug)]
enum Look {
    Billboard {
        texture: i32,
        blend: Blend,
        width: Curve,
        height: Curve,
        color: ColorCurve,
        frames: Frames,
        rotation: (f32, f32),
        spin: Curve,
    },
    Model {
        model: i32,
        blend: Blend,
        scale: [Curve; 3],
        /// Slot 7 = 1: Y and Z take the X curve (def `+0xcc` in the mode-2 draw `0x82c711d8`).
        uniform: bool,
        color: ColorCurve,
        frames: Frames,
    },
    Light {
        color: ColorCurve,
        radius: Curve,
    },
    /// Action 43 heat haze: a camera-facing quad that refracts the scene through `texture`
    /// (the normal map). Width and height are metres.
    Haze {
        #[allow(dead_code)]
        texture: i32,
        width: Curve,
        height: Curve,
        spin: Curve,
    },
    /// Action 107 (`0x6b`): `FXParticleAppearance_RadialBlur` (360 `0x83197ba0` -> `0x831971b0`
    /// -> `0x835b3658`, `FXParticleAppearance_RadialBlur.cpp`). Post-process radial blur ring,
    /// not a world-space additive billboard.
    RadialBlur,
    /// Action 40 ribbon trail: a camera-facing strip left by the parent's world position. Slots
    /// follow `sheets/ffx_actions.csv` row `40` (f0000134: texture s1847, 0.06 m wide, sample
    /// every 1/60 s, 1 s life, 16 segments, white alpha 0.235).
    Ribbon {
        texture: i32,
        blend: Blend,
        width: Curve,
        interval: f32,
        life: f32,
        segments: usize,
        /// Seconds of sample age the segment alpha fades over (slot 15, `def+0x1c`).
        fade: f32,
        color: ColorCurve,
    },
}

fn look(id: i32, l: &ParamList) -> Option<Look> {
    let p = |i| param(l, i);
    Some(match id {
        59 => Look::Billboard {
            texture: int(l, 0),
            blend: blend(int(l, 7)),
            width: float_curve(p(1)),
            height: float_curve(p(2)),
            color: color_curve(p(8)),
            frames: Frames::new(int(l, 9), int(l, 10), p(11)),
            rotation: pair(l, 21),
            spin: float_curve(p(22)),
        },
        107 => Look::RadialBlur,
        61 => Look::Model {
            model: int(l, 0),
            blend: blend(int(l, 8)),
            scale: [float_curve(p(1)), float_curve(p(2)), float_curve(p(3))],
            uniform: int(l, 7) == 1,
            color: color_curve(p(9)),
            frames: Frames::new(int(l, 16), int(l, 17), p(15)),
        },
        24 => Look::Light {
            color: color_curve(p(0)),
            radius: float_curve(p(2)),
        },
        // f0000300 node 1: 1.2 m, normal map 1206, 1.35 m out the nozzle. Refraction strength
        // is not a traced slot.
        43 => Look::Haze {
            texture: int(l, 14),
            width: float_curve(p(3)),
            height: float_curve(p(4)),
            spin: float_curve(p(13)),
        },
        // f0000134 child node: s1847 cloud texture, 0.06 m wide, sample every tick, 1 s trail
        // life, 16 segments, white alpha 0.235. The camera-facing strip trails the parent
        // effect point (a core/arm dummy during the air glide).
        40 => Look::Ribbon {
            texture: int(l, 0),
            blend: blend(int(l, 6)),
            width: float_curve(p(7)),
            interval: tick(l, 9).max(1.0 / 240.0),
            life: tick(l, 10).max(0.0),
            segments: int(l, 12).max(1) as usize,
            // Slot 15 is the age span the segment alpha fades over (`def+0x1c`, 0x83197e90:
            // `1 - clamp((cur - prev) / def[0x1c])`). f0000134 stores 0.5 s.
            fade: tick(l, 15),
            color: color_curve(p(16)),
        },
        _ => return None,
    })
}

/// Build the action-40 Tracer strip from time-stamped history samples.
///
/// The 360 per-segment update (`0x83197e90`) writes one vertex pair per `segments` sample and
/// evaluates width, colour and alpha from *that sample's* age (`r5+0x10`), not from the node's
/// current age; the alpha fades over `def[0x1c]` (`slot 15`). The strip UVs are
/// `(u across width, v along length)` (`0x83110dd0`: both side vertices share `V`). Vertices are
/// relative to `head`, which is the current effect-point world position.
#[allow(clippy::too_many_arguments)]
fn ribbon_batch(
    pts: &[(f32, Vec3)],
    head: Vec3,
    t: f32,
    life: f32,
    fade_span: f32,
    segments: usize,
    width: &Curve,
    color: &ColorCurve,
    k: f32,
    camera: Vec3,
) -> Batch {
    let mut b = Batch::default();
    let n = pts.len();
    if n < 2 {
        return b;
    }
    // Keep at most `segments` spans by walking the history at an even stride.
    let count = (segments.max(1) + 1).min(n);
    let pick = |i: usize| -> (f32, Vec3) {
        if count == n {
            pts[i]
        } else {
            pts[(i * (n - 1)) / (count - 1)]
        }
    };
    for i in 0..count {
        let (st, p) = pick(i);
        let (_, prev) = pick(i.saturating_sub(1));
        let (_, next) = pick((i + 1).min(count - 1));
        let age = (t - st).clamp(0.0, life);
        let mut side = (next - prev).cross(camera - p);
        if side.length_squared() < 1e-8 {
            side = (next - prev).cross(Vec3::Y);
        }
        if side.length_squared() < 1e-8 {
            side = Vec3::X;
        }
        let half = (width.at(age) * k).max(0.0) * 0.5;
        let side = side.normalize() * half;
        let v = if life > 0.0 {
            (age / life).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let c = color.at(age);
        let a = if fade_span > 0.0 {
            (1.0 - age / fade_span).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let col = [c.x, c.y, c.z, c.w * a];
        b.pos.push((p - head - side).to_array());
        b.pos.push((p - head + side).to_array());
        b.uv.push([0.0, v]);
        b.uv.push([1.0, v]);
        b.color.push(vertex_color(col));
        b.color.push(vertex_color(col));
    }
    for i in 0..count - 1 {
        let a = (i * 2) as u32;
        b.index.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    b
}

#[derive(Clone, Debug)]
enum Shape {
    Quad { texture: Option<i32>, blend: Blend },
    Model(i32),
}

/// How a cluster's particles look over their age.
#[derive(Clone, Debug)]
struct ParticleLook {
    shape: Shape,
    life: (f32, f32),
    size: [Curve; 3],
    /// Metres per size unit.
    scale: f32,
    color: ColorCurve,
    tint: (Vec4, Vec4),
    rotation: (f32, f32),
    spin: Curve,
    spin_range: (f32, f32),
    frames: Frames,
    start_frame: (f32, f32),
    /// Seconds of travel a streak covers; zero draws a billboard.
    streak: (f32, f32),
    /// Particles stay in the emitting node's frame instead of the world.
    follow: bool,
}

fn particle_look(id: i32, l: &ParamList) -> Option<ParticleLook> {
    let p = |i| param(l, i);
    Some(match id {
        71 => ParticleLook {
            shape: Shape::Quad {
                texture: Some(int(l, 1)),
                blend: blend(int(l, 5)),
            },
            life: pair(l, 3),
            size: [float_curve(p(10)), float_curve(p(11)), Curve::constant(1.0)],
            scale: 1.0,
            color: color_curve(p(12)),
            tint: tint(l, 13),
            rotation: pair(l, 16),
            spin: float_curve(p(21)),
            spin_range: pair(l, 22),
            frames: Frames::new(int(l, 6), int(l, 7), p(9)),
            start_frame: pair(l, 8),
            streak: (0.0, 0.0),
            follow: false,
        },
        108 => ParticleLook {
            shape: Shape::Model(int(l, 1)),
            life: pair(l, 2),
            size: [float_curve(p(3)), float_curve(p(4)), float_curve(p(5))],
            scale: 1.0,
            color: color_curve(p(6)),
            tint: tint(l, 7),
            rotation: pair(l, 16),
            spin: float_curve(p(12)),
            spin_range: pair(l, 13),
            frames: Frames::new(int(l, 17), int(l, 18), p(19)),
            start_frame: pair(l, 20),
            streak: (0.0, 0.0),
            // Slot 21 is 1 on the main-booster flames (f0000300) and 0 on ground-dust models
            // (f0001406). 1 keeps the particle on the emitter.
            follow: int(l, 21) != 0,
        },
        82 => ParticleLook {
            shape: Shape::Quad {
                texture: None,
                blend: blend(int(l, 2)),
            },
            life: pair(l, 1),
            size: [float_curve(p(3)), float_curve(p(3)), Curve::constant(1.0)],
            scale: POINT_SPRITE_METRES,
            color: color_curve(p(9)),
            tint: tint(l, 4),
            rotation: (0.0, 0.0),
            spin: Curve::constant(0.0),
            spin_range: (0.0, 0.0),
            frames: Frames::default(),
            start_frame: (0.0, 0.0),
            streak: (POINT_SPRITE_STREAK, POINT_SPRITE_STREAK),
            follow: false,
        },
        _ => return None,
    })
}

/// Spatial emitter shape for a particle cluster (`FXClusterEmitter_*`).
#[derive(Clone, Debug)]
enum EmitterShape {
    /// Actions `28` (`FXClusterEmitter_Cone`, `0x831501b0`), `117`, `4`: cone about +Z of full
    /// apex `angle` degrees (360 = every direction).
    Cone,
    /// Action `30` (`FXClusterEmitter_Circle`, `0x8314ea40` -> `0x8314e558`): horizontal XZ ring
    /// of `radius` metres (slot `[1]`) emitting radially outward at polar angle `angle` (slot `[2]`,
    /// 90° = pure horizontal XZ). Used by walk/landing dust (`f0001404`, `f0001405`).
    Circle { radius: Curve },
    /// Action `31` (`FXClusterEmitter_Sphere`, `0x8314d218` -> `0x8314cd58`): sphere of `radius`
    /// metres (slot `[1]`) emitting outward over full angle `angle` (slot `[2]`). Used by
    /// wall-kick dust (`f0000116`).
    Sphere { radius: Curve },
}

/// Where particles start: shape, spread angle, speed, size multipliers, and emitter tint curve.
#[derive(Clone, Debug)]
struct Emitter {
    shape: EmitterShape,
    angle: f32,
    speed: Spread,
    size: [Spread; 3],
    color: ColorCurve,
}

impl Emitter {
    fn read(a: Option<(i32, &ParamList)>) -> Self {
        let p = |l, i| param(l, i);
        match a {
            Some((28, l)) => Self {
                shape: EmitterShape::Cone,
                angle: float_curve(p(l, 1)).at(0.0),
                speed: Spread::read(p(l, 3)),
                size: [
                    Spread::read(p(l, 4)),
                    Spread::read(p(l, 5)),
                    Spread::read(p(l, 8)),
                ],
                color: color_curve(p(l, 9)),
            },
            Some((30, l)) => Self {
                shape: EmitterShape::Circle {
                    radius: float_curve(p(l, 1)),
                },
                angle: float_curve(p(l, 2)).at(0.0),
                speed: Spread::read(p(l, 4)),
                size: [
                    Spread::read(p(l, 5)),
                    Spread::read(p(l, 6)),
                    Spread::read(p(l, 10)),
                ],
                color: color_curve(p(l, 12)),
            },
            Some((31, l)) => Self {
                shape: EmitterShape::Sphere {
                    radius: float_curve(p(l, 1)),
                },
                angle: float_curve(p(l, 2)).at(0.0),
                speed: Spread::read(p(l, 4)),
                size: [
                    Spread::read(p(l, 5)),
                    Spread::read(p(l, 6)),
                    Spread::read(p(l, 9)),
                ],
                color: color_curve(p(l, 11)),
            },
            Some((117, l)) => Self {
                shape: EmitterShape::Cone,
                angle: float_curve(p(l, 4)).at(0.0),
                speed: Spread::read(p(l, 6)),
                size: [
                    Spread::read(p(l, 7)),
                    Spread::read(p(l, 8)),
                    Spread::read(p(l, 11)),
                ],
                color: color_curve(p(l, 14)),
            },
            Some((4, l)) => Self {
                angle: float_curve(p(l, 1)).at(0.0),
                ..Self::read(None)
            },
            _ => {
                let one = || Spread::Curve(Curve::constant(1.0), 0.0);
                Self {
                    shape: EmitterShape::Cone,
                    angle: 0.0,
                    speed: Spread::Curve(Curve::constant(0.0), 0.0),
                    size: [one(), one(), one()],
                    color: color_curve(None),
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct ClusterDef {
    /// Particles per emission over the node's age.
    count: Curve,
    /// Maximum emissions (`< 0` = unlimited; `2023` `[7]` `0x83190afc`, `2032`/`2034` `[7]` `0x83190d28`).
    emissions: i32,
    /// Seconds between emissions; zero emits once.
    interval: f32,
    /// Seconds the node emits; negative while it lives, `>= 0` finite (`0x83190ae8` / `0x83190d20`).
    emit_for: f32,
    max: usize,
    emitter: Emitter,
    look: ParticleLook,
    /// Downward acceleration (m/s², negative rises) over particle age, times a per-particle range.
    gravity: (Curve, (f32, f32)),
    /// Speed change per second over particle age.
    drag: Curve,
}

/// Node motion: action 1 moves along +Z at a random speed plus acceleration keys; action 34
/// spins about xyz (radians per second, each times a random range).
#[derive(Clone, Debug, Default)]
struct Motion {
    speed: (f32, f32),
    accel: Curve,
    spin: [(Curve, (f32, f32)); 3],
}

impl Motion {
    fn read(a: Option<(i32, &ParamList)>) -> Self {
        match a {
            Some((1, l)) => Self {
                speed: pair(l, 0),
                accel: float_curve(param(l, 1)),
                ..default()
            },
            Some((34, l)) => Self {
                spin: [0, 2, 4].map(|i| (float_curve(param(l, i)), pair(l, i + 1))),
                ..default()
            },
            _ => Self::default(),
        }
    }
}

#[derive(Clone, Debug)]
enum Kind {
    Static,
    /// Spawns `count` copies of `child` every `interval` (once when zero), `emissions` times
    /// (negative never stops), each turned into a random direction of the cone `angle`.
    Spawner {
        count: u32,
        emissions: i32,
        interval: f32,
        angle: f32,
        child: Vec<Arc<NodeDef>>,
    },
    Cluster(Arc<ClusterDef>),
}

#[derive(Clone, Debug)]
struct NodeDef {
    /// Seconds; negative never ends.
    life: f32,
    delay: f32,
    /// Whether `stop` cuts a finite-life node short (`2101` `[1]` `0x83190db8`, `2020`/`2024` `[1]`
    /// `0x83190964`, `2031`/`2035` `[11]` `0x83190bd8`, `2023` `[2]` `0x83190a7c`, `2032`/`2034`
    /// `[12]` `0x83190cd0`). Infinite-life nodes (`life < 0`) always stop on `stop`.
    stop_on_stop: bool,
    xf: Xf,
    look: Option<Look>,
    motion: Motion,
    children: Vec<Arc<NodeDef>>,
    kind: Kind,
}

/// The param-37 nodes of a list (an action 14/79 container's params).
fn nodes(l: &ParamList) -> Vec<Arc<NodeDef>> {
    l.params
        .iter()
        .filter_map(|p| match &p.value {
            Value::Node(id, list) if p.kind == 37 && *id != 0 => node(*id, list).map(Arc::new),
            _ => None,
        })
        .collect()
}

fn children(l: &ParamList, i: usize) -> Vec<Arc<NodeDef>> {
    match (param(l, i).map(|p| p.kind), l.get(i)) {
        (Some(37), Some(Value::Node(id, list))) if *id != 0 => {
            node(*id, list).map(Arc::new).into_iter().collect()
        }
        (_, Some(Value::Node(14 | 79, list))) => nodes(list),
        _ => Vec::new(),
    }
}

fn node(id: i32, l: &ParamList) -> Option<NodeDef> {
    let p = |i| param(l, i);
    let mut n = NodeDef {
        life: -1.0,
        delay: 0.0,
        stop_on_stop: false,
        xf: Xf::default(),
        look: None,
        motion: Motion::default(),
        children: Vec::new(),
        kind: Kind::Static,
    };
    match id {
        2101 | 2102 => {
            (n.life, n.delay) = (tick(l, 0), tick(l, 3));
            n.stop_on_stop = int(l, 1) != 0;
            match action(l, 4) {
                Some((10003, a)) => n.kind = Kind::Cluster(Arc::new(sparks(a))),
                Some((a, al)) => n.look = look(a, al),
                None => {}
            }
            n.xf = Xf::read(action(l, 5));
            n.motion = Motion::read(action(l, 6));
            n.children = children(l, 8);
        }
        2020 | 2024 => {
            (n.life, n.delay) = (tick(l, 0), tick(l, 4));
            n.stop_on_stop = int(l, 1) != 0;
            let angle = Emitter::read(action(l, 9)).angle;
            // f0000120's 2020 ([5] 3, [6] 1/6 s, [7] 1) keeps exactly three s1020 copies per
            // nozzle, born about 0.18 s apart, and spawns no more (`private/xenia/glide_scale.jsonl`).
            n.kind = Kind::Spawner {
                count: int(l, 7).max(1) as u32,
                emissions: int(l, 5),
                interval: tick(l, 6),
                angle,
                child: children(l, 8),
            };
            n.xf = Xf::read(
                action(l, 10)
                    .filter(|a| matches!(a.0, 35 | 36))
                    .or(action(l, 11)),
            );
        }
        2031 | 2035 => {
            // TickPair spawner (`0x83190b70`): e.g. f0001404 walk-dust rock debris.
            (n.life, n.delay) = (pair(l, 9).0, tick(l, 7));
            n.stop_on_stop = int(l, 11) != 0;
            let angle = Emitter::read(action(l, 1)).angle;
            let emis = pair(l, 6);
            n.kind = Kind::Spawner {
                count: int(l, 10).max(1) as u32,
                emissions: ((emis.0 + emis.1) * 0.5).round() as i32,
                interval: pair(l, 8).0,
                angle,
                child: children(l, 0),
            };
            n.xf = Xf::read(
                action(l, 3)
                    .filter(|a| matches!(a.0, 35 | 36))
                    .or(action(l, 2)),
            );
        }
        2023 => {
            let look = action(l, 11).and_then(|(a, al)| particle_look(a, al))?;
            (n.life, n.delay) = (tick(l, 0), tick(l, 5));
            n.stop_on_stop = int(l, 2) != 0 || int(l, 9) != 0;
            n.kind = Kind::Cluster(Arc::new(ClusterDef {
                count: float_curve(p(6)),
                emissions: int(l, 7),
                interval: tick(l, 8),
                emit_for: tick(l, 1),
                max: cluster_max(int(l, 10)),
                emitter: Emitter::read(action(l, 12)),
                look,
                gravity: gravity(action(l, 13)),
                drag: action(l, 13).map_or_else(Curve::default, |(_, m)| float_curve(param(m, 1))),
            }));
            n.xf = Xf::read(action(l, 14));
            n.motion = Motion::read(action(l, 15));
            n.children = children(l, 17);
        }
        2032 | 2034 => {
            let look = action(l, 0).and_then(|(a, al)| particle_look(a, al))?;
            (n.life, n.delay) = (pair(l, 16).0, tick(l, 8));
            n.stop_on_stop = int(l, 12) != 0 || int(l, 13) != 0;
            n.kind = Kind::Cluster(Arc::new(ClusterDef {
                count: float_curve(p(10)),
                emissions: pair(l, 7).0.round() as i32,
                interval: pair(l, 9).0,
                emit_for: pair(l, 11).0,
                max: cluster_max(int(l, 14)),
                emitter: Emitter::read(action(l, 1)),
                look,
                gravity: gravity(action(l, 2)),
                drag: action(l, 2).map_or_else(Curve::default, |(_, m)| float_curve(param(m, 1))),
            }));
            n.xf = Xf::read(action(l, 3));
            n.motion = Motion::read(action(l, 4));
            n.children = children(l, 17);
        }
        _ => return None,
    }
    Some(n)
}

/// Live-particle cap; -1 has none (f0000131's glide jet, one s1011 every 1/12 s for 1 s).
fn cluster_max(v: i32) -> usize {
    if v < 0 { usize::MAX } else { v.max(1) as usize }
}

/// Cluster motion action 55: [0] gravity keys, [1] drag keys, [2] gravity range.
fn gravity(a: Option<(i32, &ParamList)>) -> (Curve, (f32, f32)) {
    match a {
        Some((55, l)) => (
            float_curve(param(l, 0)),
            match l.get(2) {
                Some(Value::FloatPair(a, b)) => (*a, *b),
                _ => (1.0, 1.0),
            },
        ),
        _ => (Curve::constant(0.0), (1.0, 1.0)),
    }
}

/// Action 10003, a self-contained spark burst drawn as streaks: [0] texture, [1] life, [2] blend,
/// [3] width, [4] streak seconds, [5] max, [9] count, [12] cone angle, [14] speed range,
/// [16] gravity range.
fn sparks(l: &ParamList) -> ClusterDef {
    let width = float_curve(param(l, 3));
    ClusterDef {
        count: Curve::constant(int(l, 9) as f32),
        emissions: 1,
        interval: 0.0,
        emit_for: -1.0,
        max: int(l, 5).max(int(l, 9)).max(1) as usize,
        emitter: Emitter {
            shape: EmitterShape::Cone,
            angle: float_curve(param(l, 12)).at(0.0),
            speed: Spread::Range(pair(l, 14).0, pair(l, 14).1),
            size: [
                Spread::read(param(l, 3)).with_curve(1.0),
                Spread::Curve(Curve::constant(1.0), 0.0),
                Spread::Curve(Curve::constant(1.0), 0.0),
            ],
            color: color_curve(None),
        },
        look: ParticleLook {
            shape: Shape::Quad {
                texture: Some(int(l, 0)),
                blend: blend(int(l, 2)),
            },
            life: pair(l, 1),
            size: [width.clone(), width, Curve::constant(1.0)],
            scale: 1.0,
            color: color_curve(param(l, 6)),
            tint: tint(l, 7),
            rotation: (0.0, 0.0),
            spin: Curve::constant(0.0),
            spin_range: (0.0, 0.0),
            frames: Frames::default(),
            start_frame: (0.0, 0.0),
            streak: pair(l, 4),
            follow: false,
        },
        gravity: (Curve::constant(1.0), pair(l, 16)),
        drag: Curve::constant(0.0),
    }
}

impl Spread {
    /// The same spread around a constant `v`.
    fn with_curve(self, v: f32) -> Self {
        match self {
            Spread::Curve(_, s) => Spread::Curve(Curve::constant(v), s),
            r => r,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Runtime.

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / 16_777_216.0
    }

    fn signed(&mut self) -> f32 {
        self.next() * 2.0 - 1.0
    }

    fn range(&mut self, (a, b): (f32, f32)) -> f32 {
        a + (b - a) * self.next()
    }

    /// A direction within the cone of full apex `angle` degrees about +Z.
    fn cone(&mut self, angle: f32) -> Vec3 {
        let half = (angle.abs() * 0.5).to_radians().min(PI);
        let cos = 1.0 - self.next() * (1.0 - half.cos());
        let sin = (1.0 - cos * cos).max(0.0).sqrt();
        let phi = self.next() * TAU;
        Vec3::new(sin * phi.cos(), sin * phi.sin(), cos)
    }
}

struct Run {
    nodes: Vec<NodeRt>,
    rng: Rng,
}

/// Entities drawn for one node or cluster.
struct Visual {
    entities: Vec<Entity>,
    material: Handle<StandardMaterial>,
    /// True when `material` comes from [`Library::shared_materials`] and must not be mutated.
    shared_material: bool,
    mesh: Option<Handle<Mesh>>,
}

/// Marks an entity drawn for the [`Sfx`] entity `.0`.
#[derive(Component)]
struct SfxVisual(Entity);

struct Particle {
    pos: Vec3,
    vel: Vec3,
    dir: Quat,
    age: f32,
    life: f32,
    size: Vec3,
    tint: Vec4,
    angle: f32,
    spin: f32,
    frame: f32,
    streak: f32,
    gravity: f32,
    /// Position and `dir` are in world space. Following particles store both in the emitter frame.
    world: bool,
}

struct NodeRt {
    def: Arc<NodeDef>,
    /// Seconds; negative never ends. Spawned copies with an infinite template live for one
    /// pass of their colour and scale keys, otherwise they pile up on the last key.
    life: f32,
    age: f32,
    base: Affine3A,
    speed: f32,
    travel: f32,
    spin: Vec3,
    /// Billboard roll at birth, radians.
    roll: f32,
    /// World matrix of the spawner at birth. A 2020/2024 copy keeps that frame, so a
    /// glide flame stays where the nozzle was instead of riding it onto the last colour key.
    anchor: Option<Affine3A>,
    visual: Option<Visual>,
    children: Vec<NodeRt>,
    /// Seconds to the next emission or spawn.
    emit: f32,
    /// Spawner emissions so far.
    emissions: i32,
    particles: Vec<Particle>,
    batch: Option<Visual>,
    /// Action-40 ribbon history: `(age, world position)` samples, oldest first.
    ribbon: Vec<(f32, Vec3)>,
}

/// Per-frame context while stepping an effect.
struct Cx<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    lib: &'a mut Library,
    meshes: &'a mut Assets<Mesh>,
    materials: &'a mut Assets<StandardMaterial>,
    images: &'a mut Assets<Image>,
    camera: Vec3,
    camera_rot: Quat,
    owner: Entity,
    /// Transforms and lights to write to existing visuals once the step is done.
    moves: Vec<(Entity, Transform, Option<PointLight>)>,
}

impl Cx<'_, '_, '_> {
    /// Returns a cached, immutable [`StandardMaterial`] shared across all particle clusters and
    /// static effect visuals with matching `(texture, blend, color)`.
    fn shared_material(
        &mut self,
        texture: Option<Handle<Image>>,
        blend: Blend,
        color: Vec4,
    ) -> Handle<StandardMaterial> {
        cached_material(
            &mut self.lib.shared_materials,
            texture,
            blend,
            color,
            self.materials,
        )
    }

    /// Allocates a unique mutable [`StandardMaterial`] for nodes whose `base_color` or
    /// `uv_transform` animates per frame.
    fn material(
        &mut self,
        texture: Option<Handle<Image>>,
        blend: Blend,
    ) -> Handle<StandardMaterial> {
        self.materials.add(StandardMaterial {
            base_color_texture: texture,
            unlit: true,
            alpha_mode: match blend {
                Blend::Alpha => AlphaMode::Blend,
                Blend::Add => AlphaMode::Add,
            },
            double_sided: true,
            cull_mode: None,
            fog_enabled: false,
            ..default()
        })
    }

    fn spawn(&mut self, bundle: impl Bundle, transform: Transform) -> Entity {
        self.commands
            .spawn((
                bundle,
                transform,
                GlobalTransform::from(transform),
                NoFrustumCulling,
                NotShadowCaster,
                NotShadowReceiver,
                SfxVisual(self.owner),
            ))
            .id()
    }

    fn despawn(&mut self, v: Visual) {
        for e in v.entities {
            self.commands.entity(e).try_despawn();
        }
    }
}

/// Uniform scale of an effect's world transform (its [`Sfx`] entity's scale).
fn scale_of(world: Affine3A) -> f32 {
    world.matrix3.x_axis.length()
}

/// True when every UV already lies inside one flipbook cell, so the frame index must
/// translate the cell instead of scaling a 0–1 quad into it.
fn uvs_in_cell(uvs: &[[f32; 2]], cell: Vec2) -> bool {
    !uvs.is_empty()
        && uvs
            .iter()
            .all(|[u, v]| *u <= cell.x + 0.02 && *v <= cell.y + 0.02)
}

fn srgba(c: Vec4) -> Color {
    Color::srgba(c.x, c.y, c.z, c.w)
}

/// `AlphaMode::Add` shares Bevy's premultiplied pipeline, and the PBR shader then does the
/// additive case itself: `src.rgb * src.a` with output alpha 0, so the blend is `src + dst`.
/// Scaling RGB by alpha before that squares it and knocks the blue nozzle (alpha about 0.8)
/// down while an alpha-1 orange is left at full strength.
fn vertex_color(c: [f32; 4]) -> [f32; 4] {
    linear(c)
}

fn curve_end(c: &Curve) -> f32 {
    c.keys.last().map(|k| k.0).unwrap_or(0.0)
}

fn color_end(c: &ColorCurve) -> f32 {
    c.keys.last().map(|k| k.0).unwrap_or(0.0)
}

/// How long a spawned node's authored keys run. An infinite template would otherwise
/// sit on its last key forever.
fn look_span(def: &NodeDef) -> f32 {
    let Some(look) = &def.look else {
        return 0.0;
    };
    match look {
        Look::Model {
            scale, color, frames, ..
        } => curve_end(&scale[0])
            .max(curve_end(&scale[1]))
            .max(curve_end(&scale[2]))
            .max(color_end(color))
            .max(curve_end(&frames.key)),
        Look::Billboard {
            width,
            height,
            color,
            frames,
            spin,
            ..
        } => curve_end(width)
            .max(curve_end(height))
            .max(color_end(color))
            .max(curve_end(&frames.key))
            .max(curve_end(spin)),
        Look::Light { color, radius } => color_end(color).max(curve_end(radius)),
        Look::Haze {
            width, height, spin, ..
        } => curve_end(width)
            .max(curve_end(height))
            .max(curve_end(spin)),
        Look::RadialBlur => 0.0,
        Look::Ribbon { .. } => 0.0,
    }
}

impl NodeRt {
    fn new(def: Arc<NodeDef>, rng: &mut Rng, turn: Quat) -> Self {
        let base = Affine3A::from_quat(turn) * def.xf.sample(rng);
        let speed = rng.range(def.motion.speed);
        let spin = Vec3::from_array([0, 1, 2].map(|i| {
            let (c, r) = &def.motion.spin[i];
            c.at(0.0) * rng.range(*r)
        }));
        let roll = match &def.look {
            Some(Look::Billboard { rotation, .. }) => rng.range(*rotation),
            _ => 0.0,
        };
        let children = match def.kind {
            Kind::Spawner { .. } => Vec::new(),
            _ => def
                .children
                .iter()
                .map(|c| NodeRt::new(c.clone(), rng, Quat::IDENTITY))
                .collect(),
        };
        let life = def.life;
        Self {
            def,
            life,
            age: 0.0,
            base,
            speed,
            travel: 0.0,
            spin,
            roll,
            anchor: None,
            visual: None,
            children,
            emit: 0.0,
            emissions: 0,
            particles: Vec::new(),
            batch: None,
            ribbon: Vec::new(),
        }
    }

    /// Advances the node by `dt` under its parent's world transform; false once it and all its
    /// children and particles are done (`0x83190798` node lifecycle state machine).
    fn step(
        &mut self,
        parent: Affine3A,
        dt: f32,
        stop: bool,
        rng: &mut Rng,
        cx: &mut Cx,
    ) -> bool {
        let def = self.def.clone();
        // A spawned copy is posed in the spawner's world at birth and then left there.
        let parent = self.anchor.unwrap_or(parent);
        self.age += dt;
        let t = self.age - def.delay;
        if t < 0.0 {
            return !stop;
        }
        // Negative life runs until stopped (`0x83190d60`, `0x83190a14`, `0x83190c50`).
        // Finite life ends at `t >= life`, or early on `stop` if the node's stop flag is set.
        let own = if self.life < 0.0 {
            !stop
        } else {
            t < self.life && !(stop && def.stop_on_stop)
        };
        self.speed += def.motion.accel.at(t) * dt;
        self.travel += self.speed * dt;
        let s = self.spin * t;
        let local = self.base
            * Affine3A::from_rotation_translation(
                Quat::from_euler(EulerRot::YXZ, -s.y, s.x, -s.z),
                Vec3::Z * self.travel,
            );
        let world = parent * local;
        // A stopped action-40 ribbon keeps drawing its remaining history until the samples age
        // past the trail life.
        let ribbon_has = !self.ribbon.is_empty();
        match (&def.look, own || ribbon_has) {
            (Some(look), true) => self.draw(look, t, parent, world, cx, own),
            _ => {
                if let Some(v) = self.visual.take() {
                    cx.despawn(v);
                }
            }
        }
        let alive = match &def.kind {
            Kind::Static => own,
            Kind::Spawner {
                count,
                emissions,
                interval,
                angle,
                child,
            } => {
                let can_spawn = own && (*emissions < 0 || self.emissions < (*emissions).max(1));
                if can_spawn {
                    self.emit -= dt;
                    if self.emit <= 0.0 {
                        self.emissions += 1;
                        for _ in 0..*count {
                            for c in child {
                                let turn = Quat::from_rotation_arc(Vec3::Z, rng.cone(*angle));
                                let mut born = NodeRt::new(c.clone(), rng, turn);
                                if born.life < 0.0 {
                                    born.life = look_span(&born.def).max(1.0 / 60.0);
                                }
                                // f0000301's 2020 child is an infinite model 61 whose colour
                                // runs blue to orange over 2.5 s. Following the nozzle piles
                                // every copy on the last key; the chase camera then only sees
                                // the young blue end of a trail left behind the AC.
                                born.anchor = Some(world);
                                self.children.push(born);
                            }
                        }
                        self.emit = if *interval > 0.0 {
                            self.emit + interval
                        } else {
                            f32::INFINITY
                        };
                    }
                }
                // Spawner stays alive while it still has emissions left (or spawned children live).
                own && (*emissions < 0 || self.emissions < (*emissions).max(1))
            }
            Kind::Cluster(c) => self.cluster(c, t, own, world, dt, rng, cx),
        };
        // Structural children (`anchor.is_none()`) inherit parent expiry as `stop` so an
        // infinite-life child cluster under a finite-life 2101 parent (e.g. f0000098 idle
        // exhaust, f0001404 walk dust, f0001405 landing dust) stops emitting when the parent
        // expires and lets its existing particles fade out. Spawned copies (`anchor.is_some()`)
        // are independent world instances and only stop on effect-level `stop`.
        self.children.retain_mut(|c| {
            let child_stop = if c.anchor.is_some() { stop } else { stop || !own };
            let live = c.step(world, dt, child_stop, rng, cx);
            if !live {
                c.clear(cx);
            }
            live
        });
        alive || !self.children.is_empty() || !self.ribbon.is_empty()
    }

    fn clear(&mut self, cx: &mut Cx) {
        for v in [self.visual.take(), self.batch.take()]
            .into_iter()
            .flatten()
        {
            cx.despawn(v);
        }
        for c in &mut self.children {
            c.clear(cx);
        }
    }

    fn draw(
        &mut self,
        look: &Look,
        t: f32,
        parent: Affine3A,
        world: Affine3A,
        cx: &mut Cx,
        emit: bool,
    ) {
        let (_, rot, pos) = world.to_scale_rotation_translation();
        let k = scale_of(world);
        match look {
            Look::Billboard {
                texture,
                blend,
                width,
                height,
                color,
                frames,
                spin,
                ..
            } => {
                if self.visual.is_none() {
                    let tex = cx.lib.texture(*texture, cx.images);
                    let static_mat = color.keys.len() <= 1 && frames.total <= 1;
                    let material = if static_mat {
                        cx.shared_material(tex, *blend, color.at(0.0))
                    } else {
                        cx.material(tex, *blend)
                    };
                    let e = cx.spawn(
                        (
                            Mesh3d(cx.lib.quad.clone()),
                            MeshMaterial3d(material.clone()),
                        ),
                        Transform::from_translation(pos).with_scale(Vec3::ZERO),
                    );
                    self.visual = Some(Visual {
                        entities: vec![e],
                        material,
                        shared_material: static_mat,
                        mesh: None,
                    });
                }
                let v = self.visual.as_ref().unwrap();
                let roll = self.roll + spin.at(t) * t;
                let scale = Vec3::new(width.at(t) * k, height.at(t) * k, 1.0);
                let tf = Transform {
                    translation: pos,
                    rotation: cx.camera_rot * Quat::from_rotation_z(roll),
                    scale,
                };
                if !v.shared_material {
                    if let Some(mut m) = cx.materials.get_mut(&v.material) {
                        m.base_color = srgba(color.at(t));
                        let (o, s) = frames.rect(t, 0.0);
                        m.uv_transform = Affine2::from_scale_angle_translation(s, 0.0, o);
                    }
                }
                cx.moves.push((v.entities[0], tf, None));
            }
            Look::Model {
                model,
                blend,
                scale,
                uniform,
                color,
                frames,
            } => {
                if self.visual.is_none() {
                    let Some(parts) = cx.lib.model(*model, cx.meshes, cx.images) else {
                        return;
                    };
                    let tex = parts.first().and_then(|p| p.texture.clone());
                    let static_mat = color.keys.len() <= 1 && frames.total <= 1;
                    let material = if static_mat {
                        cx.shared_material(tex, *blend, color.at(0.0))
                    } else {
                        cx.material(tex, *blend)
                    };
                    let entities = parts
                        .iter()
                        .map(|p| {
                            cx.spawn(
                                (Mesh3d(p.mesh.clone()), MeshMaterial3d(material.clone())),
                                Transform::from_translation(pos).with_scale(Vec3::ZERO),
                            )
                        })
                        .collect();
                    self.visual = Some(Visual {
                        entities,
                        material,
                        shared_material: static_mat,
                        mesh: None,
                    });
                }
                let v = self.visual.as_ref().unwrap();
                let s = if *uniform {
                    Vec3::splat(scale[0].at(t))
                } else {
                    Vec3::new(scale[0].at(t), scale[1].at(t), scale[2].at(t))
                };
                let tf = Transform {
                    translation: pos,
                    rotation: rot,
                    scale: s * k,
                };
                if !v.shared_material {
                    // s1020 / s1011 UVs already sit in one atlas cell. Scaling them by the
                    // cell (right for a 0–1 billboard quad) samples an empty corner and the
                    // flame mesh disappears. Slide the cell instead.
                    let (o, s) = frames.rect(t, 0.0);
                    let slide = cx.lib.model(*model, cx.meshes, cx.images).is_some_and(|parts| {
                        parts.iter().all(|p| uvs_in_cell(&p.uvs, s))
                    });
                    let material = v.material.clone();
                    if let Some(mut m) = cx.materials.get_mut(&material) {
                        m.base_color = srgba(color.at(t));
                        m.uv_transform = if slide {
                            Affine2::from_translation(o)
                        } else {
                            Affine2::from_scale_angle_translation(s, 0.0, o)
                        };
                    }
                }
                for &e in &v.entities {
                    cx.moves.push((e, tf, None));
                }
            }
            Look::Light { color, radius } => {
                let c = color.at(t);
                let light = PointLight {
                    color: Color::srgb(c.x, c.y, c.z),
                    intensity: LIGHT_LUMENS * c.w.max(0.0),
                    range: (radius.at(t) * k).max(0.01),
                    shadow_maps_enabled: false,
                    ..default()
                };
                let tf = Transform::from_translation(pos);
                match &self.visual {
                    Some(v) => cx.moves.push((v.entities[0], tf, Some(light))),
                    None => {
                        let e = cx.spawn(light, tf);
                        self.visual = Some(Visual {
                            entities: vec![e],
                            material: Handle::default(),
                            shared_material: true,
                            mesh: None,
                        });
                    }
                }
            }
            Look::Haze { .. } | Look::RadialBlur => {
                // Action 43 (heat haze) and action 107 (FXParticleAppearance_RadialBlur,
                // 0x835b3658) are screen/post-process effects, not world-space cards.
                if let Some(v) = self.visual.take() {
                    cx.despawn(v);
                }
            }
            Look::Ribbon {
                texture,
                blend,
                width,
                interval,
                life,
                segments,
                fade,
                color,
            } => {
                // Sample the parent effect point (the dummy), not this node's own offset:
                // f0000134's outer action 34 spins about Y at 201 rad/s, which would alias a
                // 0.1 m offset into noise.
                let anchor = Vec3::from(parent.translation);
                if emit {
                    let due = self
                        .ribbon
                        .last()
                        .is_none_or(|(st, _)| t - *st >= *interval);
                    if due {
                        self.ribbon.push((t, anchor));
                    }
                }
                let floor = t - life;
                self.ribbon.retain(|(st, _)| *st >= floor);
                // Keep the live head even between interval samples so the strip reaches the
                // nozzle instead of lagging one sample behind.
                let mut pts: Vec<(f32, Vec3)> = self.ribbon.clone();
                if emit && pts.last().is_none_or(|(_, p)| p.distance_squared(anchor) > 1e-6) {
                    pts.push((t, anchor));
                }
                if pts.len() < 2 {
                    return;
                }
                // 0x83197e90 -> 0x835b48f0 evaluates width, colour and fade *per emitted
                // sample* from that sample's own age (`r5+0x10`), not from the node's current
                // age. The segment alpha is `1 - clamp(age / def[0x1c])`; f0000134's slot 15
                // (`def+0x1c`) is 0.5 s, so only the newest half of the 1 s history is opaque.
                let fade_span = if *fade > 0.0 { *fade } else { *life };
                let b = ribbon_batch(
                    &pts, anchor, t, *life, fade_span, *segments, width, color, k, cx.camera,
                );
                if self.visual.is_none() {
                    let tex = cx.lib.texture(*texture, cx.images);
                    let material = cx.shared_material(tex, *blend, Vec4::ONE);
                    let mesh = cx.meshes.add(Batch::default().mesh());
                    let e = cx.spawn(
                        (Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())),
                        Transform::from_translation(anchor),
                    );
                    self.visual = Some(Visual {
                        entities: vec![e],
                        material,
                        shared_material: true,
                        mesh: Some(mesh),
                    });
                }
                let v = self.visual.as_ref().unwrap();
                if let Some(mut m) = v.mesh.as_ref().and_then(|h| cx.meshes.get_mut(h)) {
                    *m = b.mesh();
                }
                cx.moves
                    .push((v.entities[0], Transform::from_translation(anchor), None));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cluster(
        &mut self,
        c: &ClusterDef,
        t: f32,
        own: bool,
        world: Affine3A,
        dt: f32,
        rng: &mut Rng,
        cx: &mut Cx,
    ) -> bool {
        let has_emissions = c.emissions < 0 || self.emissions < c.emissions;
        let in_window =
            c.emit_for < 0.0 || t < c.emit_for || (c.emit_for == 0.0 && self.emissions == 0);
        let emitting = own && has_emissions && in_window;
        if emitting {
            self.emit -= dt;
            while self.emit <= 0.0 {
                if c.emissions >= 0 && self.emissions >= c.emissions {
                    break;
                }
                self.emissions += 1;
                let n = c.count.at(t).round().max(0.0) as usize;
                let local_have = if c.look.follow {
                    self.particles.iter().filter(|p| !p.world).count()
                } else {
                    self.particles.iter().filter(|p| p.world).count()
                };
                for _ in 0..n.min(c.max.saturating_sub(local_have)) {
                    self.particles.push(self.particle(c, t, world, false, rng));
                }
                self.emit = if c.interval > 0.0 {
                    self.emit + c.interval
                } else {
                    f32::INFINITY
                };
            }
        }
        let still_emitting = own
            && (c.emissions < 0 || self.emissions < c.emissions)
            && (c.emit_for < 0.0 || t < c.emit_for);
        let scale = scale_of(world);
        let down_world = Vec3::NEG_Y * scale;
        let down_local = scale * world.inverse().transform_vector3(Vec3::NEG_Y);
        for p in &mut self.particles {
            p.age += dt;
            let down = if p.world { down_world } else { down_local };
            p.vel += down * c.gravity.0.at(p.age) * p.gravity * dt;
            let drag = c.drag.at(p.age);
            let s = p.vel.length();
            if drag != 0.0 && s > 0.0 {
                p.vel *= (s + drag * dt).max(0.0) / s;
            }
            p.pos += p.vel * dt;
            p.angle += p.spin * dt;
        }
        self.particles.retain(|p| p.age < p.life);
        self.draw_batch(c, world, cx);
        still_emitting || !self.particles.is_empty()
    }

    fn particle(
        &self,
        c: &ClusterDef,
        t: f32,
        world: Affine3A,
        force_world: bool,
        rng: &mut Rng,
    ) -> Particle {
        let (local_offset, dir) = match &c.emitter.shape {
            EmitterShape::Cone => (Vec3::ZERO, rng.cone(c.emitter.angle)),
            EmitterShape::Circle { radius } => {
                // Action 30 (`FXClusterEmitter_Circle`): uniform around the horizontal XZ ring of
                // radius `r`, moving radially outward at polar tilt `angle` from +Y (`90°` = XZ).
                let phi = rng.next() * std::f32::consts::TAU;
                let (s, cs) = phi.sin_cos();
                let r = radius.at(t).max(0.0);
                let polar = c.emitter.angle.to_radians();
                let dir = Vec3::new(cs * polar.sin(), polar.cos(), s * polar.sin())
                    .normalize_or(Vec3::X);
                (Vec3::new(cs * r, 0.0, s * r), dir)
            }
            EmitterShape::Sphere { radius } => {
                // Action 31 (`FXClusterEmitter_Sphere`): random direction over `angle` (full apex
                // or full sphere when >= 180°), placed at distance `r` along that direction.
                let dir = rng.cone((c.emitter.angle * 2.0).min(360.0));
                let r = radius.at(t).max(0.0);
                (dir * r, dir)
            }
        };
        let speed = c.emitter.speed.sample(t, rng);
        let size = Vec3::new(
            c.emitter.size[0].sample(t, rng),
            c.emitter.size[1].sample(t, rng),
            c.emitter.size[2].sample(t, rng),
        );
        let tint = c.look.tint.0.lerp(c.look.tint.1, rng.next()) * c.emitter.color.at(t);
        // Slot 21 = 1 (the main-booster flame) stores the particle in the emitter frame, so the
        // jet stays on the nozzle. Slot 21 = 0 leaves it where it was born.
        let in_world = force_world || !c.look.follow;
        let (_, rot, _) = world.to_scale_rotation_translation();
        let (pos, vel, orient) = if in_world {
            (
                world.transform_point3(local_offset),
                world.transform_vector3(dir) * speed,
                rot * Quat::from_rotation_arc(Vec3::Z, dir),
            )
        } else {
            (
                local_offset,
                dir * speed,
                Quat::from_rotation_arc(Vec3::Z, dir),
            )
        };
        Particle {
            pos,
            vel,
            dir: orient,
            age: 0.0,
            life: rng.range(c.look.life).max(0.001),
            size,
            tint,
            angle: rng.range(c.look.rotation),
            spin: c.look.spin.at(0.0) * rng.range(c.look.spin_range),
            frame: rng.range(c.look.start_frame).floor(),
            streak: rng.range(c.look.streak),
            gravity: rng.range(c.gravity.1),
            world: in_world,
        }
    }

    fn draw_batch(&mut self, c: &ClusterDef, world: Affine3A, cx: &mut Cx) {
        let origin = Vec3::from(world.translation);
        let model = match c.look.shape {
            Shape::Model(id) => match cx.lib.model(id, cx.meshes, cx.images) {
                Some(m) => Some(m),
                None => return,
            },
            Shape::Quad { .. } => None,
        };
        let mut b = Batch::default();
        let (right, up) = (cx.camera_rot * Vec3::X, cx.camera_rot * Vec3::Y);
        let k = scale_of(world);
        for p in &self.particles {
            let l = &c.look;
            let size = Vec3::new(
                l.size[0].at(p.age),
                l.size[1].at(p.age),
                l.size[2].at(p.age),
            ) * p.size
                * l.scale
                * k;
            let color = (l.color.at(p.age) * p.tint).to_array();
            let rect = l.frames.rect(p.age, p.frame);
            let (center, vel) = if p.world {
                (p.pos, p.vel)
            } else {
                (
                    world.transform_point3(p.pos),
                    world.transform_vector3(p.vel),
                )
            };
            if let Some(parts) = &model {
                // Mesh +Z follows the dummy forward, as in the action 61 draw. s1011 is wide at
                // the nozzle and thins out along +Z: f0000131's glide jet.
                let (_, rot, _) = world.to_scale_rotation_translation();
                let orient = if p.world { p.dir } else { rot * p.dir };
                let m = Affine3A::from_scale_rotation_translation(
                    size,
                    orient * Quat::from_rotation_z(-p.angle),
                    center - origin,
                );
                for part in parts.iter() {
                    b.model(part, m, rect, color);
                }
                continue;
            }
            let (r, u) = if p.streak > 0.0 && vel.length_squared() > 1e-6 {
                let axis = vel.normalize();
                let side = axis.cross(cx.camera - center).normalize_or_zero();
                (
                    side * size.x * 0.5,
                    axis * (vel.length() * p.streak).max(size.y) * 0.5,
                )
            } else {
                let (s, co) = p.angle.sin_cos();
                (
                    (right * co + up * s) * size.x * 0.5,
                    (up * co - right * s) * size.y * 0.5,
                )
            };
            let q = center - origin;
            b.quad(
                [q - r - u, q + r - u, q + r + u, q - r + u],
                rect,
                color,
            );
        }
        if self.batch.is_none() {
            let (texture, blend) = match (&c.look.shape, &model) {
                (
                    Shape::Quad {
                        texture: Some(id),
                        blend,
                    },
                    _,
                ) => (cx.lib.texture(*id, cx.images), *blend),
                (
                    Shape::Quad {
                        texture: None,
                        blend,
                    },
                    _,
                ) => (Some(cx.lib.dot.clone()), *blend),
                (Shape::Model(_), Some(parts)) => {
                    (parts.first().and_then(|p| p.texture.clone()), Blend::Add)
                }
                _ => (None, Blend::Add),
            };
            let material = cx.shared_material(texture, blend, Vec4::ONE);
            let mesh = cx.meshes.add(b.mesh());
            let e = cx.spawn(
                (Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())),
                Transform::from_translation(origin),
            );
            self.batch = Some(Visual {
                entities: vec![e],
                material,
                shared_material: true,
                mesh: Some(mesh),
            });
            return;
        }
        let v = self.batch.as_ref().unwrap();
        if let Some(mut m) = v.mesh.as_ref().and_then(|h| cx.meshes.get_mut(h)) {
            *m = b.mesh();
        }
        cx.moves
            .push((v.entities[0], Transform::from_translation(origin), None));
    }
}

/// Vertices of a particle batch, relative to the emitter.
#[derive(Default)]
struct Batch {
    pos: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    color: Vec<[f32; 4]>,
    index: Vec<u32>,
}

impl Batch {
    fn quad(&mut self, corners: [Vec3; 4], (o, s): (Vec2, Vec2), color: [f32; 4]) {
        let i = self.pos.len() as u32;
        self.pos.extend(corners.map(|c| c.to_array()));
        self.uv.extend(
            [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]
                .map(|[u, v]| [o.x + u * s.x, o.y + v * s.y]),
        );
        self.color.extend([vertex_color(color); 4]);
        self.index.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
    }

    fn model(
        &mut self,
        part: &Part,
        m: Affine3A,
        (o, _): (Vec2, Vec2),
        color: [f32; 4],
    ) {
        let i = self.pos.len() as u32;
        self.pos.extend(
            part.positions
                .iter()
                .map(|&p| m.transform_point3(p).to_array()),
        );
        // The mesh UVs already sit inside one atlas cell (s1020 is about 0.05–0.08 of a
        // 512-wide strip). Scaling them by the cell size samples a few texels and the card
        // goes flat white. Slide the cell by whole frames instead.
        self.uv
            .extend(part.uvs.iter().map(|[u, v]| [u + o.x, v + o.y]));
        self.color.extend(std::iter::repeat_n(
            vertex_color(color),
            part.positions.len(),
        ));
        self.index.extend(part.indices.iter().map(|&k| i + k));
    }

    fn mesh(mut self) -> Mesh {
        if self.index.is_empty() {
            self.pos = vec![[0.0; 3]; 3];
            self.uv = vec![[0.0; 2]; 3];
            self.color = vec![[0.0; 4]; 3];
            self.index = vec![0, 1, 2];
        }
        let n = self.pos.len();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uv);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.color);
        mesh.insert_indices(Indices::U32(self.index));
        mesh
    }
}

fn linear(c: [f32; 4]) -> [f32; 4] {
    Color::srgba(c[0], c[1], c[2], c[3])
        .to_linear()
        .to_f32_array()
}

#[allow(clippy::too_many_arguments)]
fn simulate(
    time: Res<Time>,
    lib: Option<ResMut<Library>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<
        &GlobalTransform,
        (
            With<Camera3d>,
            Without<SfxVisual>,
            Without<crate::env::WaterReflectionCamera>,
        ),
    >,
    mut effects: Query<(Entity, &mut Sfx, &GlobalTransform)>,
    mut visuals: Query<
        (
            &mut Transform,
            &mut GlobalTransform,
            Option<&mut PointLight>,
        ),
        (With<SfxVisual>, Without<Sfx>),
    >,
) {
    let Some(mut lib) = lib else { return };
    let (camera, camera_rot) = cameras
        .iter()
        .next()
        .map_or((Vec3::ZERO, Quat::IDENTITY), |c| {
            let (_, r, t) = c.to_scale_rotation_translation();
            (t, r)
        });
    let dt = time.delta_secs().min(0.1);
    for (entity, mut sfx, at) in &mut effects {
        let stop = sfx.stopping;
        if sfx.run.is_none() {
            let Some(roots) = lib.effect(sfx.id) else {
                commands.entity(entity).try_despawn();
                continue;
            };
            lib.seed = lib
                .seed
                .wrapping_mul(747_796_405)
                .wrapping_add(2_891_336_453);
            let mut rng = Rng(lib.seed | 1);
            let nodes = roots
                .iter()
                .map(|d| NodeRt::new(d.clone(), &mut rng, Quat::IDENTITY))
                .collect();
            sfx.run = Some(Run { nodes, rng });
        }
        let run = sfx.run.as_mut().unwrap();
        let mut cx = Cx {
            commands: &mut commands,
            lib: &mut lib,
            meshes: &mut meshes,
            materials: &mut materials,
            images: &mut images,
            camera,
            camera_rot,
            owner: entity,
            moves: Vec::new(),
        };
        let world = at.affine();
        let Run { nodes, rng } = run;
        nodes.retain_mut(|n| {
            let live = n.step(world, dt, stop, rng, &mut cx);
            if !live {
                n.clear(&mut cx);
            }
            live
        });
        for (e, tf, light) in std::mem::take(&mut cx.moves) {
            if let Ok((mut t, mut g, l)) = visuals.get_mut(e) {
                *t = tf;
                *g = GlobalTransform::from(tf);
                if let (Some(mut l), Some(light)) = (l, light) {
                    *l = light;
                }
            }
        }
        if nodes.is_empty() {
            commands.entity(entity).try_despawn();
        }
    }
}

/// Despawns visuals whose effect entity is gone (despawned with its AC, or ended).
fn sweep(
    mut commands: Commands,
    visuals: Query<(Entity, &SfxVisual)>,
    effects: Query<(), With<Sfx>>,
) {
    for (e, v) in &visuals {
        if !effects.contains(v.0) {
            commands.entity(e).try_despawn();
        }
    }
}

/// One effect the dispatcher wants on a dummy: id and uniform scale.
struct Play {
    id: i32,
    scale: f32,
}

/// `0x82825918`: eight 45° sectors of the move stick. 0 is no stick. Values 6, 7 and 8 are the
/// rear three sectors (more than 112.5° off forward). The table at `0x8208fe2c` is
/// `[1, 3, 5, 8, 6, 7, 4, 2]`.
fn rear_stick(stick: Vec2) -> bool {
    if stick.length_squared() < 1e-8 {
        return false;
    }
    let deg = stick.x.atan2(stick.y).to_degrees();
    let index = ((deg + 22.5) / 45.0 + 8.0) as i32 & 7;
    const TABLE: [u8; 8] = [1, 3, 5, 8, 6, 7, 4, 2];
    matches!(TABLE[index as usize], 6 | 7 | 8)
}

/// Effects for one dummy colour. `0x828932a0` / `0x82892f08`, fed by the lists built in
/// `0x82893ec0`:
/// - ground boost (slot `+0x10`) and air boost (slot `+0x2c`): main 300 on nozzles 31-34 and
///   light 299 on 41, scale 1; back 302 on leg points 21-24, scale 1. Air boost passes f2 = 1,
///   so the main flame stays on with the stick neutral.
/// - glide (slots `+0x34` and `+0x38`, mode 1): glide main 131 on 31-34 at the clamped stick
///   length, and glide foot 120 in place of foot 301. A rear stick also keeps main 300.
/// - strafe: back 302 on dummies 11/12 (stick x > 0.1) or 13/14 (stick x < -0.1), scale |x|,
///   with the light on 42 or 43.
fn booster_plays(
    id: u8,
    stick: Vec2,
    glide: bool,
    active: bool,
    glide_scale: f32,
    row: &acvd_data::generated::sfx::MapSfxparamSt,
) -> Vec<Play> {
    if !active {
        return Vec::new();
    }
    let main = i32::from(row.ac_main_booster);
    let back = i32::from(row.ac_back_booster);
    let light = i32::from(row.ac_booster_light);
    let mut out = Vec::new();
    let push = |out: &mut Vec<Play>, id: i32, scale: f32| {
        if id > 0 {
            out.push(Play { id, scale });
        }
    };
    if MAIN_NOZZLES.contains(&id) {
        if glide {
            if rear_stick(stick) {
                push(&mut out, main, 1.0);
            }
            // Each glide frame `0x830e8ef8` leaves five effects at 0.6 and two at 1.0
            // (`private/xenia/glide_fxscale.jsonl`), the five 131 and two 120 starts.
            push(&mut out, GLIDE_MAIN, glide_scale);
            push(&mut out, GLIDE_FOOT, 1.0);
        } else {
            push(&mut out, main, 1.0);
        }
    }
    if id == 41 {
        push(&mut out, light, 1.0);
    }
    if LEG_NOZZLES.contains(&id) && !glide {
        push(&mut out, back, 1.0);
    }
    if stick.x > SIDE_STICK && matches!(id, 11 | 12) {
        push(&mut out, back, stick.x);
    }
    if stick.x > SIDE_STICK && id == 42 {
        push(&mut out, light, stick.x);
    }
    if stick.x < -SIDE_STICK && matches!(id, 13 | 14) {
        push(&mut out, back, -stick.x);
    }
    if stick.x < -SIDE_STICK && id == 43 {
        push(&mut out, light, -stick.x);
    }
    out
}

#[derive(Default)]
struct Tally {
    nodes: u32,
    spawners: u32,
    spawned: u32,
    particles: u32,
    drawn: u32,
}

fn tally(n: &NodeRt, t: &mut Tally) {
    t.nodes += 1;
    t.particles += n.particles.len() as u32;
    if n.visual.is_some() || n.batch.is_some() {
        t.drawn += 1;
    }
    if matches!(n.def.kind, Kind::Spawner { .. }) {
        t.spawners += 1;
        t.spawned += n.children.len() as u32;
    }
    for c in &n.children {
        tally(c, t);
    }
}

fn sfx_probe(sfx: &Sfx) -> String {
    let Some(run) = &sfx.run else {
        return format!("run=none stop={}", u8::from(sfx.stopping));
    };
    let mut t = Tally::default();
    for n in &run.nodes {
        tally(n, &mut t);
    }
    format!(
        "nodes={} spawners={} spawned={} particles={} drawn={} stop={}",
        t.nodes, t.spawners, t.spawned, t.particles, t.drawn, u8::from(sfx.stopping)
    )
}

/// Logs booster effects on the frame [`ShotMark`] names. `take_shot` sets that path in Update;
/// this runs after [`simulate`] in PostUpdate, which is the state the screenshot renders.
fn probe_shot(
    mark: Option<ResMut<ShotMark>>,
    pilots: Query<(&Pilot, &Transform)>,
    points: Query<(&EffectPoint, &GlobalTransform)>,
    effects: Query<(&Sfx, &GlobalTransform)>,
    visuals: Query<(&SfxVisual, &Transform)>,
    cameras: Query<&GlobalTransform, (With<Camera3d>, Without<crate::env::WaterReflectionCamera>)>,
) {
    let Some(mut mark) = mark else { return };
    let Some(path) = mark.path.take() else { return };
    let Ok((pilot, at)) = pilots.single() else {
        eprintln!("SHOT {} no pilot", path.display());
        return;
    };
    let cam = cameras
        .iter()
        .next()
        .map(|c| c.rotation() * Vec3::NEG_Z)
        .unwrap_or(Vec3::Z);
    eprintln!(
        "SHOT {} pos=({:.2},{:.2},{:.2}) glide={} boost={} air={} stick=({:.3},{:.3}) speed={:.3} vel=({:.2},{:.2},{:.2}) cam=({:.2},{:.2},{:.2})",
        path.display(),
        at.translation.x,
        at.translation.y,
        at.translation.z,
        u8::from(pilot.glide),
        u8::from(pilot.boost),
        u8::from(pilot.airborne),
        pilot.command.x,
        pilot.command.y,
        pilot.velocity.with_y(0.0).length(),
        pilot.velocity.x,
        pilot.velocity.y,
        pilot.velocity.z,
        cam.x,
        cam.y,
        cam.z
    );
    let mut rows: Vec<_> = points.iter().collect();
    rows.sort_by_key(|(p, _)| (p.column, p.id));
    let mut shown = 0u32;
    for (p, xf) in rows {
        if p.playing.is_empty() {
            continue;
        }
        let at = xf.translation();
        for (id, fx, scale) in &p.playing {
            shown += 1;
            let body = effects
                .get(*fx)
                .map(|(s, _)| sfx_probe(s))
                .unwrap_or_else(|_| "missing".to_string());
            let axis = effects
                .get(*fx)
                .map(|(_, g)| g.rotation() * Vec3::Z)
                .unwrap_or(Vec3::ZERO);
            let mut n = 0u32;
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            let mut max_z = 0.0f32;
            for (v, tf) in &visuals {
                if v.0 != *fx {
                    continue;
                }
                n += 1;
                lo = lo.min(tf.translation);
                hi = hi.max(tf.translation);
                max_z = max_z.max(tf.scale.z.abs());
            }
            let span = if n == 0 { Vec3::ZERO } else { hi - lo };
            eprintln!(
                "SHOT {} dummy {} ({}) f{id} scale={scale:.3} at=({:.1},{:.1},{:.1}) {body} vis={n} span=({:.1},{:.1},{:.1}) maxz={max_z:.2} axis=({:.2},{:.2},{:.2})",
                path.display(),
                p.id,
                p.column,
                at.x,
                at.y,
                at.z,
                span.x,
                span.y,
                span.z,
                axis.x,
                axis.y,
                axis.z
            );
        }
    }
    if shown == 0 {
        eprintln!("SHOT {} no booster effects playing", path.display());
    }
    let mut all_fx = 0u32;
    for (sfx, xf) in &effects {
        all_fx += 1;
        let at = xf.translation();
        let body = sfx_probe(sfx);
        eprintln!(
            "SHOT {} active_sfx f{} at=({:.2},{:.2},{:.2}) {body}",
            path.display(),
            sfx.id,
            at.x,
            at.y,
            at.z,
        );
        if let Some(run) = &sfx.run {
            for (i, n) in run.nodes.iter().enumerate() {
                let kind_desc = match &n.def.kind {
                    Kind::Static => "Static".to_string(),
                    Kind::Spawner {
                        count,
                        emissions,
                        interval,
                        ..
                    } => format!(
                        "Spawner(count={count} emis={}/{emissions} int={interval:.4} children={})",
                        n.emissions,
                        n.children.len()
                    ),
                    Kind::Cluster(c) => format!(
                        "Cluster(emit_for={:.4} int={:.4} max={} parts={} follow={} p_life={:.3}..{:.3})",
                        c.emit_for,
                        c.interval,
                        c.max,
                        n.particles.len(),
                        u8::from(c.look.follow),
                        c.look.life.0,
                        c.look.life.1
                    ),
                };
                eprintln!(
                    "SHOT {}   node[{i}] age={:.3} life={:.4} delay={:.4} {kind_desc}",
                    path.display(),
                    n.age,
                    n.life,
                    n.def.delay,
                );
            }
        }
    }
    eprintln!("SHOT {} total_active_sfx={all_fx}", path.display());
}

/// Lights the booster effects. The groups and which state plays them are `0x828932a0`.
pub fn boosters(
    mut commands: Commands,
    pilots: Query<&Pilot>,
    mut points: Query<(Entity, &mut EffectPoint)>,
    mut effects: Query<&mut Sfx>,
    mut xforms: Query<&mut Transform>,
) {
    let Ok(pilot) = pilots.single() else { return };
    let Some(row) = find(PARAM_MAPSFXPARAM_BIN, 0).map(|r| &r.data) else {
        return;
    };
    let stick = pilot.command;
    let glide = pilot.glide && pilot.boost;
    // f0000134 ribbon trails belong to the air glide. Stop them when it ends; each node keeps
    // its remaining second of samples while they age out.
    if !glide {
        for mut s in &mut effects {
            if s.id == GLIDE_TRAIL {
                s.stop();
            }
        }
    }
    let wj_accel = pilot.in_wall_jump_accel();
    let kicking = pilot.in_kick();
    let blade_dash = pilot.in_blade_dash();
    let glide_scale = if wj_accel {
        pilot.ctrl.wall_jump_sfx_scale
    } else if kicking {
        pilot.ctrl.kick_sfx_scale
    } else if blade_dash {
        GLIDE_SFX_SCALE
    } else {
        stick.length().clamp(0.0, GLIDE_SFX_SCALE)
    };
    let moving = stick.length() > SIDE_STICK || pilot.velocity.with_y(0.0).length() > BOOST_MOVING;
    // Ground boost only while the boost-move slot runs. Air boost passes f2 = 1, so the main
    // flame stays lit with the stick neutral. Glide passes the clamped stick as both;
    // AcMove_wallJump (`0x82824560`) lights mode 4 at `wall_jump_sfx_scale` (0.7);
    // AcMove_boostChargeKick (`0x82823740`) lights mode 3 at `kick_sfx_scale` (0.7).
    let active = wj_accel
        || kicking
        || blade_dash
        || pilot.boost && (glide && glide_scale > 0.0 || !glide && (pilot.airborne || moving));
    for (e, mut p) in &mut points {
        let want = booster_plays(p.id, stick, glide, active, glide_scale, row);
        p.playing.retain(|(_, fx, _)| effects.contains(*fx));
        for (id, fx, applied) in p.playing.clone() {
            if !want.iter().any(|w| w.id == id) {
                if let Ok(mut s) = effects.get_mut(fx) {
                    s.stop();
                }
            } else if let Some(w) = want.iter().find(|w| w.id == id) {
                if (applied - w.scale).abs() > 0.001 {
                    if let Ok(mut t) = xforms.get_mut(fx) {
                        t.scale = Vec3::splat(w.scale.max(0.001));
                    }
                }
            }
        }
        p.playing.retain(|(_, fx, _)| {
            effects.contains(*fx) && !effects.get(*fx).is_ok_and(|s| s.stopping)
        });
        for w in &want {
            if p.playing.iter().any(|(id, _, _)| *id == w.id) {
                if let Some(slot) = p.playing.iter_mut().find(|(id, _, _)| *id == w.id) {
                    slot.2 = w.scale;
                }
                continue;
            }
            let fx = commands
                .spawn((
                    Sfx::new(w.id),
                    Transform::from_scale(Vec3::splat(w.scale.max(0.001))),
                    Visibility::default(),
                    ChildOf(e),
                ))
                .id();
            p.playing.push((w.id, fx, w.scale));
        }
    }
}

/// Selects the Quick Boost effect ID from `MapSfxparamSt` matching 360 `0x82891ce8`:
/// - `scale > 1.0` -> `ac_qb_large` (`305`)
/// - `scale < 1.0` -> `ac_qb_shortage` (`304`)
/// - `scale == 1.0` -> `ac_qb_normal` (`303`)
pub(crate) fn qb_sfx_id(scale: f32, row: &MapSfxparamSt) -> i32 {
    if scale > 1.0 {
        i32::from(row.ac_qb_large)
    } else if scale < 1.0 {
        i32::from(row.ac_qb_shortage)
    } else {
        i32::from(row.ac_qb_normal)
    }
}

/// Extra FFX bursts triggered by `HighBoostState` TAE events `0xC1D..=0xC22` (`0x8283c8d0` ->
/// `0x828a816c`, verified in `private/xenia/fx_polish2.jsonl` lines 95–98, 134–138, 195–198):
/// - `0xC1D`/`0xC1E` (charge enter, ground/air): wind-up only
/// - `0xC1F`/`0xC20` (normal release, ground/air): `f0000766` (`0x2FE`) at scale `1.5`
/// - `0xC21`/`0xC22` (second-stage release, ground/air): `f0000121` (`0x79`) + `f0000766` at `1.5`
pub(crate) fn qb_tae_ffx(tae_id: u32) -> &'static [(i32, f32)] {
    match tae_id {
        0x0C1F | 0x0C20 => &[(766, 1.5)],
        0x0C21 | 0x0C22 => &[(121, 1.5), (766, 1.5)],
        _ => &[],
    }
}

/// Which booster dummy sockets fire on a Quick Boost in body-relative eighth `0..=7`
/// (`0` forward, `2` right, `4` back, `6` left; 360 `0x828926b0` / `0x82893ec0`).
fn qb_nozzle_matches(eighth: u8, dummy_id: u8) -> bool {
    match eighth & 7 {
        0 => MAIN_NOZZLES.contains(&dummy_id),
        1 => MAIN_NOZZLES.contains(&dummy_id) || matches!(dummy_id, 11 | 12),
        2 => matches!(dummy_id, 11 | 12),
        3 => LEG_NOZZLES.contains(&dummy_id) || matches!(dummy_id, 11 | 12),
        4 => LEG_NOZZLES.contains(&dummy_id) || MAIN_NOZZLES.contains(&dummy_id),
        5 => LEG_NOZZLES.contains(&dummy_id) || matches!(dummy_id, 13 | 14),
        6 => matches!(dummy_id, 13 | 14),
        7 => MAIN_NOZZLES.contains(&dummy_id) || matches!(dummy_id, 13 | 14),
        _ => false,
    }
}

#[derive(Default)]
struct QbSfxState {
    quick_boosts: u32,
    qb_shortages: u32,
}

/// Fires one-shot Quick Boost effects:
/// - EN shortage (`304`, `ac_qb_shortage`, scale `< 1.0`, `0x82891ce8`)
/// - Normal QB (`303`, `ac_qb_normal`, scale `1.0`) + TAE `0xC1F`/`0xC20` (`766` @ 1.5)
/// - Second-Stage / Large QB (`305`, `ac_qb_large`, scale `> 1.0`) + TAE `0xC21`/`0xC22`
///   (`121` + `766` @ 1.5)
fn qb_sfx(
    mut commands: Commands,
    pilots: Query<(&Pilot, &Transform)>,
    points: Query<(Entity, &EffectPoint)>,
    mut state: Local<QbSfxState>,
) {
    let Ok((pilot, ac_tf)) = pilots.single() else {
        return;
    };
    let Some(row) = find(PARAM_MAPSFXPARAM_BIN, 0).map(|r| &r.data) else {
        return;
    };

    if pilot.qb_shortages != state.qb_shortages {
        state.qb_shortages = pilot.qb_shortages;
        let id = qb_sfx_id(QB_SHORTAGE_SCALE, row);
        if id > 0 {
            let mut spawned = false;
            for (e, p) in &points {
                if MAIN_NOZZLES.contains(&p.id) {
                    commands.spawn((
                        Sfx::new(id),
                        Transform::from_scale(Vec3::splat(QB_SHORTAGE_SCALE)),
                        Visibility::default(),
                        ChildOf(e),
                    ));
                    spawned = true;
                }
            }
            if !spawned {
                commands.spawn((
                    Sfx::new(id),
                    Transform::from_translation(ac_tf.translation + Vec3::Y * 4.0)
                        .with_scale(Vec3::splat(QB_SHORTAGE_SCALE)),
                ));
            }
        }
    }

    if pilot.quick_boosts != state.quick_boosts {
        state.quick_boosts = pilot.quick_boosts;
        let scale = if pilot.second_stage_qb {
            QB_LARGE_SCALE
        } else {
            1.0
        };
        let id = qb_sfx_id(scale, row);
        if id > 0 {
            let mut matched = Vec::new();
            let mut main_fallback = Vec::new();
            for (e, p) in &points {
                if qb_nozzle_matches(pilot.qb_eighth, p.id) {
                    matched.push(e);
                }
                if MAIN_NOZZLES.contains(&p.id) {
                    main_fallback.push(e);
                }
            }
            let targets = if matched.is_empty() {
                &main_fallback
            } else {
                &matched
            };
            if targets.is_empty() {
                commands.spawn((
                    Sfx::new(id),
                    Transform::from_translation(ac_tf.translation + Vec3::Y * 4.0)
                        .with_scale(Vec3::splat(scale)),
                ));
            } else {
                for &e in targets {
                    commands.spawn((
                        Sfx::new(id),
                        Transform::from_scale(Vec3::splat(scale)),
                        Visibility::default(),
                        ChildOf(e),
                    ));
                }
            }
        }
        // HighBoostState TAE 0xC1F..0xC22 bursts (`0x8283c8d0` -> `0x828a816c`, `fx_polish2.jsonl`).
        let primary_nozzle = points.iter().find(|(_, p)| p.id == 31).map(|(e, _)| e);
        for &(tae_fx, tae_scale) in qb_tae_ffx(pilot.last_qb_tae) {
            match primary_nozzle {
                Some(e) => {
                    commands.spawn((
                        Sfx::new(tae_fx),
                        Transform::from_scale(Vec3::splat(tae_scale)),
                        Visibility::default(),
                        ChildOf(e),
                    ));
                }
                None => {
                    commands.spawn((
                        Sfx::new(tae_fx),
                        Transform::from_translation(ac_tf.translation + Vec3::Y * 4.0)
                            .with_scale(Vec3::splat(tae_scale)),
                    ));
                }
            }
        }
    }
}

/// Resolves a `param/groundsfxparam.bin` effect ID for `row_id` (1 walk, 2 landing, 3 grounded
/// slide/boost, 7 kick) and HMD [`GroundSurface`] (360 `0x829ee1d0`).
pub(crate) fn ground_sfx_id(row_id: u32, surface: GroundSurface) -> i32 {
    let Some(row) = find(PARAM_GROUNDSFXPARAM_BIN, row_id) else {
        return 0;
    };
    let id = surface.sfx_id(&row.data);
    if id > 0 {
        id
    } else if surface == GroundSurface::Default {
        // `groundsfxparam.bin` leaves `default` at 0 on rows 1-3; fall back to `concrete`
        // (`1404`/`1405`/`1406`, confirmed on m4000 in `private/xenia/fx_polish.jsonl`).
        i32::from(row.data.concrete)
    } else {
        0
    }
}

#[derive(Default)]
struct GroundWaterState {
    landings: u32,
    wall_jumps: u32,
    /// Continuous looping grounded-slide dust entity (`groundsfxparam` row 3, e.g. `f0001406`,
    /// whose nodes have infinite life `-1/60` and must be stopped when sliding ends).
    slide_fx: Option<(i32, Entity)>,
    /// Continuous looping near-water spray entity (`envsfxparam` row 0 `water_spray_sfx_id = 1420`,
    /// `AcSfxCtrl + 0x690` `0x82382688`, whose nodes have infinite life `-1/60`).
    water_spray_fx: Option<(i32, Entity)>,
    submerged: bool,
}

/// Maintains a single continuous looping [`Sfx`] handle (`f0001406` / `f0001420`) at `at` when
/// `want_id > 0`, and stops it cleanly when `want_id <= 0` or the ID changes.
fn sync_loop_sfx(
    commands: &mut Commands,
    slot: &mut Option<(i32, Entity)>,
    want_id: i32,
    at: Vec3,
    effects: &mut Query<(&mut Sfx, &mut Transform), Without<Pilot>>,
) {
    if let Some((cur_id, e)) = *slot {
        match effects.get_mut(e) {
            Ok((mut sfx, mut tf)) => {
                if cur_id == want_id && want_id > 0 && !sfx.stopping {
                    tf.translation = at;
                    return;
                }
                sfx.stop();
                *slot = None;
            }
            Err(_) => {
                *slot = None;
            }
        }
    }
    if want_id > 0 {
        let e = commands
            .spawn((Sfx::new(want_id), Transform::from_translation(at)))
            .id();
        *slot = Some((want_id, e));
    }
}

/// Spawns ground walk/landing/grounded-boost/kick dust from `param/groundsfxparam.bin`
/// (`0x82835570` -> `0x82833d54`) and water entry/exit splashes + near-water spray from
/// `param/envsfxparam.bin` (`0x82382688` -> `0x82895200` / `0x82895240` / `0x82895290`).
#[allow(clippy::too_many_arguments)]
fn ground_and_water_sfx(
    mut commands: Commands,
    collision: Res<Collision>,
    water_return: Option<Res<crate::map::WaterReturnPoints>>,
    motion: Option<Res<crate::pose::Motion>>,
    mut pilots: Query<(&mut Pilot, &Transform)>,
    mut loop_fx: Query<(&mut Sfx, &mut Transform), Without<Pilot>>,
    mut state: Local<GroundWaterState>,
) {
    let Ok((mut pilot, ac_tf)) = pilots.single_mut() else {
        return;
    };
    let pos = ac_tf.translation;
    let surface = pilot.ground_surface;

    // 1. Walk dust (`groundsfxparam` row 1, `1404` on concrete) on TAE footsteps while grounded.
    if !pilot.airborne {
        if let Some(m) = motion.as_deref() {
            let walk_id = ground_sfx_id(GROUND_ROW_WALK, surface);
            if walk_id > 0 {
                for ev in &m.tae_events {
                    if let crate::pose::TaeFired::Footstep { .. } = ev {
                        commands.spawn((Sfx::new(walk_id), Transform::from_translation(pos)));
                    }
                }
            }
        }
    }

    // 2. Landing dust (`groundsfxparam` row 2, `1405` on concrete) on touchdown.
    if pilot.landings != state.landings {
        state.landings = pilot.landings;
        let land_id = ground_sfx_id(GROUND_ROW_LAND, surface);
        if land_id > 0 {
            commands.spawn((Sfx::new(land_id), Transform::from_translation(pos)));
        }
    }

    // 3. Grounded boost / slide dust (`groundsfxparam` row 3, continuous looping `1406` on concrete).
    let horiz_speed = pilot.velocity.with_y(0.0).length();
    let want_slide = if !pilot.airborne
        && (pilot.boost || pilot.in_quick_boost() || pilot.glide)
        && horiz_speed > GROUND_SLIDE_SPEED
    {
        ground_sfx_id(GROUND_ROW_SLIDE, surface)
    } else {
        0
    };
    sync_loop_sfx(&mut commands, &mut state.slide_fx, want_slide, pos, &mut loop_fx);

    // 4. Wall-kick / surface kick-off dust (`groundsfxparam` row 7, `116`, `0x828358a0..0x82835918`:
    //    only fires on wall-jump or grounded surface kick contact, not mid-air boost-charge kick).
    if pilot.wall_jumps != state.wall_jumps {
        state.wall_jumps = pilot.wall_jumps;
        let kick_id = ground_sfx_id(GROUND_ROW_KICK, surface);
        if kick_id > 0 {
            commands.spawn((Sfx::new(kick_id), Transform::from_translation(pos)));
        }
    }

    // 5. Water splashes & continuous near-water spray (`envsfxparam.bin` row 0: 1419 / 1420).
    let Some(env_row) = find(PARAM_ENVSFXPARAM_BIN, 0).map(|r| &r.data) else {
        return;
    };
    let splash_id = i32::from(env_row.water_splash_sfx_id);
    let fly_out_id = i32::from(env_row.water_fly_out_sfx_id);
    let spray_id = i32::from(env_row.water_spray_sfx_id);

    if let Some(impact_at) = pilot.water_splash_at.take() {
        if splash_id > 0 {
            commands.spawn((Sfx::new(splash_id), Transform::from_translation(impact_at)));
        }
        state.submerged = false;
    }

    let water_y = collision
        .ray_down(pos + Vec3::Y * 54.6, pos.y - 50.0, Layer::Water)
        .or_else(|| water_return.as_deref().and_then(|w| w.water_height));
    if let Some(wy) = water_y {
        let now_submerged = pos.y <= wy;
        let at_water = Vec3::new(pos.x, wy, pos.z);
        if !state.submerged && now_submerged && splash_id > 0 {
            commands.spawn((Sfx::new(splash_id), Transform::from_translation(at_water)));
        } else if state.submerged && !now_submerged && fly_out_id > 0 {
            commands.spawn((Sfx::new(fly_out_id), Transform::from_translation(at_water)));
        }
        state.submerged = now_submerged;

        // Only emit continuous near-water spray over open water (not through a solid concrete
        // bridge or road surface that sits above `wy`).
        let ground_above_water = collision
            .ground_below(pos)
            .is_some_and(|gy| gy > wy + 0.1);
        let clearance = pos.y - wy;
        let want_spray = if !ground_above_water
            && (-2.0..=WATER_SPRAY_CLEARANCE).contains(&clearance)
            && (pilot.boost || horiz_speed > BOOST_MOVING)
        {
            spray_id
        } else {
            0
        };
        sync_loop_sfx(
            &mut commands,
            &mut state.water_spray_fx,
            want_spray,
            at_water,
            &mut loop_fx,
        );
    } else {
        state.submerged = false;
        sync_loop_sfx(&mut commands, &mut state.water_spray_fx, 0, pos, &mut loop_fx);
    }
}

/// `--sfx`: keeps one copy of the effect playing 20 m ahead of and 5 m above the AC, facing up.
fn preview(
    mut commands: Commands,
    preview: Option<Res<Preview>>,
    acs: Query<&GlobalTransform, With<Pilot>>,
    effects: Query<&Sfx>,
    mut playing: Local<Option<Entity>>,
) {
    let Some(preview) = preview else { return };
    if playing.is_some_and(|e| effects.contains(e)) {
        return;
    }
    let Ok(ac) = acs.single() else { return };
    let at = ac.transform_point(Vec3::new(0.0, 5.0, -20.0));
    *playing = Some(
        commands
            .spawn((
                Sfx::new(preview.0),
                Transform::from_translation(at)
                    .with_rotation(Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
            ))
            .id(),
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use acvd_data::find;
    use acvd_data::generated::sfx::{PARAM_ENVSFXPARAM_BIN, PARAM_MAPSFXPARAM_BIN};
    use bevy::prelude::{Assets, Image, StandardMaterial, Vec2, Vec3, Vec4};

    use super::{
        cached_material, ground_sfx_id, qb_sfx_id, qb_tae_ffx, rear_stick, ribbon_batch, Blend,
        ColorCurve, Curve, GROUND_ROW_KICK, GROUND_ROW_LAND, GROUND_ROW_SLIDE, GROUND_ROW_WALK,
        QB_LARGE_SCALE, QB_SHORTAGE_SCALE,
    };
    use crate::collision::GroundSurface;

    #[test]
    fn stick_sectors_match_the_360_table() {
        assert!(!rear_stick(Vec2::ZERO));
        assert!(!rear_stick(Vec2::new(0.0, 1.0)), "forward");
        assert!(!rear_stick(Vec2::new(1.0, 0.0)), "right");
        assert!(!rear_stick(Vec2::new(-1.0, 0.0)), "left");
        assert!(rear_stick(Vec2::new(0.0, -1.0)), "back");
        assert!(rear_stick(Vec2::new(0.5, -1.0)), "back-right");
        assert!(!rear_stick(Vec2::new(1.0, 0.4)), "forward-right");
    }

    #[test]
    fn qb_sfx_id_matches_360_dispatcher_and_tae_bursts() {
        let row = &find(PARAM_MAPSFXPARAM_BIN, 0).expect("mapsfxparam row 0").data;
        assert_eq!(qb_sfx_id(QB_SHORTAGE_SCALE, row), 304, "scale < 1.0 -> ac_qb_shortage");
        assert_eq!(qb_sfx_id(1.0, row), 303, "scale == 1.0 -> ac_qb_normal");
        assert_eq!(qb_sfx_id(QB_LARGE_SCALE, row), 305, "scale > 1.0 -> ac_qb_large");

        // HighBoostState TAE 0xC1D..0xC22 (private/xenia/fx_polish2.jsonl).
        assert!(qb_tae_ffx(0x0C1D).is_empty());
        assert!(qb_tae_ffx(0x0C1E).is_empty());
        assert_eq!(qb_tae_ffx(0x0C1F), &[(766, 1.5)]);
        assert_eq!(qb_tae_ffx(0x0C20), &[(766, 1.5)]);
        assert_eq!(qb_tae_ffx(0x0C21), &[(121, 1.5), (766, 1.5)]);
        assert_eq!(qb_tae_ffx(0x0C22), &[(121, 1.5), (766, 1.5)]);
    }

    #[test]
    fn ground_and_water_sfx_rows_resolve_from_disc_params() {
        assert_eq!(ground_sfx_id(GROUND_ROW_WALK, GroundSurface::Concrete), 1404);
        assert_eq!(ground_sfx_id(GROUND_ROW_LAND, GroundSurface::Concrete), 1405);
        assert_eq!(ground_sfx_id(GROUND_ROW_SLIDE, GroundSurface::Concrete), 1406);
        assert_eq!(ground_sfx_id(GROUND_ROW_WALK, GroundSurface::Sand), 1407);
        assert_eq!(ground_sfx_id(GROUND_ROW_LAND, GroundSurface::Sand), 1408);
        assert_eq!(ground_sfx_id(GROUND_ROW_SLIDE, GroundSurface::Sand), 1409);
        assert_eq!(ground_sfx_id(GROUND_ROW_KICK, GroundSurface::Concrete), 116);
        // Default falls back to concrete on rows 1-3 where `default == 0`.
        assert_eq!(ground_sfx_id(GROUND_ROW_WALK, GroundSurface::Default), 1404);

        let env = &find(PARAM_ENVSFXPARAM_BIN, 0).expect("envsfxparam row 0").data;
        assert_eq!(env.water_splash_sfx_id, 1419);
        assert_eq!(env.water_fly_out_sfx_id, 1419);
        assert_eq!(env.water_spray_sfx_id, 1420);
    }

    #[test]
    fn particle_material_cache_reuses_identical_handles() {
        let mut cache = HashMap::new();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut images = Assets::<Image>::default();
        let img_a = images.add(Image::default());
        let img_b = images.add(Image::default());

        let h1 = cached_material(&mut cache, Some(img_a.clone()), Blend::Add, Vec4::ONE, &mut materials);
        let h2 = cached_material(&mut cache, Some(img_a.clone()), Blend::Add, Vec4::ONE, &mut materials);
        assert_eq!(h1.id(), h2.id(), "identical (texture, blend, color) must share one material");
        assert_eq!(materials.len(), 1);

        // Different blend mode -> distinct material.
        let h3 = cached_material(&mut cache, Some(img_a.clone()), Blend::Alpha, Vec4::ONE, &mut materials);
        assert_ne!(h1.id(), h3.id());
        assert_eq!(materials.len(), 2);

        // Different texture -> distinct material.
        let h4 = cached_material(&mut cache, Some(img_b), Blend::Add, Vec4::ONE, &mut materials);
        assert_ne!(h1.id(), h4.id());
        assert_eq!(materials.len(), 3);
    }

    #[test]
    fn circle_and_sphere_emitters_read_disc_dust_effects() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let Ok(disc) = acvd_formats::vfs::Disc::default_for(root) else {
            return;
        };
        let load = |id: i32| {
            let bytes = acvd_formats::vfs::open(&disc, &format!("{}|f{id:07}.ffx", super::EFFECTS))
                .expect("ffx entry");
            let eff = acvd_formats::ffx::read(&bytes).expect("parse ffx");
            eff.states
                .first()
                .into_iter()
                .flat_map(|s| &s.actions)
                .flat_map(|a| super::nodes(&a.params))
                .collect::<Vec<_>>()
        };
        fn find_any(
            list: &[std::sync::Arc<super::NodeDef>],
            f: &dyn Fn(&super::NodeDef) -> bool,
        ) -> bool {
            list.iter().any(|n| {
                f(n) || find_any(&n.children, f)
                    || matches!(&n.kind, super::Kind::Spawner { child, .. } if find_any(child, f))
            })
        }

        let walk = load(1404);
        assert!(
            find_any(&walk, &|n| {
                matches!(
                    &n.kind,
                    super::Kind::Cluster(c)
                        if matches!(&c.emitter.shape, super::EmitterShape::Circle { radius }
                            if (radius.at(0.0) - 2.0).abs() < 0.01)
                )
            }),
            "f0001404 walk dust must parse action 30 Circle emitter (radius 2.0 m)"
        );

        let kick = load(116);
        assert!(
            find_any(&kick, &|n| {
                matches!(
                    &n.kind,
                    super::Kind::Cluster(c)
                        if matches!(&c.emitter.shape, super::EmitterShape::Sphere { .. })
                )
            }),
            "f0000116 kick dust must parse action 31 Sphere emitter"
        );
    }

    #[test]
    fn effect_timing_matches_360_node_lifecycle() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let Ok(disc) = acvd_formats::vfs::Disc::default_for(root) else {
            return;
        };
        let load = |id: i32| {
            let bytes = acvd_formats::vfs::open(&disc, &format!("{}|f{id:07}.ffx", super::EFFECTS))
                .expect("ffx entry");
            let eff = acvd_formats::ffx::read(&bytes).expect("parse ffx");
            eff.states
                .first()
                .into_iter()
                .flat_map(|s| &s.actions)
                .flat_map(|a| super::nodes(&a.params))
                .collect::<Vec<_>>()
        };

        // Helper that simulates a parsed FFX tree in a headless Bevy World for `secs` at 60 Hz,
        // returning `(still_alive, active_particle_count)` per root node.
        let run_for = |id: i32, secs: f32| -> (usize, usize) {
            let roots = load(id);
            let mut world = bevy::prelude::World::new();
            world.insert_resource(Assets::<bevy::prelude::Mesh>::default());
            world.insert_resource(Assets::<StandardMaterial>::default());
            world.insert_resource(Assets::<Image>::default());
            let mut lib = super::Library {
                disc: disc.clone(),
                effects: HashMap::new(),
                textures: HashMap::new(),
                models: HashMap::new(),
                shared_materials: HashMap::new(),
                packs: acvd_render::Packs::default(),
                quad: world
                    .resource_mut::<Assets<bevy::prelude::Mesh>>()
                    .add(bevy::prelude::Rectangle::new(1.0, 1.0)),
                haze: world
                    .resource_mut::<Assets<bevy::prelude::Mesh>>()
                    .add(bevy::prelude::Rectangle::new(1.0, 1.0)),
                dot: world
                    .resource_mut::<Assets<Image>>()
                    .add(Image::default()),
                seed: 0x2545_f491,
            };
            let mut rng = super::Rng(0x1234_5679);
            let mut nodes: Vec<_> = roots
                .iter()
                .map(|d| super::NodeRt::new(d.clone(), &mut rng, bevy::prelude::Quat::IDENTITY))
                .collect();
            let owner = world.spawn_empty().id();
            let steps = (secs * 60.0).round() as u32;
            let dt = 1.0 / 60.0;
            for _ in 0..steps {
                let mut meshes = world.remove_resource::<Assets<bevy::prelude::Mesh>>().unwrap();
                let mut materials = world.remove_resource::<Assets<StandardMaterial>>().unwrap();
                let mut images = world.remove_resource::<Assets<Image>>().unwrap();
                let mut commands = world.commands();
                let mut cx = super::Cx {
                    commands: &mut commands,
                    lib: &mut lib,
                    meshes: &mut meshes,
                    materials: &mut materials,
                    images: &mut images,
                    camera: bevy::prelude::Vec3::ZERO,
                    camera_rot: bevy::prelude::Quat::IDENTITY,
                    owner,
                    moves: Vec::new(),
                };
                nodes.retain_mut(|n| {
                    let live = n.step(bevy::math::Affine3A::IDENTITY, dt, false, &mut rng, &mut cx);
                    if !live {
                        n.clear(&mut cx);
                    }
                    live
                });
                world.flush();
                world.insert_resource(meshes);
                world.insert_resource(materials);
                world.insert_resource(images);
            }
            fn count_particles(n: &super::NodeRt) -> usize {
                n.particles.len() + n.children.iter().map(count_particles).sum::<usize>()
            }
            let particles = nodes.iter().map(count_particles).sum();
            (nodes.len(), particles)
        };

        // 1. f0000098 (idle exhaust puff): parent 2101 life=1.0s stops child 2023 smoke;
        //    emits during [0.083s..1.0s] and finishes completely by 2.5s.
        let (alive_mid, parts_mid) = run_for(98, 0.5);
        assert!(alive_mid > 0 && parts_mid > 0, "f0000098 should emit smoke at 0.5s");
        let (alive_end, parts_end) = run_for(98, 2.5);
        assert_eq!(
            (alive_end, parts_end),
            (0, 0),
            "f0000098 idle exhaust puff must stop when parent 2101 (1.0s) expires"
        );

        // 2. f0001404 (walk step dust): parent 2101 life=0.0833s stops child 2023 dust;
        //    finishes completely by 2.0s instead of leaking forever.
        let (walk_mid, walk_parts) = run_for(1404, 0.2);
        assert!(walk_mid > 0 && walk_parts > 0, "f0001404 should have active dust at 0.2s");
        let (walk_end, walk_parts_end) = run_for(1404, 2.0);
        assert_eq!(
            (walk_end, walk_parts_end),
            (0, 0),
            "f0001404 walk dust must finish after parent 2101 (0.083s) + particle life"
        );

        // 3. f0001405 (landing dust): parent 2101 life=0.1667s stops child 2023 dust;
        //    finishes completely by 2.0s.
        let (land_end, land_parts_end) = run_for(1405, 2.0);
        assert_eq!(
            (land_end, land_parts_end),
            (0, 0),
            "f0001405 landing dust must finish after parent 2101 (0.167s) + particle life"
        );

        // 4. f0000116 (wall-kick dust): finite emit_for on infinite-life 2023 must despawn.
        let (kick_end, kick_parts_end) = run_for(116, 4.0);
        assert_eq!(
            (kick_end, kick_parts_end),
            (0, 0),
            "f0000116 wall-kick dust must finish instead of leaking"
        );

        // 5. f0000300 (main booster): Node(2034) smoke cluster has emit_for=0.5s, life=2.0s
        //    (`0x83190c50`), so by 2.5s only the infinite booster flame/heat-haze nodes remain
        //    and no action-71 smoke cluster is still alive.
        let roots_300 = load(300);
        assert!(
            roots_300.iter().any(|n| matches!(&n.kind, super::Kind::Cluster(c) if (c.emit_for - 0.5).abs() < 0.01)),
            "f0000300 should contain Node(2034) smoke cluster with emit_for=0.5s"
        );
        let (b_alive, _) = run_for(300, 2.5);
        assert_eq!(
            b_alive, 2,
            "f0000300 at 2.5s should keep only its 2 infinite booster nodes (flame + haze), not the 0.5s smoke puff or 0.33s flash"
        );
    }

    #[test]
    fn glide_ribbon_reads_f0000134_action_40() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let Ok(disc) = acvd_formats::vfs::Disc::default_for(root) else {
            return;
        };
        let bytes = acvd_formats::vfs::open(&disc, &format!("{}|f0000134.ffx", super::EFFECTS))
            .expect("f0000134 ffx entry");
        let eff = acvd_formats::ffx::read(&bytes).expect("parse f0000134");
        let roots: Vec<_> = eff
            .states
            .first()
            .into_iter()
            .flat_map(|s| &s.actions)
            .flat_map(|a| super::nodes(&a.params))
            .collect();
        fn find_ribbon<'a>(
            ns: &'a [std::sync::Arc<super::NodeDef>],
        ) -> Option<&'a super::Look> {
            for n in ns {
                if let Some(l @ super::Look::Ribbon { .. }) = &n.look {
                    return Some(l);
                }
                if let Some(l) = find_ribbon(&n.children) {
                    return Some(l);
                }
            }
            None
        }
        let Some(super::Look::Ribbon {
            texture,
            blend,
            width,
            interval,
            life,
            segments,
            fade,
            color,
        }) = find_ribbon(&roots)
        else {
            panic!("f0000134 must parse an action-40 ribbon node");
        };
        assert_eq!(*texture, 1847, "s1847 cloud texture");
        assert_eq!(*blend, super::Blend::Add);
        assert!((width.at(0.0) - 0.06).abs() < 1e-4, "0.06 m wide");
        assert!((interval - 1.0 / 60.0).abs() < 1e-5, "one tick sample");
        assert!((life - 1.0).abs() < 1e-5, "1 s trail life");
        assert_eq!(*segments, 16);
        assert!((*fade - 0.5).abs() < 1e-5, "0.5 s segment alpha fade (slot 15)");
        let c = color.at(0.0);
        assert!(
            (c.x - 1.0).abs() < 1e-3 && (c.w - 0.235).abs() < 1e-3,
            "white alpha 0.235, got {c:?}"
        );
    }

    #[test]
    fn ribbon_batch_fades_and_sizes_each_sample_by_its_age() {
        // Width 0 m at age 0 -> 1 m at age 1 s; constant white.
        let width = Curve {
            keys: vec![(0.0, 0.0), (1.0, 1.0)],
            step: false,
            cyclic: false,
        };
        let color = ColorCurve {
            keys: vec![(0.0, Vec4::new(1.0, 1.0, 1.0, 0.8))],
            step: false,
            cyclic: false,
        };
        // History at ages 1.0, 0.5 and 0.0 s behind the head (t = 1.0), straight along -Z.
        let pts = [
            (0.0, Vec3::new(0.0, 0.0, 0.0)),
            (0.5, Vec3::new(0.0, 0.0, -1.0)),
            (1.0, Vec3::new(0.0, 0.0, -2.0)),
        ];
        let b = ribbon_batch(
            &pts,
            Vec3::ZERO,
            1.0,
            1.0,
            0.5,
            2,
            &width,
            &color,
            1.0,
            Vec3::new(0.0, 10.0, 0.0),
        );
        assert_eq!(b.pos.len(), 6, "3 samples x 2 side vertices");
        assert_eq!(b.uv.len(), 6);
        let width_at = |i: usize| {
            (Vec3::from_array(b.pos[i * 2]) - Vec3::from_array(b.pos[i * 2 + 1])).length()
        };
        // Width from the per-sample age, not the node's current age (which would be 0 m).
        assert!((width_at(0) - 1.0).abs() < 1e-4, "age 1.0 s -> 1 m");
        assert!((width_at(1) - 0.5).abs() < 1e-4, "age 0.5 s -> 0.5 m");
        assert!((width_at(2) - 0.0).abs() < 1e-4, "age 0 s -> 0 m");
        // Alpha `1 - age / 0.5 s`, clamped: the two older samples are fully faded.
        assert!((b.color[0][3] - 0.0).abs() < 1e-4);
        assert!((b.color[2][3] - 0.0).abs() < 1e-4);
        assert!((b.color[4][3] - 0.8).abs() < 1e-4);
        // UV: U across the width, V advances with age (0 newest -> 1 oldest).
        assert_eq!(b.uv[0], [0.0, 1.0]);
        assert_eq!(b.uv[1], [1.0, 1.0]);
        assert_eq!(b.uv[4], [0.0, 0.0]);
        assert_eq!(b.uv[5], [1.0, 0.0]);
    }
}
