//! FFX particle effects (`sfx/*.ffxbnd` entries `fNNNNNNN.ffx`): the little-endian "DLsE"
//! serialised `FXSerializableEffect` tree, the same container SoulsFormats reads as `FFXDLSE`.
//!
//! Header: `"DLsE", u8 1, u8 3, u16 0, u32 0, u32 0, u8 0, u32 1, i16 class_count`, then
//! `class_count` × `(u32 len, ASCII name)`. Every object then opens with
//! `i16 class (1-based index into the names), u32 version, u32 length` (length counts from the
//! class index), except `DLVector`, which is `i16 class, u32 count, i32 × count`.
//!
//! - Effect (v5): `u32 0, i32 id, u32 0, u32 0, u32 2, u16 0, i16 2, u32 0`, two param lists,
//!   a state map, a resource set, `u8 0`.
//! - ParamList (v2): `u32 count, i32 unk, Param × count`.
//! - Param (v2): `i32 kind`, then a kind-specific body (see [`Value`]).
//! - StateMap (v1): `u32 count, State × count`; State (v1): `u32 actions, u32 triggers`,
//!   actions, triggers. Action (v1): `i32 id, ParamList`. Trigger (v1): `i32 state, Eval`.
//! - Evaluatable (v1): `i32 opcode, i32 type`, then per opcode: 1 const `i32`; 2/3 `i32, i32`;
//!   8-15 binary (right operand first); 20 unary; 4/5/21-24 nothing.
//! - ResourceSet (v1): five `DLVector`s; in ACVD the first lists `sfx_m` model ids, the second
//!   `sfx_t` texture ids, the fourth nested action ids and the fifth node template ids.
//! - Primitives (v1): `dl_int32` i32, `dl_float32` / `FXTick` f32 (seconds), `FXColorRGBA`
//!   4 × f32; some colour keys carry an `FXPos` (4 × f32) instead.
//!
//! The top-level action 14 holds param-37 nodes whose `id` is a node template (2101, 2023, ...)
//! and whose params nest param-38 actions (`id` = renderer, emitter or movement type); the
//! interpretation of those ids lives in `acvd-game::sfx` and `sheets/ffx_actions.csv`.

use anyhow::{bail, ensure, Context, Result};

pub const MAGIC: &[u8; 4] = b"DLsE";

#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub id: i32,
    pub params1: ParamList,
    pub params2: ParamList,
    pub states: Vec<State>,
    pub resources: [Vec<i32>; 5],
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParamList {
    pub unk: i32,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub actions: Vec<Action>,
    pub triggers: Vec<Trigger>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub id: i32,
    pub params: ParamList,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub state: i32,
    pub eval: Eval,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Eval {
    Const(i32),
    /// Opcodes 2 and 3: `(unk, arg index)`.
    Arg(i32, i32, i32),
    /// Opcodes 4, 5, 21-24 (current tick, total tick, child exists, parent exists, distance
    /// from camera, emitters stopped).
    Leaf(i32),
    Unary(i32, Box<Eval>),
    /// `(opcode, left, right)`; 8 and, 9 or, 10 ge, 11 gt, 12 le, 13 lt, 14 eq, 15 ne.
    Binary(i32, Box<Eval>, Box<Eval>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub kind: i32,
    pub value: Value,
    /// Bytes left inside the param after its known body (kind 89 carries 5).
    pub extra: Vec<u8>,
}

/// A param body. Sequence kinds follow the runtime class order (`FXFloatSequenceParamReal`,
/// `...Cyclic...`, `...Linear...`, `...LinearCyclic...`, `...Cubic...`, `...CubicCyclic...`):
/// float 9-14, RGBA 17-22, int 3-6 and 89. Cubic keys carry the value and two tangents.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 1
    Int(i32),
    /// 2
    Ints(Vec<i32>),
    /// 3, 5, 6, 89: `(tick, value)` keys.
    IntKeys(Vec<(f32, i32)>),
    /// 7
    Float(f32),
    /// 9-12
    FloatKeys(Vec<(f32, f32)>),
    /// 13, 14: `(tick, [value, tangent, tangent])`.
    CubicKeys(Vec<(f32, [f32; 3])>),
    /// 15
    Color([f32; 4]),
    /// 17-20
    ColorKeys(Vec<(f32, [f32; 4])>),
    /// 21, 22
    CubicColorKeys(Vec<(f32, [[f32; 4]; 3])>),
    /// 37 (node template) and 38 (action): a type id and its params.
    Node(i32, ParamList),
    /// 40 texture id, 41 model id, 68 sound id, 69.
    Id(i32),
    /// 44-47, 59, 60, 66, 71, 87: `(unk, arg index)`, a value the spawner passes in.
    Arg(i32, i32),
    /// 70
    Tick(f32),
    /// 79
    IntPair(i32, i32),
    /// 81: usually a `(min, max)` random range.
    FloatPair(f32, f32),
    /// 82: a param scaled by a random spread.
    Scaled(Box<Param>, f32),
    /// 83
    ColorPair([f32; 4], [f32; 4]),
    /// 84
    ScaledColor(Box<Param>, [f32; 4]),
    /// 85
    TickPair(f32, f32),
    /// Kinds without a known body (4, 91, 93): the raw bytes.
    Raw(Vec<u8>),
}

pub fn is_ffx(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn read(data: &[u8]) -> Result<Effect> {
    ensure!(is_ffx(data), "not a DLsE effect");
    let mut r = Le { d: data, at: 4, names: Vec::new() };
    ensure!(r.bytes(4)? == [1, 3, 0, 0], "DLsE version bytes");
    ensure!(r.u32()? == 0 && r.u32()? == 0 && r.u8()? == 0 && r.u32()? == 1, "DLsE header");
    let count = r.i16()?;
    for _ in 0..count {
        let len = r.u32()? as usize;
        let name = String::from_utf8_lossy(r.bytes(len)?).into_owned();
        r.names.push(name);
    }
    r.effect()
}

struct Le<'a> {
    d: &'a [u8],
    at: usize,
    names: Vec<String>,
}

const PRIM: &str = "FXSerializablePrimitive<";

impl<'a> Le<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let Some(b) = self.at.checked_add(n).and_then(|end| self.d.get(self.at..end)) else {
            bail!("read of {n} bytes at {:#x} runs past end of {:#x}-byte effect", self.at, self.d.len())
        };
        self.at += n;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.bytes(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into()?))
    }

    fn class(&mut self) -> Result<&str> {
        let at = self.at;
        let i = self.i16()?;
        let names = &self.names;
        usize::try_from(i - 1).ok().and_then(|i| names.get(i)).map(String::as_str).with_context(|| format!("class index {i} at {at:#x}"))
    }

    /// Opens an object of class `want`; returns where it must end.
    fn open(&mut self, want: &str) -> Result<usize> {
        let start = self.at;
        let got = self.class()?;
        ensure!(got == want, "expected {want} at {start:#x}, found {got}");
        let _version = self.u32()?;
        let len = self.u32()? as usize;
        Ok(start + len)
    }

    fn close(&mut self, end: usize, what: &str) -> Result<()> {
        ensure!(self.at == end, "{what} ends at {:#x}, read to {:#x}", end, self.at);
        Ok(())
    }

    /// A primitive of any type, as its floats (an `dl_int32` is returned bit-cast).
    fn prim(&mut self) -> Result<(String, Vec<f32>, i32)> {
        let start = self.at;
        let class = self.class()?.to_owned();
        let Some(kind) = class.strip_prefix(PRIM).and_then(|k| k.strip_suffix('>')) else { bail!("expected a primitive at {start:#x}, found {class}") };
        let kind = kind.to_owned();
        let _version = self.u32()?;
        let end = start + self.u32()? as usize;
        let (mut floats, mut int) = (Vec::new(), 0);
        if kind == "dl_int32" {
            int = self.i32()?;
        } else {
            while self.at < end {
                floats.push(self.f32()?);
            }
        }
        self.close(end, &class)?;
        Ok((kind, floats, int))
    }
    fn int(&mut self) -> Result<i32> {
        let (kind, _, v) = self.prim()?;
        ensure!(kind == "dl_int32", "expected dl_int32, found {kind}");
        Ok(v)
    }
    fn float(&mut self) -> Result<f32> {
        let (kind, f, _) = self.prim()?;
        ensure!(f.len() == 1, "expected one float in {kind}, found {}", f.len());
        Ok(f[0])
    }
    fn color(&mut self) -> Result<[f32; 4]> {
        let (kind, f, _) = self.prim()?;
        f.try_into().map_err(|f: Vec<f32>| anyhow::anyhow!("expected four floats in {kind}, found {}", f.len()))
    }
    fn keys<T>(&mut self, mut key: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<(f32, T)>> {
        let n = self.u32()?;
        (0..n).map(|_| Ok((self.float()?, key(self)?))).collect()
    }

    fn list(&mut self) -> Result<ParamList> {
        let end = self.open("FXSerializableParamList")?;
        let n = self.u32()?;
        let unk = self.i32()?;
        let params = (0..n).map(|_| self.param()).collect::<Result<_>>()?;
        self.close(end, "param list")?;
        Ok(ParamList { unk, params })
    }

    fn param(&mut self) -> Result<Param> {
        let end = self.open("FXSerializableParam")?;
        let kind = self.i32()?;
        let value = match kind {
            1 => Value::Int(self.int()?),
            2 => {
                let n = self.u32()?;
                Value::Ints((0..n).map(|_| self.int()).collect::<Result<_>>()?)
            }
            3 | 5 | 6 | 89 => Value::IntKeys(self.keys(Self::int)?),
            7 => Value::Float(self.float()?),
            9..=12 => Value::FloatKeys(self.keys(Self::float)?),
            13 | 14 => Value::CubicKeys(self.keys(|r| Ok([r.float()?, r.float()?, r.float()?]))?),
            15 => Value::Color(self.color()?),
            17..=20 => Value::ColorKeys(self.keys(Self::color)?),
            21 | 22 => Value::CubicColorKeys(self.keys(|r| Ok([r.color()?, r.color()?, r.color()?]))?),
            37 | 38 => Value::Node(self.i32()?, self.list()?),
            40 | 41 | 68 | 69 => Value::Id(self.i32()?),
            44..=47 | 59 | 60 | 66 | 71 | 87 => Value::Arg(self.i32()?, self.i32()?),
            70 => Value::Tick(self.float()?),
            79 => Value::IntPair(self.int()?, self.int()?),
            81 => Value::FloatPair(self.float()?, self.float()?),
            82 => Value::Scaled(Box::new(self.param()?), self.float()?),
            83 => Value::ColorPair(self.color()?, self.color()?),
            84 => Value::ScaledColor(Box::new(self.param()?), self.color()?),
            85 => Value::TickPair(self.float()?, self.float()?),
            _ => Value::Raw(self.bytes(end.saturating_sub(self.at))?.to_vec()),
        };
        ensure!(self.at <= end, "param kind {kind} ends at {end:#x}, read to {:#x}", self.at);
        let extra = self.bytes(end - self.at)?.to_vec();
        Ok(Param { kind, value, extra })
    }

    fn eval(&mut self) -> Result<Eval> {
        let end = self.open("FXSerializableEvaluatable<dl_int32>")?;
        let op = self.i32()?;
        let _ty = self.i32()?;
        let e = match op {
            1 => Eval::Const(self.i32()?),
            2 | 3 => Eval::Arg(op, self.i32()?, self.i32()?),
            4 | 5 | 21..=24 => Eval::Leaf(op),
            8..=15 => {
                let right = self.eval()?;
                Eval::Binary(op, Box::new(self.eval()?), Box::new(right))
            }
            20 => Eval::Unary(op, Box::new(self.eval()?)),
            _ => bail!("evaluatable opcode {op}"),
        };
        self.close(end, "evaluatable")?;
        Ok(e)
    }

    fn dlvector(&mut self) -> Result<Vec<i32>> {
        let class = self.class()?;
        ensure!(class == "DLVector", "expected DLVector, found {class}");
        let n = self.u32()?;
        (0..n).map(|_| self.i32()).collect()
    }

    fn effect(&mut self) -> Result<Effect> {
        let end = self.open("FXSerializableEffect")?;
        ensure!(self.u32()? == 0, "effect word 0");
        let id = self.i32()?;
        let _fixed = [self.u32()?, self.u32()?, self.u32()?];
        let _ = (self.i16()?, self.i16()?, self.u32()?);
        let params1 = self.list()?;
        let params2 = self.list()?;
        let map_end = self.open("FXSerializableStateMap")?;
        let n = self.u32()?;
        let mut states = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let state_end = self.open("FXSerializableState")?;
            let (na, nt) = (self.u32()?, self.u32()?);
            let mut actions = Vec::with_capacity(na as usize);
            for _ in 0..na {
                let e = self.open("FXSerializableAction")?;
                actions.push(Action { id: self.i32()?, params: self.list()? });
                self.close(e, "action")?;
            }
            let mut triggers = Vec::with_capacity(nt as usize);
            for _ in 0..nt {
                let e = self.open("FXSerializableTrigger")?;
                triggers.push(Trigger { state: self.i32()?, eval: self.eval()? });
                self.close(e, "trigger")?;
            }
            self.close(state_end, "state")?;
            states.push(State { actions, triggers });
        }
        self.close(map_end, "state map")?;
        let res_end = self.open("FXResourceSet")?;
        let resources = [self.dlvector()?, self.dlvector()?, self.dlvector()?, self.dlvector()?, self.dlvector()?];
        self.close(res_end, "resource set")?;
        // The effect length is not exact: on sfx/f0004092.ffx it runs 3 bytes past the file.
        let _ = end;
        ensure!(self.u8()? == 0, "effect {id} tail");
        Ok(Effect { id, params1, params2, states, resources })
    }
}

impl ParamList {
    pub fn get(&self, i: usize) -> Option<&Value> {
        self.params.get(i).map(|p| &p.value)
    }
}

impl Effect {
    /// Every param-37 node of every action of every state, outermost first.
    pub fn nodes(&self) -> impl Iterator<Item = (i32, &ParamList)> {
        self.states.iter().flat_map(|s| &s.actions).flat_map(|a| &a.params.params).filter_map(|p| match &p.value {
            Value::Node(id, list) if p.kind == 37 && *id != 0 => Some((*id, list)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct W {
        out: Vec<u8>,
        names: Vec<&'static str>,
    }

    impl W {
        fn class(&mut self, name: &'static str) {
            let i = match self.names.iter().position(|n| *n == name) {
                Some(i) => i,
                None => {
                    self.names.push(name);
                    self.names.len() - 1
                }
            };
            self.out.extend((i as i16 + 1).to_le_bytes());
        }
        fn obj(&mut self, name: &'static str, body: impl FnOnce(&mut W)) {
            let start = self.out.len();
            self.class(name);
            self.out.extend(1u32.to_le_bytes());
            let len_at = self.out.len();
            self.out.extend(0u32.to_le_bytes());
            body(self);
            let len = (self.out.len() - start) as u32;
            self.out[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
        }
        fn u32(&mut self, v: u32) {
            self.out.extend(v.to_le_bytes());
        }
        fn u32s(&mut self, vs: &[u32]) {
            vs.iter().for_each(|&v| self.u32(v));
        }
        fn f32(&mut self, v: f32) {
            self.out.extend(v.to_le_bytes());
        }
        fn list(&mut self, params: impl FnOnce(&mut W), n: u32) {
            self.obj("FXSerializableParamList", |w| {
                w.u32(n);
                w.u32(0);
                params(w);
            });
        }
    }

    #[test]
    fn reads_a_minimal_effect() {
        let mut w = W { out: Vec::new(), names: Vec::new() };
        w.obj("FXSerializableEffect", |w| {
            w.u32(0);
            w.u32(1218);
            w.u32s(&[0, 0, 2]);
            w.out.extend([0, 0, 2, 0]);
            w.u32(0);
            w.list(
                |w| {
                    w.obj("FXSerializableParam", |w| {
                        w.u32(11);
                        w.u32(2);
                        for (t, v) in [(0.0, 1.0), (0.5, 4.0)] {
                            w.obj("FXSerializablePrimitive<FXTick>", |w| w.f32(t));
                            w.obj("FXSerializablePrimitive<dl_float32>", |w| w.f32(v));
                        }
                    });
                    w.obj("FXSerializableParam", |w| {
                        w.u32(38);
                        w.u32(59);
                        w.list(|w| w.obj("FXSerializableParam", |w| w.u32s(&[40, 1036])), 1);
                    });
                },
                2,
            );
            w.list(|_| {}, 0);
            w.obj("FXSerializableStateMap", |w| {
                w.u32(1);
                w.obj("FXSerializableState", |w| {
                    w.u32(1);
                    w.u32(1);
                    w.obj("FXSerializableAction", |w| {
                        w.u32(5);
                        w.list(|_| {}, 0);
                    });
                    w.obj("FXSerializableTrigger", |w| {
                        w.u32(1);
                        w.obj("FXSerializableEvaluatable<dl_int32>", |w| w.u32s(&[1, 3, 7]));
                    });
                });
            });
            w.obj("FXResourceSet", |w| {
                for i in 0..5 {
                    w.class("DLVector");
                    w.u32(1);
                    w.u32(i);
                }
            });
        });
        w.out.push(0);
        let mut file = b"DLsE\x01\x03\0\0".to_vec();
        file.extend([0; 9]);
        file.extend(1u32.to_le_bytes());
        file.extend((w.names.len() as i16).to_le_bytes());
        for n in &w.names {
            file.extend((n.len() as u32).to_le_bytes());
            file.extend(n.as_bytes());
        }
        file.extend(&w.out);
        let e = read(&file).unwrap();
        assert_eq!(e.id, 1218);
        assert_eq!(e.params1.get(0), Some(&Value::FloatKeys(vec![(0.0, 1.0), (0.5, 4.0)])));
        let Some(Value::Node(59, inner)) = e.params1.get(1) else { panic!("{:?}", e.params1) };
        assert_eq!(inner.get(0), Some(&Value::Id(1036)));
        assert_eq!(e.states[0].triggers[0], Trigger { state: 1, eval: Eval::Const(7) });
        assert_eq!(e.resources[4], [4]);
    }

    #[test]
    fn disc_effects() {
        let usrdir = crate::vfs::usrdir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("ACVD Unbound"));
        let path = usrdir.join("sfx/acv_commoneffects.ffxbnd");
        if !path.is_file() {
            return;
        }
        let binder = std::fs::read(&path).unwrap();
        let b = crate::bnd3::read(&binder).unwrap();
        let mut n = 0;
        for entry in &b.entries {
            let data = entry.contents(&binder).unwrap();
            if !is_ffx(&data) {
                continue;
            }
            let name = entry.name.clone().unwrap_or_default();
            let e = read(&data).unwrap_or_else(|err| panic!("{name}: {err:#}"));
            assert!(name.contains(&format!("{:07}", e.id)), "{name} holds effect {}", e.id);
            n += 1;
        }
        assert!(n >= 1800, "{n} effects");
        let muzzle = read(&crate::vfs::open(&usrdir, "sfx/acv_commoneffects.ffxbnd|f0001218.ffx").unwrap()).unwrap();
        assert_eq!(muzzle.resources[1], [1038, 230, 4022, 1036, 220, 242, 232, 233]);
        assert!(muzzle.nodes().any(|(id, _)| id == 2023));
    }
}
