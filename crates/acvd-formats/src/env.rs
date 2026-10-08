//! Map environment (`model/map/ch_env/{map}_env.msb`): the MSB section chain at version
//! `msb::MAGIC_ENV`, with every record in `EVENT_PARAM_ST`. Field layouts and the shader
//! constants built from them are in `sheets/map_env.csv`.
//!
//! Event record: `u32 name, i32 id, u32 type, u32 index, u32 data`, offsets relative to the
//! record. Types: 100 light set, 101 scene parameters, 102 wind, 400 BGM (mission envs only).

use anyhow::{ensure, Result};
use serde::Serialize;

use crate::msb;
use crate::reader::Be;

pub const EVENT_LIGHT: u32 = 100;
pub const EVENT_SCENE: u32 = 101;
pub const EVENT_WIND: u32 = 102;
pub const EVENT_BGM: u32 = 400;

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub name: String,
    pub id: i32,
    pub kind: u32,
    pub index: u32,
    /// Absolute offset of the record's data.
    pub data: usize,
}

/// Fog block of a light set (data `+0xa0..+0xe4`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Fog {
    /// Distance fog colour; `w` is the fog's maximum (`+0xa0`).
    pub color: [f32; 4],
    /// Distance over which distance fog rises (`+0xb0`).
    pub range: f32,
    /// Horizontal distance where distance fog starts (`+0xb4`).
    pub start: f32,
    /// Height-fog falloff divisor, clamped to at least 0.1 (`+0xbc`).
    pub falloff: f32,
    /// Overall fog multiplier on both maxima (`+0xc0`).
    pub density: f32,
    /// How much scene luminance tints the fog colours (`+0xc4`).
    pub luma_tint: f32,
    /// Nonzero: positive height falloff (`+0xc8` byte).
    pub rising: bool,
    /// Height-fog range (`+0xcc`); 0 turns height fog off.
    pub height_range: f32,
    /// World height where height fog starts (`+0xd0`).
    pub height_base: f32,
    /// Height fog colour; `w` is its maximum (`+0xd4`).
    pub height_color: [f32; 4],
}

/// A type-100 event: two directional lights, a hemisphere ambient and a fog block. Draws pick
/// one by `slot`.
#[derive(Debug, Clone, Serialize)]
pub struct LightSet {
    pub name: String,
    /// `group * 32 + id` (data bytes 0 and 1), the key the draw info's light / fog ids select.
    pub slot: u32,
    /// Pitch and yaw in degrees of the two directional lights (`+2`, `+0x28`).
    pub angles: [[i16; 2]; 2],
    /// Directional light colours, RGB and intensity in `w` (`+8`, `+0x2c`).
    pub colors: [[f32; 4]; 2],
    /// Second colour of each light (`+0x18`, `+0x3c`), uploaded to c11 / c12.
    pub colors_b: [[f32; 4]; 2],
    /// Hemisphere ambient: sky (`+0x70`) and ground (`+0x80`) colours, intensity in `w`.
    pub sky: [f32; 4],
    pub ground: [f32; 4],
    /// `+0x90`, uploaded to c39 (unused by the map shader).
    pub extra: [f32; 4],
    pub fog: Fog,
}

impl LightSet {
    /// Unit vector toward light `i`, in game axes: `(-cos p sin y, sin p, -cos p cos y)`.
    pub fn direction(&self, i: usize) -> [f32; 3] {
        let [p, y] = self.angles[i].map(|a| (a as f32).to_radians());
        [-p.cos() * y.sin(), p.sin(), -p.cos() * y.cos()]
    }
}

/// The type-101 scene record's first sub-block.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Scene {
    /// Bytes 0..3 of the first sub-block, RGB 0-255.
    pub clear: [u8; 3],
    /// The first sub-block's `+4`, the view far distance.
    pub far: f32,
    /// `+0x1c`, the sky dome scale (debug label 天球スケール); 0x827ffa80 keeps the default 1
    /// unless it is positive.
    pub sky_scale: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Env {
    pub events: Vec<Event>,
    pub lights: Vec<LightSet>,
    pub scene: Option<Scene>,
}

impl Env {
    pub fn light(&self, slot: u32) -> Option<&LightSet> {
        self.lights.iter().find(|l| l.slot == slot)
    }
}

fn vec4(r: &Be, at: usize) -> Result<[f32; 4]> {
    Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?, r.f32(at + 12)?])
}

fn light(r: &Be, name: String, d: usize) -> Result<LightSet> {
    Ok(LightSet {
        name,
        slot: r.u8(d + 1)? as u32 * 32 + r.u8(d)? as u32,
        angles: [[r.i16(d + 2)?, r.i16(d + 4)?], [r.i16(d + 0x28)?, r.i16(d + 0x2a)?]],
        colors: [vec4(r, d + 8)?, vec4(r, d + 0x2c)?],
        colors_b: [vec4(r, d + 0x18)?, vec4(r, d + 0x3c)?],
        sky: vec4(r, d + 0x70)?,
        ground: vec4(r, d + 0x80)?,
        extra: vec4(r, d + 0x90)?,
        fog: Fog {
            color: vec4(r, d + 0xa0)?,
            range: r.f32(d + 0xb0)?,
            start: r.f32(d + 0xb4)?,
            falloff: r.f32(d + 0xbc)?,
            density: r.f32(d + 0xc0)?,
            luma_tint: r.f32(d + 0xc4)?,
            rising: r.u8(d + 0xc8)? != 0,
            height_range: r.f32(d + 0xcc)?,
            height_base: r.f32(d + 0xd0)?,
            height_color: vec4(r, d + 0xd4)?,
        },
    })
}

pub fn read(data: &[u8]) -> Result<Env> {
    let r = Be(data);
    ensure!(r.u32(0)? == msb::MAGIC_ENV, "not an env MSB");
    let mut env = Env {
        events: Vec::new(),
        lights: Vec::new(),
        scene: None,
    };
    for off in msb::section(data, "EVENT_PARAM_ST")? {
        let name_rel = r.u32(off)? as usize;
        ensure!(name_rel < 0x400, "env event at {off:#x} name offset {name_rel:#x}");
        let e = Event {
            name: r.cstr_sjis(off + name_rel)?,
            id: r.i32(off + 4)?,
            kind: r.u32(off + 8)?,
            index: r.u32(off + 0xc)?,
            data: off + r.u32(off + 0x10)? as usize,
        };
        match e.kind {
            EVENT_LIGHT => env.lights.push(light(&r, e.name.clone(), e.data)?),
            EVENT_SCENE if env.scene.is_none() => {
                let sub = e.data + r.u32(e.data)? as usize;
                env.scene = Some(Scene {
                    clear: [r.u8(sub)?, r.u8(sub + 1)?, r.u8(sub + 2)?],
                    far: r.f32(sub + 4)?,
                    sky_scale: Some(r.f32(sub + 0x1c)?).filter(|&s| s > 0.0).unwrap_or(1.0),
                });
            }
            _ => {}
        }
        env.events.push(e);
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Values the 360 uploaded for m4000's map set (`private/xenia/envlook_mission2.jsonl`).
    #[test]
    fn m4000_env_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else { return };
        let env = read(&crate::vfs::open(&disc, "model/map/ch_env/m4000_env.msb").unwrap()).unwrap();
        assert_eq!(env.events.len(), 21);
        assert_eq!(env.events.iter().filter(|e| e.kind == EVENT_BGM).count(), 1);
        let slots: Vec<u32> = env.lights.iter().map(|l| l.slot).collect();
        assert_eq!(&slots[..6], &[0, 1, 2, 3, 4, 6]);
        assert!(slots.contains(&19));

        let map = env.light(1).unwrap();
        let d = map.direction(0);
        assert!(near(d[0], -0.4777) && near(d[1], 0.6691) && near(d[2], 0.5693), "{d:?}");
        assert_eq!(map.angles, [[42, 140], [-6, 30]]);
        assert_eq!(map.colors[0], [1.5, 1.5, 1.4, 1.6]);
        assert_eq!(map.fog.color, [0.8, 0.8, 0.8, 0.6]);
        assert_eq!((map.fog.start, map.fog.range, map.fog.falloff), (30.0, 375.0, 10.0));
        assert!(map.fog.rising);
        assert_eq!(map.fog.height_range, 0.0);

        let mountain = env.light(2).unwrap();
        assert_eq!((mountain.fog.height_range, mountain.fog.height_base), (20.0, 50.0));
        assert!(near(mountain.fog.height_color[3], 0.14));

        let scene = env.scene.unwrap();
        assert_eq!(scene.clear, [0x25, 0x32, 0x31]);
        assert_eq!(scene.far, 15000.0);
        assert_eq!(scene.sky_scale, 100.0);
    }
}
