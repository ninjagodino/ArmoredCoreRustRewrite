//! Joint-control table `param/jcondata.bin`: named controls (`gun_L`, `gun_R`, `sniper_L`, ...)
//! that pose and kick bones procedurally. The disc's `jcondata.xml` beside it is an older
//! revision (AC4 bone names, other angles); the values here are the 360 build's.
//!
//! Header: `u32 0, u32 control count, u32 control table offset, u32 string offset`. A control is
//! `u32 name, u32 object count, u32 object offset` (from the end of the control table). An
//! object is 0x40 bytes and is the runtime layout the 360 XML reader `0x82b73b28` fills:
//! `u32 bone name, u32 AxisY bone` (string offsets) then the floats below.

use anyhow::{ensure, Result};

use crate::reader::Be;

const OBJECT: usize = 0x40;

#[derive(Debug, Clone)]
pub struct Control {
    pub name: String,
    pub objects: Vec<Object>,
}

/// Angles are degrees, times 60 Hz frames.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    pub bone: String,
    pub axis_bone: String,
    /// +0x08, the `%f` of `AxisY = bone , angle`.
    pub axis_angle: f32,
    /// +0x0c `LockAng`.
    pub lock_ang: f32,
    /// +0x10 / +0x14 `LimitRotAngVelX` / `LimitRotAngVelY`.
    pub limit_rot_vel: [f32; 2],
    /// +0x18 `ReactAng`: the shot kick.
    pub react_ang: f32,
    /// +0x1c `TraceAng`.
    pub trace_ang: f32,
    /// +0x20 `ReactDelay`.
    pub react_delay: f32,
    /// +0x24 `ReactTime`.
    pub react_time: f32,
    /// +0x28..+0x3c `LockMinX/MaxX/MinY/MaxY/MinZ/MaxZ`.
    pub lock: [[f32; 2]; 3],
}

pub fn read(data: &[u8]) -> Result<Vec<Control>> {
    let b = Be(data);
    let count = b.u32(4)? as usize;
    let table = b.u32(8)? as usize;
    let strings = b.u32(0xc)? as usize;
    ensure!(count < 0x1000, "jcondata control count {count}");
    let objects_at = table + count * 12;
    let text = |off: u32| b.cstr_sjis(strings + off as usize);
    let mut controls = Vec::with_capacity(count);
    for i in 0..count {
        let at = table + i * 12;
        let n = b.u32(at + 4)? as usize;
        let first = objects_at + b.u32(at + 8)? as usize;
        let mut objects = Vec::with_capacity(n);
        for j in 0..n {
            let o = first + j * OBJECT;
            let f = |k: usize| b.f32(o + 8 + 4 * k);
            objects.push(Object {
                bone: text(b.u32(o)?)?,
                axis_bone: text(b.u32(o + 4)?)?,
                axis_angle: f(0)?,
                lock_ang: f(1)?,
                limit_rot_vel: [f(2)?, f(3)?],
                react_ang: f(4)?,
                trace_ang: f(5)?,
                react_delay: f(6)?,
                react_time: f(7)?,
                lock: [[f(8)?, f(9)?], [f(10)?, f(11)?], [f(12)?, f(13)?]],
            });
        }
        controls.push(Control { name: text(b.u32(at)?)?, objects });
    }
    Ok(controls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_control() {
        let mut d = vec![0u8; 0x10];
        d[4..8].copy_from_slice(&1u32.to_be_bytes());
        d[8..12].copy_from_slice(&0x10u32.to_be_bytes());
        let strings = 0x10 + 12 + OBJECT;
        d[12..16].copy_from_slice(&(strings as u32).to_be_bytes());
        for v in [0u32, 1, 0] {
            d.extend(v.to_be_bytes());
        }
        d.extend(6u32.to_be_bytes());
        d.extend(14u32.to_be_bytes());
        for k in 0..14 {
            d.extend((k as f32).to_be_bytes());
        }
        d.extend(b"gun_R\0r_arm01\0\0");
        let c = read(&d).unwrap();
        assert_eq!((c[0].name.as_str(), c[0].objects[0].bone.as_str()), ("gun_R", "r_arm01"));
        let o = &c[0].objects[0];
        assert_eq!((o.axis_bone.as_str(), o.react_ang, o.react_delay, o.react_time), ("", 4.0, 6.0, 7.0));
        assert_eq!(o.lock[2], [12.0, 13.0]);
    }

    #[test]
    fn gun_kick_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else { return };
        let c = read(&crate::vfs::open(&disc, "param/jcondata.bin").unwrap()).unwrap();
        let names: Vec<_> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["gun_L", "gun_R", "OW", "Core", "sniper_L", "sniper_R", "weaponArms"]);
        let kick: Vec<_> = c[1].objects.iter().map(|o| (o.bone.as_str(), o.react_ang, o.react_delay, o.react_time)).collect();
        assert_eq!(
            kick,
            [
                ("r_arm01", -10.0, 5.0, 6.0),
                ("r_shoul", 0.0, 0.0, 0.0),
                ("r_arm02", -2.0, 5.0, 6.0),
                ("r_arm03", 0.0, 0.0, 0.0),
                ("r_arm04", 0.0, 0.0, 0.0),
                ("r_arm05", -5.0, 0.0, 6.0),
                ("r_armhand", 0.0, 5.0, 0.0),
            ]
        );
        assert_eq!((c[5].objects[0].bone.as_str(), c[5].objects[0].react_ang), ("r_arm01", -2.0));
    }
}
