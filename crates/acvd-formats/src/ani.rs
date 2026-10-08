//! `.ani` skeletal motion clips (`model/**/*_a.bnd.dcx`, cutscene and camera binders), big-endian.
//!
//! Header: u32 magic, u32 0, u32 frame count, u32 bone table offset, u32 bone count, u32 vector
//! pool offset, u32 key pool offset, u32 vector count, u32 key count, u32 key pool end rounded
//! to 4 (older clips) or 0, u32 0, u32 quantized flag, 4 flag bytes (quantized, quantized,
//! scale quantized, 0). The vector pool holds f32 triples; the key pool i16 triples.
//! Translations and their tangents always index the vector pool. Rotations index the key pool
//! when quantized, scales when scale quantized (/ 1000); the vector pool otherwise. Rotations are quaternion x,y,z (w >= 0 implied; quantized / 32767), except in
//! older clips, which store euler radians (Ry·Rz·Rx) quantized / 1000.
//!
//! Named clips (`0x60100910`, older `0x20051014`) carry a 0x40-byte entry per bone: name
//! offset, kind, u16 index, parent, first child, next sibling, rest translation / euler
//! (Ry·Rz·Rx) / scale, track offset, camera block offset. Unnamed clips (`0xA0100910`) carry
//! only an 8-byte entry per bone (track offset, 0); their bones follow a named skeleton of the
//! same motion set by index.
//!
//! A zero track offset marks an unanimated bone (read as an empty track of kind 0).
//! Track: u32 row offset, u32 row count, u32 kind, 6 f32 cached values, u32 0. Each row is a
//! u16 frame followed by pool indices whose width and channels the kind selects (`LAYOUTS`);
//! rows are padded to 4 bytes. Tangents are per-frame derivatives (cubic Hermite). Two rows
//! may share a frame: a step, sampled as the later row from that frame on.

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use crate::reader::Be;

pub const NAMED: u32 = 0x6010_0910;
pub const UNNAMED: u32 = 0xA010_0910;
pub const OLDER: u32 = 0x2005_1014;
const NAMED_ENTRY: usize = 0x40;
const UNNAMED_ENTRY: usize = 8;
const NONE: u16 = 0xFFFF;

/// Row layout for one track kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Layout {
    pub kind: u32,
    /// Index width in bytes.
    pub width: usize,
    /// Translation value, in tangent, out tangent.
    pub translation: bool,
    /// Rotation in / out tangents (Hermite); linear otherwise.
    pub rotation_tangents: bool,
    pub scale: bool,
}

impl Layout {
    pub fn fields(&self) -> usize {
        1 + 3 * self.translation as usize
            + 2 * self.rotation_tangents as usize
            + self.scale as usize
    }
    pub fn row_size(&self) -> usize {
        (2 + self.width * self.fields()).next_multiple_of(4)
    }
}

const fn layout(
    kind: u32,
    width: usize,
    translation: bool,
    rotation_tangents: bool,
    scale: bool,
) -> Layout {
    Layout {
        kind,
        width,
        translation,
        rotation_tangents,
        scale,
    }
}

pub const LAYOUTS: [Layout; 6] = [
    layout(9, 2, false, false, false),
    layout(4, 2, false, true, false),
    layout(7, 2, true, false, true),
    layout(2, 2, true, true, true),
    layout(6, 1, true, false, false),
    layout(1, 1, true, true, false),
];

pub fn layout_of(kind: u32) -> Option<Layout> {
    LAYOUTS.iter().copied().find(|l| l.kind == kind)
}

pub fn magic(data: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(..4)?.try_into().ok()?))
}

pub fn is_ani(data: &[u8]) -> bool {
    matches!(magic(data), Some(NAMED | UNNAMED | OLDER))
}

/// Quaternion (x, y, z, w) of euler radians applied as Ry·Rz·Rx.
pub fn euler_quat([x, y, z]: [f32; 3]) -> [f32; 4] {
    let mul = |a: [f32; 4], b: [f32; 4]| {
        [
            a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
            a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
            a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
            a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
        ]
    };
    let (sx, cx) = (x / 2.0).sin_cos();
    let (sy, cy) = (y / 2.0).sin_cos();
    let (sz, cz) = (z / 2.0).sin_cos();
    mul(
        mul([0.0, sy, 0.0, cy], [0.0, 0.0, sz, cz]),
        [sx, 0.0, 0.0, cx],
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Rest {
    pub name: String,
    pub kind: u32,
    pub parent: Option<u16>,
    pub translation: [f32; 3],
    pub euler: [f32; 3],
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Key {
    pub frame: u16,
    /// Value, in tangent, out tangent.
    pub translation: Option<[[f32; 3]; 3]>,
    /// Quaternion x, y, z (w >= 0 implied), or euler radians when the track says so.
    pub rotation: [f32; 3],
    /// In and out tangents of `rotation`.
    pub rotation_tangents: Option<[[f32; 3]; 2]>,
    pub scale: Option<[f32; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    pub kind: u32,
    /// Rotations are euler radians (Ry·Rz·Rx) rather than quaternion x, y, z.
    pub euler: bool,
    pub keys: Vec<Key>,
}

impl Track {
    /// Bracketing keys and the span-local parameter at `frame`.
    fn bracket(&self, frame: f32) -> Option<(&Key, &Key, f32, f32)> {
        let last = self.keys.len().checked_sub(1)?;
        let i = self.keys.partition_point(|k| k.frame as f32 <= frame);
        let (a, b) = (
            &self.keys[i.saturating_sub(1).min(last)],
            &self.keys[i.min(last)],
        );
        let span = b.frame as f32 - a.frame as f32;
        let t = if span > 0.0 {
            ((frame - a.frame as f32) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some((a, b, t, span))
    }

    /// Local rotation quaternion (x, y, z, w) at `frame`.
    pub fn rotation(&self, frame: f32) -> Option<[f32; 4]> {
        let (a, b, t, span) = self.bracket(frame)?;
        let xyz = match (a.rotation_tangents, b.rotation_tangents) {
            (Some(ta), Some(tb)) => hermite(a.rotation, ta[1], b.rotation, tb[0], t, span),
            _ => std::array::from_fn(|i| a.rotation[i] + (b.rotation[i] - a.rotation[i]) * t),
        };
        Some(if self.euler {
            euler_quat(xyz)
        } else {
            quat(xyz)
        })
    }

    /// Local translation at `frame`, if the track carries one.
    pub fn translation(&self, frame: f32) -> Option<[f32; 3]> {
        let (a, b, t, span) = self.bracket(frame)?;
        let (ta, tb) = (a.translation?, b.translation?);
        Some(hermite(ta[0], ta[2], tb[0], tb[1], t, span))
    }

    /// Local scale at `frame`, if the track carries one.
    pub fn scale(&self, frame: f32) -> Option<[f32; 3]> {
        let (a, b, t, _) = self.bracket(frame)?;
        let (sa, sb) = (a.scale?, b.scale?);
        Some(std::array::from_fn(|i| sa[i] + (sb[i] - sa[i]) * t))
    }
}

fn hermite(p0: [f32; 3], m0: [f32; 3], p1: [f32; 3], m1: [f32; 3], t: f32, span: f32) -> [f32; 3] {
    let (t2, t3) = (t * t, t * t * t);
    let (h00, h10, h01, h11) = (
        2.0 * t3 - 3.0 * t2 + 1.0,
        t3 - 2.0 * t2 + t,
        -2.0 * t3 + 3.0 * t2,
        t3 - t2,
    );
    std::array::from_fn(|i| h00 * p0[i] + h10 * span * m0[i] + h01 * p1[i] + h11 * span * m1[i])
}

/// Unit quaternion from stored x, y, z with w >= 0.
pub fn quat([x, y, z]: [f32; 3]) -> [f32; 4] {
    let q = [x, y, z, (1.0 - x * x - y * y - z * z).max(0.0).sqrt()];
    let n = q
        .iter()
        .map(|c| c * c)
        .sum::<f32>()
        .sqrt()
        .max(f32::EPSILON);
    q.map(|c| c / n)
}

#[derive(Debug, Clone, Serialize)]
pub struct Bone {
    /// Present in named clips only.
    pub rest: Option<Rest>,
    pub track: Track,
    /// Six f32 stored after the kind (cached per-track values; not needed to sample).
    pub cached: [f32; 6],
    /// Offset of a camera block (field of view first), camera clips only.
    pub camera: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Anim {
    pub magic: u32,
    pub frames: u32,
    pub quantized: bool,
    pub scale_quantized: bool,
    pub euler: bool,
    pub vectors: usize,
    pub keys: usize,
    pub bones: Vec<Bone>,
}

impl Anim {
    pub fn named(&self) -> bool {
        self.magic != UNNAMED
    }
}

pub fn read(data: &[u8]) -> Result<Anim> {
    let r = Be(data);
    ensure!(
        is_ani(data),
        "not an .ani clip (magic {:#010x})",
        r.u32(0).unwrap_or(0)
    );
    let magic = r.u32(0)?;
    let euler = magic == OLDER;
    let (frames, table, count) = (r.u32(8)?, r.u32(0x0C)? as usize, r.u32(0x10)? as usize);
    let (vpool, kpool) = (r.u32(0x14)? as usize, r.u32(0x18)? as usize);
    let (vectors, keys) = (r.u32(0x1C)? as usize, r.u32(0x20)? as usize);
    let key_end = if euler {
        (kpool + keys * 6).next_multiple_of(4)
    } else {
        0
    };
    for (at, want) in [(0x04, 0), (0x24, key_end), (0x28, 0)] {
        ensure!(
            r.u32(at)? as usize == want,
            "header word {at:#x} is {:#x}, expected {want:#x}",
            r.u32(at)?
        );
    }
    let (quantized, scale_quantized) = match (r.u32(0x2C)?, r.bytes(0x30, 3)?) {
        (1, [1, 1, s @ (0 | 1)]) => (true, *s == 1),
        (0, [0, 0, 0]) => (false, false),
        (w, b) => bail!("quantization flags {w:#x} {b:02x?} not seen on the disc"),
    };
    let named = magic != UNNAMED;
    let vec3 = |i: usize| -> Result<[f32; 3]> {
        ensure!(i < vectors, "vector index {i} past pool of {vectors}");
        let at = vpool + i * 12;
        Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?])
    };
    let key = |i: usize, scale: f32, quantized: bool| -> Result<[f32; 3]> {
        if !quantized {
            return vec3(i);
        }
        ensure!(i < keys, "key index {i} past pool of {keys}");
        let at = kpool + i * 6;
        Ok([r.i16(at)?, r.i16(at + 2)?, r.i16(at + 4)?].map(|c| c as f32 / scale))
    };
    let rot = |i| key(i, if euler { 1000.0 } else { 32767.0 }, quantized);
    let scl = |i| key(i, 1000.0, scale_quantized);

    let mut bones = Vec::with_capacity(count);
    for b in 0..count {
        let (rest, track_at, camera) = if named {
            let e = table + b * NAMED_ENTRY;
            ensure!(
                r.u16(e + 8)? as usize == b,
                "bone {b} entry carries index {}",
                r.u16(e + 8)?
            );
            let f3 = |at: usize| -> Result<[f32; 3]> {
                Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?])
            };
            let parent = r.u16(e + 0x0C)?;
            ensure!(
                parent == NONE || (parent as usize) < count,
                "bone {b} parent {parent} out of range"
            );
            let rest = Rest {
                name: r.cstr_sjis(r.u32(e)? as usize)?,
                kind: r.u32(e + 4)?,
                parent: (parent != NONE).then_some(parent),
                translation: f3(e + 0x14)?,
                euler: f3(e + 0x20)?,
                scale: f3(e + 0x2C)?,
            };
            (
                Some(rest),
                r.u32(e + 0x38)? as usize,
                Some(r.u32(e + 0x3C)?).filter(|&c| c != 0),
            )
        } else {
            let e = table + b * UNNAMED_ENTRY;
            ensure!(
                r.u32(e + 4)? == 0,
                "bone {b} entry word 4 is {:#x}",
                r.u32(e + 4)?
            );
            (None, r.u32(e)? as usize, None)
        };
        if track_at == 0 {
            bones.push(Bone {
                rest,
                track: Track {
                    kind: 0,
                    euler,
                    keys: Vec::new(),
                },
                cached: [0.0; 6],
                camera,
            });
            continue;
        }
        let (rows, n, kind) = (
            r.u32(track_at)? as usize,
            r.u32(track_at + 4)? as usize,
            r.u32(track_at + 8)?,
        );
        let cached: [f32; 6] =
            std::array::from_fn(|i| r.f32(track_at + 12 + 4 * i).unwrap_or(f32::NAN));
        ensure!(
            r.u32(track_at + 36)? == 0,
            "bone {b} track word 0x24 is {:#x}",
            r.u32(track_at + 36)?
        );
        let Some(l) = layout_of(kind) else {
            bail!("bone {b} track kind {kind} has no known row layout")
        };
        let mut list = Vec::with_capacity(n);
        for k in 0..n {
            let at = rows + k * l.row_size();
            let mut f = 0;
            let mut next = || -> Result<usize> {
                let p = at + 2 + f * l.width;
                f += 1;
                Ok(if l.width == 1 {
                    r.u8(p)? as usize
                } else {
                    r.u16(p)? as usize
                })
            };
            let translation = if l.translation {
                Some([vec3(next()?)?, vec3(next()?)?, vec3(next()?)?])
            } else {
                None
            };
            let rotation = rot(next()?)?;
            let rotation_tangents = if l.rotation_tangents {
                Some([rot(next()?)?, rot(next()?)?])
            } else {
                None
            };
            let scale = if l.scale { Some(scl(next()?)?) } else { None };
            let frame = r.u16(at)?;
            ensure!(
                list.last().is_none_or(|p: &Key| frame >= p.frame),
                "bone {b} key frames decrease at {frame}"
            );
            ensure!(
                frame as u32 <= frames,
                "bone {b} key frame {frame} past clip length {frames}"
            );
            list.push(Key {
                frame,
                translation,
                rotation,
                rotation_tangents,
                scale,
            });
        }
        bones.push(Bone {
            rest,
            track: Track {
                kind,
                euler,
                keys: list,
            },
            cached,
            camera,
        });
    }
    Ok(Anim {
        magic,
        frames,
        quantized,
        scale_quantized,
        euler,
        vectors,
        keys,
        bones,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAC_1_SQRT_2: f32 = std::f32::consts::FRAC_1_SQRT_2;

    /// One-bone named clip with two keys (frames 0 and 10) of the given kind.
    /// Vectors: (0,0,0), (1,2,3), (0.1,0,0). Keys: identity, x -90 deg, scale 1.
    fn clip(kind: u32) -> Vec<u8> {
        let mut d = vec![0u8; 0x200];
        let w =
            |d: &mut Vec<u8>, at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_be_bytes());
        let h =
            |d: &mut Vec<u8>, at: usize, v: u16| d[at..at + 2].copy_from_slice(&v.to_be_bytes());
        for (at, v) in [
            (0, 0x6010_0910),
            (8, 10),
            (0x0C, 0x40),
            (0x10, 1),
            (0x14, 0x100),
            (0x18, 0x140),
            (0x1C, 3),
            (0x20, 3),
            (0x2C, 1),
        ] {
            w(&mut d, at, v);
        }
        d[0x30..0x33].copy_from_slice(&[1, 1, 1]);
        w(&mut d, 0x40, 0x180);
        h(&mut d, 0x4C, NONE);
        d[0x180..0x184].copy_from_slice(b"rt\0\0");
        w(&mut d, 0x78, 0x90);
        w(&mut d, 0x90, 0xC0);
        w(&mut d, 0x94, 2);
        w(&mut d, 0x98, kind);
        for (i, v) in [1.0f32, 2.0, 3.0, 0.1].iter().enumerate() {
            d[0x10C + 4 * i..0x110 + 4 * i].copy_from_slice(&v.to_be_bytes());
        }
        h(&mut d, 0x146, (-23169i16) as u16);
        for i in 0..3 {
            h(&mut d, 0x14C + 2 * i, 1000);
        }
        let l = layout_of(kind).unwrap();
        // per key: translation (value, in, out), rotation, rotation tangents (zero), scale
        for (k, (t, rot)) in [(0usize, 0usize), (1, 1)].into_iter().enumerate() {
            let at = 0xC0 + k * l.row_size();
            h(&mut d, at, (k * 10) as u16);
            let mut fields = Vec::new();
            if l.translation {
                fields.extend([t, 0, 0]);
            }
            fields.push(rot);
            if l.rotation_tangents {
                fields.extend([0, 0]);
            }
            if l.scale {
                fields.push(2);
            }
            for (i, v) in fields.into_iter().enumerate() {
                if l.width == 1 {
                    d[at + 2 + i] = v as u8;
                } else {
                    h(&mut d, at + 2 + 2 * i, v as u16);
                }
            }
        }
        d
    }

    #[test]
    fn every_layout_reads_and_samples() {
        for l in LAYOUTS {
            let a = read(&clip(l.kind)).unwrap();
            assert_eq!(
                (a.frames, a.bones.len(), a.quantized),
                (10, 1, true),
                "kind {}",
                l.kind
            );
            let tr = &a.bones[0].track;
            let q = tr.rotation(10.0).unwrap();
            assert!(
                (q[0] + FRAC_1_SQRT_2).abs() < 1e-4 && (q[3] - FRAC_1_SQRT_2).abs() < 1e-4,
                "kind {} {q:?}",
                l.kind
            );
            assert_eq!(
                tr.translation(10.0),
                l.translation.then_some([1.0, 2.0, 3.0])
            );
            assert_eq!(tr.scale(5.0), l.scale.then_some([1.0; 3]));
        }
    }

    #[test]
    fn hermite_uses_per_frame_tangents() {
        let k = |frame, p: f32, m: f32| Key {
            frame,
            translation: Some([[p, 0.0, 0.0], [m, 0.0, 0.0], [m, 0.0, 0.0]]),
            rotation: [0.0; 3],
            rotation_tangents: None,
            scale: None,
        };
        let tr = Track {
            kind: 6,
            euler: false,
            keys: vec![k(0, 0.0, 0.1), k(10, 1.0, 0.1)],
        };
        assert!((tr.translation(5.0).unwrap()[0] - 0.5).abs() < 1e-6);
        assert!((tr.translation(2.0).unwrap()[0] - 0.2).abs() < 1e-6);
        assert_eq!(
            Track {
                kind: 9,
                euler: false,
                keys: vec![]
            }
            .rotation(0.0),
            None
        );
    }

    #[test]
    fn older_clips_store_euler_rotations() {
        let mut d = clip(9);
        d[0..4].copy_from_slice(&OLDER.to_be_bytes());
        d[0x24..0x28].copy_from_slice(&(0x140u32 + 3 * 6).next_multiple_of(4).to_be_bytes());
        d[0x32] = 0;
        d[0x146..0x148].copy_from_slice(&(-1571i16).to_be_bytes());
        let a = read(&d).unwrap();
        assert!(a.euler && a.named());
        let q = a.bones[0].track.rotation(10.0).unwrap();
        assert!(
            (q[0] + FRAC_1_SQRT_2).abs() < 1e-3 && (q[3] - FRAC_1_SQRT_2).abs() < 1e-3,
            "{q:?}"
        );
        let yz = euler_quat([0.0, 0.5, 0.25]);
        let expect = [
            (0.25f32).sin() * (0.125f32).sin(),
            (0.25f32).sin() * (0.125f32).cos(),
            (0.25f32).cos() * (0.125f32).sin(),
            (0.25f32).cos() * (0.125f32).cos(),
        ];
        assert!(
            yz.iter().zip(expect).all(|(a, b)| (a - b).abs() < 1e-6),
            "{yz:?}"
        );
    }

    #[test]
    fn rejects_unknown_track_kind() {
        let mut d = clip(9);
        d[0x9B] = 5;
        assert!(read(&d).is_err());
    }
}
