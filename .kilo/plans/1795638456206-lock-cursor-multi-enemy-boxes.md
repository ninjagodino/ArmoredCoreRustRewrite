# Lock cursor: one quadrant box per enemy

## Goal

Finish the in-game lock-on cursor so it behaves like the scan/recon boxes: a four-quadrant
`sortie.drb` box on **every** enemy the FCS can acquire, with the selected target's box carrying
the enemy name/AP, per-weapon `LOCK ON`, `ReloadBar` and missile pips.

## Decisions (from the user)

- **One box per enemy**, not a single selected-target box.
- **Which enemies**: exactly the FCS `valid_targets` set (inner lock cone + LOS, plus the current
  target while it lingers in the +2° outer hysteresis cone).
- **Pool**: 8 box roots, assigned in FCS aim-score order; extra candidates are not drawn.
- **Info on selected only**: the FCS-locked target's box shows `EnemyName`/`EnemyAP`, per-weapon
  `LockOnText`/`ReloadBar` and shoulder `MissileLock1..6`; every other box shows only `CursorLine`.

## Facts already established

- Scaling/anchoring reference: `scan_hud` places `ACV_Scan_Cur_Center` at
  `vp - (20,20)*hud.scale` (crates/acvd-game/src/hud.rs:1528-1535) and `recon_hud` places the
  per-enemy `ACV_MapScan_SubTargetS` at `vp - (160,160)*hud.scale` (hud.rs:1932-1940).
  `menu::spawn` takes an origin in logical px and multiplies object rects by `scale`
  (crates/acvd-render/src/menu.rs:487-547), so `vp - anchor*scale` is the correct pattern.
- The four quadrant dialogs tile a 256×128 box: `CursorLine` tips are R (0,0), L (128,0),
  T (0,64), E (128,64); box center = target viewport point.
- 360: `0x829d4b88` builds one cursor group from the four `ACV_OutLine_LockCursor_{E,R,L,T}`
  dialogs plus the `LockColor1st/2nd` rects; `0x829d4e40` creates two such groups
  (`+0x88`/`+0x8c`) and two `ACV_OutLine_LockCursor_Target` 32×32 markers; `0x829d4558` draws one
  quadrant per candidate (the groups are reusable draw commands, so multiple boxes per frame are
  consistent with retail).
- `hud::lock_cursor` today draws one box from `fcs.locked` only
  (crates/acvd-game/src/hud.rs:1951-2149); `fcs::update` already computes the candidate set but
  keeps only `best`/`valid_targets` (crates/acvd-game/src/fcs.rs:400-479).

## Steps

1. **Expose FCS candidates** (`crates/acvd-game/src/fcs.rs`)
   - Add `pub struct LockCandidate { pub entity: Entity, pub world_pos: Vec3, pub score: f32 }`
     and `pub candidates: Vec<LockCandidate>` on `Fcs` (init empty in `from_ctrl`).
   - In `update`, push an entry for every accepted target (inner cone and the current-target outer
     branch) with the same `score` used for `best`; after the loop sort by `score` ascending
     (aim-center first) and store in `fcs.candidates`. Leave `locked`, `valid_targets` and missile
     lock logic unchanged.
   - Clear `candidates` in the existing scan-mode / OW-lockout early return (fcs.rs:383-389).
   - Test: two in-cone enemies are both listed in score order; a LOS-blocked enemy is not
     (extend `fcs::tests`, mirroring `lock_check_ray_blocks_fcs_lock_behind_wall`).

2. **Shared placement helper** (`crates/acvd-game/src/hud.rs`)
   - Add `fn place_centered(node: &mut Node, vp: Vec2, anchor: Vec2, scale: f32)` that sets
     `left/top = vp - anchor*scale`, and use it for the scan bracket (anchor `(20,20)`), recon
     `SubTargetS` (`(160,160)`), lock box group (`(128,64)`) and missile marker (`(16,16)`).
     This is the "same scaling as the scan box" requirement; no behaviour change for scan/recon.
   - Add a unit test pinning the helper at scale 1.0 and 2.0.

3. **Spawn 8 box groups** (`hud.rs`, where the wings are spawned, ~716-745)
   - `const MAX_LOCK_BOXES: usize = 8;`
   - Per slot: one group root `(MenuRoot, Node { position_type: Absolute, .. }, LockBox(slot),
     Visibility::Hidden)`; spawn the four dialogs with `menu::spawn(..., Vec2::ZERO, scale)`, then
     `.remove::<MenuRoot>().insert((ChildOf(group), LockBoxQuadrant { slot, wing }))` (the same
     pattern as `garage_assemble.rs:855`), so resize/sortie cleanup despawning `MenuRoot` groups
     removes the children once.
   - Keep the existing 6 `MissileLockTarget` roots unchanged. Replace `LockCursorWing` as the
     root tag with `LockBox` + `LockBoxQuadrant { slot, wing }` (keep the enum for the wing kind).

4. **Rewrite `lock_cursor`** (`hud.rs:1951`)
   - Project `fcs.candidates` in order; keep candidates whose `world_to_viewport` succeeds, cap at
     8. If `fcs.locked` exists but is missing from candidates (should not happen), append it.
   - Per slot: assigned → `Visibility::Inherited` and `place_centered(node, vp, (128,64), scale)`;
     unassigned → hidden.
   - Selected slot (target entity == `fcs.locked.entity`): keep the current per-quadrant logic —
     `CursorLine` gauge fill = per-hand secondary progress, colour = `lock_color(ready)`;
     `LockOnText` per weapon readiness; `ReloadBar` while the hand/shoulder cools down;
     `MissileLock1..6` from `fcs.missile_locks.len()`; `EnemyName` = `lock.model` and `EnemyAP`
     on `_E`; `EnemySquadNumber` hidden.
   - Every other assigned slot: `CursorLine` fill 1.0 with the **authored** colour (do not set
     `g.color`), and hide `LockOnText`, `ReloadBar`, `MissileLock*`, `EnemyName`, `EnemyAP`,
     `EnemySquadNumber` — reset every frame so text/pips do not linger when a slot changes target.
   - Add SHOT telemetry (same pattern as `track_lock_sight`'s `Local<bool>`):
     `SHOT lock_boxes=<n> selected=<model> candidates=[...]` so `--shot` runs are inspectable.

5. **Markers** (keep, verify separately)
   - Leave the 6 `ACV_OutLine_LockCursor_Target` markers as per missile-locked target, teal
     `LockColor1st` for one missile on a target and red `LockColor2nd` once a second is on it.
   - Follow-up (not blocking): disassemble `0x829d4b88` to confirm the group `+0xc` pointers are
     the four quadrants and what the `+8` config byte on the second group is; only change marker
     semantics if that contradicts the shots.

6. **Verification**
   - `cargo test -p acvd-game` (all pass), `cargo clippy -p acvd-game` (no new warnings outside
     pre-existing hud ones), `cargo fmt`.
   - Rewrite shots: `cargo run -p acvd-game -- --map m4000 --test actest2 --shot
     private\shots\lock_multi.png --wait 6` (and `--test all`) — expect ≥2 boxes with only the
     selected one carrying name/AP and `LOCK ON`; check the `SHOT lock_boxes=` line.
   - 360 reference: fix `tools/xenia/recipes/ac_test_lockhunt.txt` to look level/up and pan a full
     360° with longer waits, then run it; AC TEST ships test units, so at least one frame should
     show a locked box. Compare with `tools\shot-check.ps1` (or `acvd-shot diff`).
   - If no 360 lock frame can be produced, record that in `docs/open.md` and accept the rewrite
     shot as the visual check, stating it explicitly.

7. **Docs and commit**
   - Update the lock-on bullets in `docs/archive.md` (per-enemy boxes, pool 8, shared placement
     helper, 360 refs) and `docs/open.md` (retail capture + marker-colour RE still open).
   - No sheet changes, so no `acvd-sheets all`; `acvd-index audit` only if sheets change.
   - Stage only touched files (`hud.rs`, `fcs.rs`, `docs/archive.md`, `docs/open.md`,
     `tools/xenia/recipes/ac_test_lockhunt.txt`) — the worktree has unrelated edits from other
     sessions; commit per repo workflow.

## Risks / pitfalls

- Child positioning: `menu::spawn` roots are `PositionType::Absolute`; as `ChildOf(group)` their
  `left/top` become relative to the group. Verify with one `--shot` before tuning styling.
- Nested `MenuRoot` double-despawn on mode switch (`main.rs:1291-1319`): children must not keep
  `MenuRoot`.
- Resize respawn (`hud.rs:436-460`) despawns `MenuRoot` entities: group roots must keep it.
- Off-screen/behind-camera projections: rely on the FCS cone; optionally skip boxes whose viewport
  point is far outside the screen to avoid stray UI.
- Slot reuse: always reset per-slot element visibility/text, not only when the selected target
  changes.

## Out of scope

- `EnemySquadNumber` content (no squad table in the rewrite; stays hidden).
- `LockOnText` text (static `Message` bank 1 id 2313; visibility only).
- Animated `ReloadBar` fill (visibility only).
- A nicer enemy display name: keep `lock.model`, matching scan's `EnemyText`.
