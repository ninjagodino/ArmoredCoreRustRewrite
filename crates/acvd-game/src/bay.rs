//! Bay shift. Hold R (△ on a pad, manual id 20403) and press a hand's fire button: that hand's
//! weapon trades places with the one on its rack. The body plays `bay_shift_*`
//! (`sheets/ac_states.csv`, acmotion 280/281, anims 300/301). A `ready_position` weapon cannot
//! sit on the bay (manual 20813), so the shift purges it (`bay_purge_*`, acmotion 282/283)
//! and brings the bay weapon to the hand.
//!
//! The equipped weapon is seated on the rack at frame 40, the first time `r_armhand` / `l_armhand`
//! is up. The weapon that was already on the rack stays there and rides the prong spin. The hand
//! takes it at the clip's second reach, about 10 frames before the end (frame 140 of 150). The
//! purge clips are 55 frames and have only the first reach.

use acvd_data::generated::ac_unit::PARAM_ACMOTION_BIN;
use acvd_data::{ac_state, find};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::control::{Held, Pilot, Piloting};
use crate::pose::Motion;
use crate::sfx::EffectPoint;
use crate::weapons::{Armament, Hand, Hardpoint, Kind, Shift, WeaponAnims};
use crate::Garage;

/// First reach of `l_armhand` / `r_armhand` in anim 300/301 (the hand is up from about frame 30).
const SEAT_FRAME: f32 = 40.0;
/// Frames before the end of anim 300/301 where the hand is up again and takes the spun weapon.
const GRAB_BEFORE_END: f32 = 10.0;
/// The purge clips (anims 302/303) key the hand every frame through 21, then ease out.
const PURGE_FRAME: f32 = 20.0;
/// `a00_011` frame where the prong has swung the stowed weapon forward, toward the hand.
const PRESENT_FRAME: f32 = 90.0;
/// Seconds the prong takes to fold back to the stow after the grab.
const FOLD_SECS: f32 = 0.45;
const PURGED: &str = "purged";
const COLUMN_SWAP: &str = "bay-swap";

/// Where one weapon root is mounted, and the socket it currently occupies.
pub struct Mounted {
    pub weapon: Option<Entity>,
    pub parent: Entity,
    pub local: Transform,
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
                MouseButton::Left,
                GamepadButton::RightTrigger2,
            ),
            Hand::Left => (
                KeyCode::KeyC,
                MouseButton::Right,
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

pub(crate) fn shift(
    press: Press,
    piloting: Res<Piloting>,
    garage: Res<Garage>,
    mut commands: Commands,
    mut active: Option<ResMut<Shift>>,
    mut loadout: Option<ResMut<Loadout>>,
    mut acs: Query<(&mut Pilot, &mut Armament)>,
    mut anims: Option<ResMut<WeaponAnims>>,
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
    let Ok((mut pilot, mut arms)) = acs.single_mut() else {
        return;
    };

    if let Some(shift) = active.as_mut() {
        let frame = motion.as_ref().map_or(shift.swap_at, |m| m.frame);
        if !shift.swapped && frame >= shift.swap_at {
            if shift.purge {
                exchange(
                    &mut commands,
                    loadout,
                    &mut arms,
                    anims.as_deref_mut(),
                    &mut points,
                    &hardpoints,
                    &meshes,
                    shift.hand,
                    true,
                );
                shift.grabbed = true;
            } else {
                seat(commands.reborrow(), loadout, shift);
            }
            shift.swapped = true;
        }
        if shift.swapped && !shift.grabbed && frame >= shift.grab_at {
            grab(
                commands.reborrow(),
                loadout,
                &mut arms,
                anims.as_deref_mut(),
                &mut points,
                &hardpoints,
                shift,
            );
            shift.grabbed = true;
        }
        if shift.grabbed {
            shift.present = (shift.present - time.delta_secs() / FOLD_SECS).max(0.0);
        } else if shift.swapped && !shift.purge {
            let span = (shift.grab_at - shift.swap_at).max(1.0);
            shift.present = ((frame - shift.swap_at) / span).clamp(0.0, 1.0);
        }
        let body_done = motion.as_ref().is_none_or(|m| m.finished());
        if shift.grabbed && shift.present <= 0.0 && body_done {
            pilot.body_held(false);
            commands.remove_resource::<Shift>();
        }
        return;
    }

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
    let Some(hand) = rising.into_iter().find_map(|(h, edge)| edge.then_some(h)) else {
        return;
    };
    let i = hand as usize;
    if arms.bays[i].kind == Kind::Skip && arms.hands[i].kind == Kind::Skip {
        return;
    }
    // Nothing on the rack to bring forward.
    if loadout.slots[index(hand, true)]
        .as_ref()
        .is_none_or(|m| m.weapon.is_none())
    {
        return;
    }
    let purge = arms.hands[i].stance;
    let (state, swap_at) = match (hand, purge) {
        (Hand::Right, false) => ("bay_shift_r", SEAT_FRAME),
        (Hand::Left, false) => ("bay_shift_l", SEAT_FRAME),
        (Hand::Right, true) => ("bay_purge_r", PURGE_FRAME),
        (Hand::Left, true) => ("bay_purge_l", PURGE_FRAME),
    };
    let racked = loadout.slots[index(hand, true)]
        .as_ref()
        .and_then(|m| m.weapon);
    let mut grab_at = swap_at;
    let played = motion.as_mut().is_some_and(|motion| {
        let Some(row) = ac_state(state, None).and_then(|s| find(PARAM_ACMOTION_BIN, s.row)) else {
            return false;
        };
        let ok = motion
            .play(
                &garage.disc,
                row.data.anim_id,
                row.data.b_loop != 0,
                1.0,
                pilot.fade(row.data.interpolate_id),
            )
            .is_ok();
        if ok {
            pilot.body_held(true);
            if !purge {
                grab_at = (motion.clip.frames as f32 - GRAB_BEFORE_END).max(swap_at);
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
        return;
    }
    commands.insert_resource(Shift {
        hand,
        purge,
        swap_at,
        grab_at,
        swapped: false,
        grabbed: false,
        present: 0.0,
        racked,
    });
}

/// Parent the equipped weapon onto the rack. The weapon that was already there stays put.
fn seat(mut commands: Commands, loadout: &mut Loadout, shift: &Shift) {
    let hand = shift.hand;
    let (hand_weapon, _hand_socket) = {
        let Some(mount) = loadout.slots[index(hand, false)].as_ref() else {
            return;
        };
        (mount.weapon, (mount.parent, mount.local))
    };
    let bay_socket = {
        let Some(mount) = loadout.slots[index(hand, true)].as_ref() else {
            return;
        };
        (mount.parent, mount.local)
    };
    if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
        slot.weapon = None;
    }
    if let Some(weapon) = hand_weapon {
        commands
            .entity(weapon)
            .insert((ChildOf(bay_socket.0), bay_socket.1));
        commands.entity(weapon).remove::<Hardpoint>();
        if let Some(slot) = loadout.slots[index(hand, true)].as_mut() {
            slot.weapon = Some(weapon);
        }
    }
}

/// Take the weapon that rode the rack spin onto the hand.
fn grab(
    mut commands: Commands,
    loadout: &mut Loadout,
    arms: &mut Armament,
    anims: Option<&mut WeaponAnims>,
    points: &mut Query<&mut EffectPoint>,
    hardpoints: &Query<(Entity, &Hardpoint)>,
    shift: &Shift,
) {
    let hand = shift.hand;
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
    if let Some(weapon) = shift.racked {
        commands
            .entity(weapon)
            .insert((ChildOf(hand_socket.0), hand_socket.1));
        if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
            slot.weapon = Some(weapon);
        }
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
    if let Some(weapon) = shift.racked {
        commands.entity(weapon).insert(Hardpoint { hand });
    }
}

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
    let (hand_weapon, hand_socket) = {
        let Some(mount) = loadout.slots[index(hand, false)].as_ref() else {
            return;
        };
        (mount.weapon, (mount.parent, mount.local))
    };
    let (bay_weapon, bay_socket) = {
        let Some(mount) = loadout.slots[index(hand, true)].as_ref() else {
            return;
        };
        (mount.weapon, (mount.parent, mount.local))
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
            }
        }
    }
    if let Some(weapon) = bay_weapon {
        commands
            .entity(weapon)
            .insert((ChildOf(hand_socket.0), hand_socket.1));
        if let Some(slot) = loadout.slots[index(hand, false)].as_mut() {
            slot.weapon = Some(weapon);
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
