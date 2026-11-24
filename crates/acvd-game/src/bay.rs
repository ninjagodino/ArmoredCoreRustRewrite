//! Bay shift & purge (`sheets/ac_states.csv`, `sheets/hanger_anim.csv`, `sheets/sound_cues.csv`).
//!
//! Hold R (△ on a pad, manual id 20403) and press a hand's fire button: that hand's weapon trades
//! places with the one on its rack. A `ready_position` weapon (`arms.hands[i].stance`) cannot sit
//! on the bay (manual 20813), so the shift purges it (`bay_purge_*`, acmotion 282/283, anims
//! 302/303) and brings the bay weapon to the hand.
//!
//! 360 flow (`0x828777c8` -> `0x82876f48` -> `0x82849440`/`0x82849450` -> `0x828487b8`):
//! - Each hand tracks its own rack carousel phase (`0` or `1`, toggled on completion) and plays
//!   `bay_shift_r`/`bay_shift_l` (acmotion 281/280, anims 301/300, 150f) on the per-arm overlay
//!   layer (`r_kata` / `l_kata`, 360 layer `0x0a`/`0x0b`) so the other arm stays free.
//! - Passes explicit `own000100.tae` IDs `2000..2003` (shift) and `2010..2013` (purge):
//!   - Frame `0`: sound `c00000047` (`/se_ac/change_weapon02`), FFX `4109` (scale `1.5`), and
//!     rack carousel clip `a00_011` (`0->1`) or `a00_021` (`1->0`).
//!   - Frame `15`: FFX `98` (steam/vent puff, scale `1.5`).
//!   - Frame `40`: `kind=101` (`Reattach`) seats the hand weapon onto the rack's **empty** prong
//!     (`*_hg_b` in phase 0, `*_hg_f` in phase 1) so the two weapons never overlap.
//!   - Frame `150`: `kind=101` (`Reattach`) grabs the spun stowed weapon onto the hand and
//!     flips the rack carousel phase (`0 <-> 1`).
//!   - Purge (`2010..2013`) frame `10`: sound `c00000045` (`/se_ac/purge00`) and `kind=102`
//!     (`Purge`).

use acvd_data::generated::ac_unit::PARAM_ACMOTION_BIN;
use acvd_data::{ac_state, find};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::Garage;
use crate::control::{Held, Pilot, Piloting};
use crate::hanger::HangerPose;
use crate::pose::{Motion, TaeFired};
use crate::sfx::EffectPoint;
use crate::weapons::{Armament, Hand, HandShift, Hardpoint, Kind, Shift, WeaponAnims};
use acvd_render::app::ShotMark;

/// Fallback seat frame for bay shift (`own000100.tae` `2000..2003` `kind=101` at frame 40).
const SEAT_FRAME: f32 = 40.0;
/// Fallback grab frame for bay shift (`own000100.tae` `2000..2003` `kind=101` at frame 150).
const GRAB_FRAME: f32 = 150.0;
/// Fallback purge frame (`own000100.tae` `2010..2013` `kind=102` at frame 10).
const PURGE_FRAME: f32 = 10.0;
/// Rack carousel clip length (`a00_011.ani` / `a00_021.ani` = 180 frames at 60 fps).
const RACK_FRAMES: f32 = 180.0;
const PURGED: &str = "purged";
const COLUMN_SWAP: &str = "bay-swap";

/// Where one weapon root is mounted, and the socket it currently occupies.
/// Bay sockets also carry `alt_parent` (`r_hg_b` / `l_hg_b`), the carousel's opposite prong, so
/// the newly seated weapon sits on the empty prong while the stowed weapon rides the other prong.
pub struct Mounted {
    pub weapon: Option<Entity>,
    pub parent: Entity,
    pub alt_parent: Option<Entity>,
    pub local: Transform,
    /// `acvparts` category-10 `+0x0a` (`hanger_pose`, `0..=4`) for the weapon in this slot.
    pub hanger_pose: u8,
}

/// Hand right, hand left, bay right, bay left.
#[derive(Resource, Default)]
pub struct Loadout {
    pub slots: [Option<Mounted>; 4],
}

impl Loadout {
    pub fn set(&mut self, hand: Hand, bay: bool, mounted: Mounted) {
        self.slots[index(hand, bay)] = Some(mounted);
    }
}

fn index(hand: Hand, bay: bool) -> usize {
    hand as usize + if bay { 2 } else { 0 }
}

/// Explicit `own000100.tae` animation ID passed by 360 `0x82849440`/`0x82849450` (shift) and
/// `0x82848548` (purge).
pub fn tae_id_for(hand: Hand, phase: u8, purge: bool) -> u32 {
    if purge {
        match (hand, phase & 1) {
            (Hand::Right, 0) => 2010,
            (Hand::Left, 0) => 2011,
            (Hand::Right, _) => 2012,
            (Hand::Left, _) => 2013,
        }
    } else {
        match (hand, phase & 1) {
            (Hand::Left, 0) => 2000,
            (Hand::Right, 0) => 2001,
            (Hand::Left, _) => 2002,
            (Hand::Right, _) => 2003,
        }
    }
}

/// TAE `kind=101` (`Reattach`) IDs that mean "seat hand weapon onto empty rack prong" (frame 40)
/// for `(hand, phase)` (`own000100.tae` anims `2000..2003`).
fn is_seat_reattach(hand: Hand, phase: u8, id: u32) -> bool {
    match (hand, phase & 1) {
        (Hand::Left, 0) => id == 13,
        (Hand::Right, 0) => id == 14,
        (Hand::Left, _) => id == 9,
        (Hand::Right, _) => id == 10,
    }
}

/// TAE `kind=101` (`Reattach`) IDs that mean "grab spun weapon from rack onto hand" (frame 150)
/// for `(hand, phase)` (`own000100.tae` anims `2000..2003`).
fn is_grab_reattach(hand: Hand, phase: u8, id: u32) -> bool {
    match (hand, phase & 1) {
        (Hand::Left, 0) => id == 11,
        (Hand::Right, 0) => id == 12,
        (Hand::Left, _) => id == 2,
        (Hand::Right, _) => id == 3,
    }
}

/// TAE `kind=102` (`Purge`) IDs for `(hand, phase)` (`own000100.tae` anims `2010..2013`).
fn is_purge_event(hand: Hand, phase: u8, id: u32) -> bool {
    match (hand, phase & 1) {
        (Hand::Right, 0) => id == 2,
        (Hand::Left, 0) => id == 3,
        (Hand::Right, _) => id == 11,
        (Hand::Left, _) => id == 12,
    }
}

/// A skinned mesh of one weapon, so a purge can hide that weapon and not the one that replaced it.
#[derive(Component)]
pub struct WeaponMesh {
    pub root: Entity,
}

#[derive(SystemParam)]
pub(crate) struct Press<'w, 's> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    mouse: Res<'w, ButtonInput<MouseButton>>,
    held: Res<'w, Held>,
    pads: Query<'w, 's, &'static Gamepad>,
}

impl Press<'_, '_> {
    fn modifier(&self) -> bool {
        self.keys.pressed(KeyCode::KeyR)
            || self.held.0.contains(&KeyCode::KeyR)
            || self
                .pads
                .iter()
                .any(|pad| pad.pressed(GamepadButton::North))
    }

    fn down(&self, hand: Hand) -> bool {
        let (key, click, trigger) = match hand {
            Hand::Right => (
                KeyCode::KeyF,
                MouseButton::Right,
                GamepadButton::RightTrigger2,
            ),
            Hand::Left => (
                KeyCode::KeyC,
                MouseButton::Left,
                GamepadButton::LeftTrigger2,
            ),
        };
        self.keys.pressed(key)
            || self.held.0.contains(&key)
            || self.mouse.pressed(click)
            || self.pads.iter().any(|pad| pad.pressed(trigger))
    }
}

#[derive(Default)]
pub(crate) struct Edges {
    right: bool,
    left: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn shift(
    press: Press,
    piloting: Res<Piloting>,
    garage: Res<Garage>,
    mut commands: Commands,
    active: Option<ResMut<Shift>>,
    mut loadout: Option<ResMut<Loadout>>,
    mut acs: Query<(&mut Pilot, &mut Armament)>,
    mut anims: Option<ResMut<WeaponAnims>>,
    mut hanger: Option<ResMut<HangerPose>>,
    mut points: Query<&mut EffectPoint>,
    hardpoints: Query<(Entity, &Hardpoint)>,
    meshes: Query<(Entity, &WeaponMesh)>,
    mut motion: Option<ResMut<Motion>>,
    time: Res<Time>,
    mut edges: Local<Edges>,
) {
    if !piloting.0 {
        return;
    }
    let Some(loadout) = loadout.as_mut() else {
        return;
    };
    let Ok((pilot, mut arms)) = acs.single_mut() else {
        return;
    };
    if arms.weapon_arm {
        return;
    }
    let Some(mut shift) = active else {
        commands.insert_resource(Shift::default());
        return;
    };

    // 1. Advance any active per-hand shifts and handle TAE Reattach / Purge events.
    let dt_frames = time.delta_secs().min(0.1) * 60.0;
    let tae_events: Vec<TaeFired> = motion
        .as_ref()
        .map(|m| m.tae_events.clone())
        .unwrap_or_default();

    for side in 0..2 {
        let Some(mut hs) = shift.hands[side] else {
            continue;
        };
        hs.rack_frame += dt_frames;
        let arm_frame = motion
            .as_ref()
            .and_then(|m| m.arm_layers[side].as_ref().map(|l| l.frame))
            .unwrap_or(if hs.purge { hs.swap_at } else { hs.grab_at });

        let mut seat_hit = !hs.swapped && arm_frame >= hs.swap_at;
        let mut grab_hit = hs.swapped && !hs.grabbed && !hs.purge && arm_frame >= hs.grab_at;
        for ev in &tae_events {
            match *ev {
                TaeFired::Reattach { id } if !hs.purge => {
                    if !hs.swapped && is_seat_reattach(hs.hand, hs.phase, id) {
                        seat_hit = true;
                    } else if !hs.grabbed && is_grab_reattach(hs.hand, hs.phase, id) {
                        grab_hit = true;
                    }
                }
                TaeFired::Purge { id } if hs.purge => {
                    if !hs.swapped && is_purge_event(hs.hand, hs.phase, id) {
                        seat_hit = true;
                    }
                }
                _ => {}
            }
        }

        if seat_hit {
            hs.swapped = true;
            if hs.purge {
                exchange(
                    &mut commands,
                    loadout,
                    &mut arms,
                    anims.as_deref_mut(),
                    &mut points,
                    &hardpoints,
                    &meshes,
                    hs.hand,
                    true,
                );
                hs.grabbed = true;
            } else {
                seat(
                    commands.reborrow(),
                    loadout,
                    hanger.as_deref_mut(),
                    &hs,
                );
            }
        }
        if grab_hit {
            hs.grabbed = true;
            grab(
                commands.reborrow(),
                loadout,
                &mut arms,
                anims.as_deref_mut(),
                &mut points,
                &hardpoints,
                &hs,
            );
        }

        let arm_done = motion.as_ref().is_none_or(|m| m.arm_finished(side));
        let rack_done = hs.purge || hs.rack_frame >= RACK_FRAMES;
        if hs.grabbed && arm_done {
            if let Some(m) = motion.as_mut() {
                if m.arm_layers[side].is_some() {
                    m.clear_arm(side, 0.15);
                }
            }
        }
        if hs.grabbed && arm_done && rack_done {
            if !hs.purge {
                shift.phase[side] ^= 1;
            }
            shift.hands[side] = None;
        } else {
            shift.hands[side] = Some(hs);
        }
    }

    // 2. Check for new bay shift / purge input on any idle hand.
    if !press.modifier() {
        edges.right = false;
        edges.left = false;
        return;
    }
    let right = press.down(Hand::Right);
    let left = press.down(Hand::Left);
    let rising = [
        (Hand::Right, right && !edges.right),
        (Hand::Left, left && !edges.left),
    ];
    edges.right = right;
    edges.left = left;

    for (hand, edge) in rising {
        if !edge {
            continue;
        }
        let i = hand as usize;
        if shift.hands[i].is_some() {
            continue;
        }
        if arms.bays[i].kind == Kind::Skip && arms.hands[i].kind == Kind::Skip {
            continue;
        }
        // Nothing on the rack to bring forward.
        if loadout.slots[index(hand, true)]
            .as_ref()
            .is_none_or(|m| m.weapon.is_none())
        {
            continue;
        }
        let purge = arms.hands[i].stance;
        let (state, swap_at) = match (hand, purge) {
            (Hand::Right, false) => ("bay_shift_r", SEAT_FRAME),
            (Hand::Left, false) => ("bay_shift_l", SEAT_FRAME),
            (Hand::Right, true) => ("bay_purge_r", PURGE_FRAME),
            (Hand::Left, true) => ("bay_purge_l", PURGE_FRAME),
        };
        let phase = shift.phase[i];
        let tae_id = tae_id_for(hand, phase, purge);
        let racked = loadout.slots[index(hand, true)]
            .as_ref()
            .and_then(|m| m.weapon);
        let incoming_pose = loadout.slots[index(hand, false)]
            .as_ref()
            .map(|m| m.hanger_pose)
            .unwrap_or(0);
        let mut grab_at = if purge { swap_at } else { GRAB_FRAME };
        let played = motion.as_mut().is_some_and(|motion| {
            let Some(row) = ac_state(state, None).and_then(|s| find(PARAM_ACMOTION_BIN, s.row))
            else {
                return false;
            };
            let ok = motion
                .play_arm(
                    &garage.disc,
                    i,
                    row.data.anim_id,
                    tae_id,
                    1.0,
                    pilot.fade(row.data.interpolate_id),
                )
                .is_ok();
            if ok && !purge {
                if let Some(ref arm) = motion.arm_layers[i] {
                    grab_at = (arm.clip.frames as f32).min(GRAB_FRAME).max(swap_at);
                }
            }
            ok
        });
        if !played {
            exchange(
                &mut commands,
                loadout,
                &mut arms,
                anims.as_deref_mut(),
                &mut points,
                &hardpoints,
                &meshes,
                hand,
                purge,
            );
            continue;
        }
        shift.hands[i] = Some(HandShift {
            hand,
            purge,
            phase,
            swap_at,
            grab_at,
            swapped: false,
            grabbed: false,
            rack_frame: 0.0,
            racked,
            incoming_pose,
        });
    }
}

/// Frame 40 of bay shift: parent the equipped hand weapon onto the rack's **empty** prong that
/// just swung forward to meet the hand (`parent` = `*_hg_f` in phase 0 / `a00_011`, `alt_parent` =
/// `*_hg_b` in phase 1 / `a00_021`) so the two weapons ride opposite prongs through the 180° spin.
fn seat(
    mut commands: Commands,
    loadout: &mut Loadout,
    hanger: Option<&mut HangerPose>,
    hs: &HandShift,
) {
    let hand = hs.hand;
    let hand_weapon = loadout.slots[index(hand, false)]
        .as_ref()
        .and_then(|m| m.weapon);
    let (empty_prong, bay_local) = {
        let Some(mount) = loadout.slots[index(hand, true)].as_ref() else {
            return;
        };
        let prong = if hs.phase & 1 == 0 {
            mount.parent
        } else {
            mount.alt_parent.unwrap_or(mount.parent)
        };
        (prong, mount.local)
    };
    if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
        slot.weapon = None;
    }
    if let Some(weapon) = hand_weapon {
        commands
            .entity(weapon)
            .insert((ChildOf(empty_prong), bay_local));
        commands.entity(weapon).remove::<Hardpoint>();
        if let Some(slot) = loadout.slots[index(hand, true)].as_mut() {
            slot.weapon = Some(weapon);
        }
    }
    if let Some(hanger) = hanger {
        hanger.set_pose(hand, hs.incoming_pose);
    }
}

/// Frame 150 of bay shift: take the weapon that rode the rack spin onto the hand.
fn grab(
    mut commands: Commands,
    loadout: &mut Loadout,
    arms: &mut Armament,
    anims: Option<&mut WeaponAnims>,
    points: &mut Query<&mut EffectPoint>,
    hardpoints: &Query<(Entity, &Hardpoint)>,
    hs: &HandShift,
) {
    let hand = hs.hand;
    let i = hand as usize;
    let (hand_col, bay_col) = match hand {
        Hand::Right => ("armwep_r", "hanger_r"),
        Hand::Left => ("armwep_l", "hanger_l"),
    };
    let hand_socket = {
        let Some(mount) = loadout.slots[index(hand, false)].as_ref() else {
            return;
        };
        (mount.parent, mount.local)
    };
    if let Some(weapon) = hs.racked {
        commands
            .entity(weapon)
            .insert((ChildOf(hand_socket.0), hand_socket.1));
        if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
            slot.weapon = Some(weapon);
        }
    }
    // Swap stored hanger_pose metadata between hand and bay slots.
    let bay_pose = loadout.slots[index(hand, true)]
        .as_ref()
        .map(|m| m.hanger_pose)
        .unwrap_or(0);
    if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
        slot.hanger_pose = bay_pose;
    }
    if let Some(slot) = loadout.slots[index(hand, true)].as_mut() {
        slot.hanger_pose = hs.incoming_pose;
    }

    let bay = arms.bays[i];
    arms.bays[i] = arms.hands[i];
    arms.hands[i] = bay;
    if let Some(anims) = anims {
        anims.transfer(hand, false);
    }
    relabel(points, hand_col, bay_col, false);
    for (entity, point) in hardpoints.iter() {
        if point.hand == hand {
            commands.entity(entity).remove::<Hardpoint>();
        }
    }
    if let Some(weapon) = hs.racked {
        commands.entity(weapon).insert(Hardpoint { hand });
    }
}

#[allow(clippy::too_many_arguments)]
fn exchange(
    commands: &mut Commands,
    loadout: &mut Loadout,
    arms: &mut Armament,
    anims: Option<&mut WeaponAnims>,
    points: &mut Query<&mut EffectPoint>,
    hardpoints: &Query<(Entity, &Hardpoint)>,
    meshes: &Query<(Entity, &WeaponMesh)>,
    hand: Hand,
    purge: bool,
) {
    let i = hand as usize;
    let (hand_col, bay_col) = match hand {
        Hand::Right => ("armwep_r", "hanger_r"),
        Hand::Left => ("armwep_l", "hanger_l"),
    };
    let (hand_weapon, hand_socket, hand_pose) = {
        let Some(mount) = loadout.slots[index(hand, false)].as_ref() else {
            return;
        };
        (mount.weapon, (mount.parent, mount.local), mount.hanger_pose)
    };
    let (bay_weapon, bay_socket, bay_pose) = {
        let Some(mount) = loadout.slots[index(hand, true)].as_ref() else {
            return;
        };
        (mount.weapon, (mount.parent, mount.local), mount.hanger_pose)
    };
    if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
        slot.weapon = None;
    }
    if let Some(slot) = loadout.slots[index(hand, true)].as_mut() {
        slot.weapon = None;
    }

    if let Some(weapon) = hand_weapon {
        if purge {
            for (mesh, tag) in meshes.iter() {
                if tag.root == weapon {
                    commands.entity(mesh).insert(Visibility::Hidden);
                }
            }
        } else {
            commands
                .entity(weapon)
                .insert((ChildOf(bay_socket.0), bay_socket.1));
            if let Some(slot) = loadout.slots[index(hand, true)].as_mut() {
                slot.weapon = Some(weapon);
                slot.hanger_pose = hand_pose;
            }
        }
    }
    if let Some(weapon) = bay_weapon {
        commands
            .entity(weapon)
            .insert((ChildOf(hand_socket.0), hand_socket.1));
        if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
            slot.weapon = Some(weapon);
            slot.hanger_pose = bay_pose;
        }
    }

    if purge {
        arms.hands[i] = arms.bays[i];
        arms.bays[i] = crate::weapons::silent();
    } else {
        let bay = arms.bays[i];
        arms.bays[i] = arms.hands[i];
        arms.hands[i] = bay;
    }

    if let Some(anims) = anims {
        anims.transfer(hand, purge);
    }
    relabel(points, hand_col, bay_col, purge);
    let now = loadout.slots[index(hand, false)]
        .as_ref()
        .and_then(|m| m.weapon);
    for (entity, point) in hardpoints.iter() {
        if point.hand == hand {
            commands.entity(entity).remove::<Hardpoint>();
        }
    }
    if let Some(weapon) = now {
        commands.entity(weapon).insert(Hardpoint { hand });
    }
}

fn relabel(
    points: &mut Query<&mut EffectPoint>,
    hand_col: &'static str,
    bay_col: &'static str,
    purge: bool,
) {
    for mut point in points.iter_mut() {
        if point.column == hand_col {
            point.column = if purge { PURGED } else { COLUMN_SWAP };
        }
    }
    for mut point in points.iter_mut() {
        if point.column == bay_col {
            point.column = hand_col;
        } else if point.column == COLUMN_SWAP {
            point.column = bay_col;
        }
    }
}

/// Logs bay-shift and per-arm overlay state on the frame [`ShotMark`] names.
pub(crate) fn probe_shot(
    mark: Option<Res<ShotMark>>,
    shift: Option<Res<Shift>>,
    motion: Option<Res<Motion>>,
    loadout: Option<Res<Loadout>>,
    acs: Query<&Armament, With<Pilot>>,
) {
    let Some(path) = mark.as_ref().and_then(|m| m.path.as_ref()) else {
        return;
    };
    let arm_info = motion
        .as_ref()
        .map(|m| {
            let r = m.arm_layers[0]
                .as_ref()
                .map(|a| format!("anim={} tae={} f={:.1}", a.anim_id, a.tae_id, a.frame))
                .unwrap_or_else(|| "none".into());
            let l = m.arm_layers[1]
                .as_ref()
                .map(|a| format!("anim={} tae={} f={:.1}", a.anim_id, a.tae_id, a.frame))
                .unwrap_or_else(|| "none".into());
            format!("arm_r=({r}) arm_l=({l}) tae_loaded={}", m.tae.is_some())
        })
        .unwrap_or_else(|| "no_motion".into());
    let shift_info = shift
        .as_ref()
        .map(|s| {
            let hs = |i: usize| {
                s.hands[i]
                    .map(|h| {
                        format!(
                            "p={} sw={} gr={} rf={:.1} purge={}",
                            h.phase, h.swapped as u8, h.grabbed as u8, h.rack_frame, h.purge as u8
                        )
                    })
                    .unwrap_or_else(|| "idle".into())
            };
            format!(
                "phase=[{},{}] r=({}) l=({})",
                s.phase[0],
                s.phase[1],
                hs(0),
                hs(1)
            )
        })
        .unwrap_or_else(|| "no_shift".into());
    let slots = loadout
        .as_ref()
        .map(|l| {
            format!(
                "slots=[{},{},{},{}]",
                l.slots[0].as_ref().and_then(|m| m.weapon).is_some() as u8,
                l.slots[1].as_ref().and_then(|m| m.weapon).is_some() as u8,
                l.slots[2].as_ref().and_then(|m| m.weapon).is_some() as u8,
                l.slots[3].as_ref().and_then(|m| m.weapon).is_some() as u8,
            )
        })
        .unwrap_or_else(|| "no_loadout".into());
    let wep_arm = acs.single().map(|a| a.weapon_arm as u8).unwrap_or(9);
    eprintln!(
        "SHOT {} bay {shift_info} {arm_info} {slots} wep_arm={wep_arm}",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tae_ids_match_360_dispatch() {
        // 360 0x82849440 (phase 0->1): L=2000, R=2001
        assert_eq!(tae_id_for(Hand::Left, 0, false), 2000);
        assert_eq!(tae_id_for(Hand::Right, 0, false), 2001);
        // 360 0x82849450 (phase 1->0): L=2002, R=2003
        assert_eq!(tae_id_for(Hand::Left, 1, false), 2002);
        assert_eq!(tae_id_for(Hand::Right, 1, false), 2003);
        // 360 0x82848548 (purge): R0=2010, L0=2011, R1=2012, L1=2013
        assert_eq!(tae_id_for(Hand::Right, 0, true), 2010);
        assert_eq!(tae_id_for(Hand::Left, 0, true), 2011);
        assert_eq!(tae_id_for(Hand::Right, 1, true), 2012);
        assert_eq!(tae_id_for(Hand::Left, 1, true), 2013);
    }

    #[test]
    fn reattach_and_purge_ids_match_own000100_tae() {
        assert!(is_seat_reattach(Hand::Left, 0, 13));
        assert!(is_grab_reattach(Hand::Left, 0, 11));
        assert!(is_seat_reattach(Hand::Right, 0, 14));
        assert!(is_grab_reattach(Hand::Right, 0, 12));
        assert!(is_seat_reattach(Hand::Left, 1, 9));
        assert!(is_grab_reattach(Hand::Left, 1, 2));
        assert!(is_seat_reattach(Hand::Right, 1, 10));
        assert!(is_grab_reattach(Hand::Right, 1, 3));
        assert!(is_purge_event(Hand::Right, 0, 2));
        assert!(is_purge_event(Hand::Left, 0, 3));
        assert!(is_purge_event(Hand::Right, 1, 11));
        assert!(is_purge_event(Hand::Left, 1, 12));
    }
}
