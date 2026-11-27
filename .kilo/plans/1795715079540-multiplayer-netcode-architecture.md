# ACVD/ACV Multiplayer Netcode Architecture

Status: implementation-ready plan. No source changes have been made.

## 1. What the workspace inspection found

- Crate graph today: `acvd-formats` (disc readers, no Bevy) -> `acvd-data` (generated tables, no deps) -> `acvd-render` (Bevy assets) and `acvd-game` (Bevy app). There is no sim, net, or server crate.
- Simulation tick: movement is a **60 Hz accumulator** (`crates/acvd-game/src/control.rs:3230-3232`, `TICK_RATE = 60.0`), but it lives inside a Bevy `Update` system coupled to `Transform`, `Window`, `CursorOptions`, `ButtonInput`, `Garage`, `Collision`, and camera state.
- Projectiles/missiles are **not deterministic**: they use raw `time.delta_secs()` (`crates/acvd-game/src/weapons.rs:3288,3544`), and hit validation has no history rewind.
- Combat state is on Bevy entities: `TargetHealth`/`Target` (`targets.rs`), `Projectile`/`Missile` (`weapons.rs`), `DestructibleProp` (`map.rs`). Missiles and blades store Bevy `Entity` targets.
- Destructibles: `DestructibleProp` mixes sim data (`prop_id`, `hp`, `cutoff_damage`, `collision_damage_mult`, `damage_id`, `broken`) with render handles (`full`, `low`, `broken_mesh: Entity`); break handling is `map::step_destructibles`.
- Rendering boundary: `acvd-render` is the only Bevy asset layer; `acvd-game` owns Bevy ECS/camera/audio/UI.
- Bevy 0.19 uses **glam 0.32.1**, so a GPU-free sim crate can use the exact same `Vec3/Quat` types without depending on Bevy.
- `renet 2.0.1` + `renet_netcode 2.0.1` provide exactly the required channel model: `SendType::{Unreliable, ReliableOrdered, ReliableUnordered}`, per-channel configs, a per-connection send budget, and RTT/loss/bytes metrics. `NetcodeServerTransport`/`NetcodeClientTransport` take a concrete `UdpSocket` and expose no transport trait, so a synthetic conditioner must sit below renet as a UDP relay.
- `RenetServer::new_local_client`/`process_local_client` exist for socket-free in-process tests.

## 2. Locked decisions

| Decision | Choice |
|---|---|
| Transport | `renet` 2.0.1 + `renet_netcode` 2.0.1, standalone, **no Bevy** |
| Wire format | Hand-rolled bit codec in `acvd-proto`; no JSON/serde on the hot path |
| Sim math | glam 0.32.1 `f32`, fixed 60 Hz; server-authoritative correction, not lockstep |
| Authority | Authoritative server; clients send **inputs only**, never positions/damage |
| Topology | Client-server only; no P2P mesh |
| Listen server | Background thread running the same server loop; host client connects over loopback UDP |
| Conditioner | Transparent UDP relay (RTT/jitter/loss/reorder/duplicate), seeded |
| Snapshot cadence | 30 Hz near, 10 Hz mid, 2 Hz far; 60 Hz input |
| Hit authority | Server simulation + hitbox-history rewind for hitscan/fast projectiles |
| Deployment | Server binary is GPU-free; operator supplies the owned disc/generated data at build/runtime |

### 2.1 Transport rationale (why renet)

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **renet + renet_netcode** | Exact `Unreliable` / `ReliableOrdered` / `ReliableUnordered` channels; per-tick send budget; RTT/loss/bytes metrics; netcode.io encryption/handshake; no async runtime; small dep set (`bytes`, `octets`, `renetcode`); socket-free local-client test path | Netcode adds handshake/token overhead; no public transport trait, so netem must be a UDP relay; not as bit-controllable as a custom layer | **Chosen** |
| Custom UDP over `socket2` | Maximum control of bytes/budgets; zero protocol overhead | Must build reliability, ordering, congestion, handshake, replay protection, and tests; highest risk | Rejected for now |
| `laminar` | Reliable UDP with channels; simpler than custom | Extra packet semantics; less control of the hot-path bit layout; another protocol to learn/debug | Rejected |
| `tokio` + `quinn` (QUIC) | Robust congestion control; streams | QUIC streams introduce head-of-line blocking; conflicts with the separate unreliable hot path; async runtime is heavy for a fixed-tick sim | Rejected |
| `renet` + Bevy replication | Turnkey ECS replication | Couples the headless server to Bevy ECS; opinionated serialization; violates strict headless separation | Rejected |

`acvd-net` wraps renet so the transport can be replaced later without touching `acvd-sim` or `acvd-server`.

## 3. Target crate graph and exact boundaries

```
acvd-formats (no Bevy)  acvd-data (no deps)
        \                  /
         +------ acvd-sim (no Bevy, no GPU, no window/audio)
                  |
              acvd-proto (wire structs + bit codec; glam/bytes only)
                  |
              acvd-net (renet + renet_netcode + conditioner; no Bevy)
                  |
         +--------+---------+
         |                  |
   acvd-server         acvd-game (client adapter)
   (lib + bin)         + acvd-render (Bevy assets)
   no acvd-render      + Bevy window/audio/UI
```

Add to workspace members: `crates/acvd-sim`, `crates/acvd-proto`, `crates/acvd-net`, `crates/acvd-server`.

### `acvd-sim` (pure simulation)

Dependencies: `acvd-data`, `acvd-formats`, `glam = "0.32"`, `anyhow`.
Must not depend on: `bevy`, `acvd-render`, `image`, `wgpu`, `winit`, `cpal`, `alsa`, `rodio`.

Modules:
- `tick.rs`: `Tick(u32)`, `TICK_HZ = 60`, `TICK_DT = 1.0/60.0`.
- `ids.rs`: `NetId(u8)` (**1..=254**; `0 = none`, `255 = reserved`), `PlayerId`, `ProjectileId(u32)`, `PropId(u32)`.
- `input.rs`: `InputFrame { seq: u16, buttons: u16, stick: [i8;2], aim_yaw: u16, aim_pitch: i16, lock_target: NetId }`. Aim is absolute; the server clamps the AC's per-tick turn to `AcCtrlParam` turn speed, so packet loss cannot cause unbounded aim drift.
- `motion.rs`: `AcMotion` (the movement subset of today's `Pilot`, **without** `FollowCam`), `step_tick`, `start_quick_boost`, `start_wall_jump`, `start_kick`, `start_blade`, `step_en`, `check_water_return`, prop-crush callback.
- `collision.rs`: today's `collision.rs`, Bevy imports replaced with glam, `Resource` derive removed.
- `damage.rs`: today's pure damage model, Bevy imports removed.
- `map.rs`: `Destructible` sim data, `DestructibleSet` (bitset + hp), `load_destructibles(disc, map)`.
- `weapons.rs`: `Gun`, `Flight`, `MissileFlight`, `RangeDecay`, `ImpactBlast`, `Projectile`, `Missile`, `Armament` fire/salvo state, `BladeSpec`, `OveredWeaponSpec/State`; `step_tick` only.
- `world.rs`: `SimWorld` with `HashMap<NetId, SimAc>`, projectiles, `DestructibleSet`, `tick`, `step`, snapshot-record production.
- `events.rs`: sim events (`Fired`, `ProjectileHit`, `Damaged`, `Killed`, `PropBroken`, `Respawned`).
- `rng.rs`: deterministic PCG32 seeded from `(match_seed, tick, net_id)`; no `rand` dependency.
- `stats.rs`: loadout -> `AcCtrlParam` + `Armament` (pure data via `acvd-data`).

### `acvd-proto` (wire contract)

Dependencies: `glam`, `bytes`.
Owns: `PROTOCOL_VERSION`, `BitWriter`, `BitReader`, quantizers, message structs, encode/decode, and golden-size tests. No game logic, no renet.

### `acvd-net` (transport only)

Dependencies: `acvd-proto`, `renet`, `renet_netcode`, `bytes`, `anyhow`, `log`.
Owns: channel constants/config, `NetServer`/`NetClient` wrappers, connection/token handling, `netem` relay, metrics. No sim types, no Bevy.

### `acvd-server` (authoritative runtime; lib + bin)

Dependencies: `acvd-sim`, `acvd-proto`, `acvd-net`, `anyhow`, `log` (CLI args may be manual or `clap`).
Owns: tick loop, input queues, interest management, lag compensation, match/drop-in/respawn, snapshot builder, bot driver, soak metrics. Exposes `run(config, shutdown) -> Result<()>` for the listen-server; `main.rs` is the dedicated binary.

### `acvd-game` (client)

Existing deps plus `acvd-sim`, `acvd-proto`, `acvd-net`. Owns: input sampling, prediction/reconciliation, remote interpolation, `NetId -> Entity` view, event presentation, camera/audio/UI/animation. `acvd-render` stays Bevy-only.

## 4. Simulation contract (`acvd-sim`)

- **One tick = one 60 Hz step.** No `time.delta_secs()` inside sim. Convert `Projectile::step(dt)`/`Missile::step(dt)` to `step_tick()` with `frames = 1.0`, `dt = 1/60`.
- **No wall clock, no `Entity`.** Targets/projectiles reference `NetId`/`ProjectileId`/`PropId`.
- **No camera, audio, animation, UI, assets.** Sim emits state flags and events; the client maps them to clips/effects.
- **Single source of truth.** The client's offline mode, client prediction, and dedicated server all call the same `step_tick`.
- `SimWorld::step(inputs: &[(NetId, InputFrame)]) -> Vec<SimEvent>`:
  1. apply one validated input per AC (or repeat/idle per server policy),
  2. integrate motion + collision + prop crush,
  3. step projectiles/missiles/melee, resolve hits with rewind hooks supplied by the server,
  4. tick stagger/EN/reload/respawn timers,
  5. emit events.
- `AcMotion` keeps today's movement parity fields (`velocity`, `en`, `boost`, `glide`, phase timers, `qb`, `wj`, `kick`, `blade`, `ready_cam`, counters). `FollowCam`, camera blends/shake/blur, and clip selection stay in `acvd-game`.

## 5. Wire protocol

### Channels (renet)

| Channel | Direction | SendType | Payload |
|---|---|---|---|
| 0 `HOT` | both | `Unreliable` | inputs, snapshots, FireEvent, missile corrections |
| 1 `RELIABLE` | server->client | `ReliableOrdered { resend_time: 50ms }` | lifecycle/world transitions only: JoinAccepted, PlayerJoined/Left, LoadoutUpdate, MatchState, KillEvent, RespawnDrop, DestructibleBreak |
| 1 `COMMAND` | client->server | `ReliableOrdered { resend_time: 50ms }` | join, loadout, respawn request, team select |
| 2 `BULK` | server->client | `ReliableOrdered { resend_time: 100ms }` | late-join full world/loadout/destructible state |
| 3 `COMBAT` | server->client | `ReliableUnordered { resend_time: 50ms }` | DamageEvent, ProjectileHit, ProjectileExpire (coalesced per target per tick) |

Set `available_bytes_per_tick` to **4096** (not renet's 60,000 default) and `max_memory_usage_bytes` per channel. Server channel priority order: `HOT`, `COMBAT`, `RELIABLE`, `BULK` so high-frequency combat feedback cannot be starved by bulk state and `RELIABLE` is not blocked by combat traffic. Gatling firefights therefore cannot head-of-line block kills or destructible transitions.

### Hot-path messages

- **InputPacket** (client -> server, 60 Hz): `start_seq: u16`, `count: u8` (1..3), then the last 1-3 unacked frames. Frame = `buttons u16 + stick 2xi8 + aim_yaw u16 + aim_pitch i16 + lock_target u8` = 9 B. Packet ~30 B -> ~14 kbps. Aim is absolute; the server clamps the AC's per-tick turn to `AcCtrlParam` turn speed, so loss cannot cause unbounded aim drift.
- **SnapshotPacket** (server -> client, per snapshot tick): `server_tick u32`, `snapshot_seq u16`, `acked_input_seq u16`, `ac_count u8`, `missile_correction_count u8`, `flags u8`, local-player high-precision record (optional), then AC records. **Each HOT SnapshotPacket must fit one UDP datagram: <=1100 B.** When records exceed 1100 B (around 30-34 records at 32 B), emit up to 2 independent unreliable SnapshotPackets per snapshot tick; `LocalPlayerState` is always in packet 1 so losing a fragment never drops local reconciliation.
- **AcRecord** (target 32 B, hard ceiling 48 B):
  - `net_id: u8`
  - `pos: 3 x u16` quantized against the map AABB from join data (~3 cm)
  - `vel: 3 x s16`, 0.01 m/tick
  - `yaw: u16`, full turn
  - `pitch: i8`
  - `flags: u16` (boost, glide, airborne, scan, qb, wall-jump, kick, blade, reload L/R, ready L/R, overed, destroyed, team, invulnerable)
  - `pose_state: u8` (locomotion enum for remote animation)
  - `ap: u16`, `en: u8`, `reload: 2 x u8`, `ammo: 2 x u8`
  - Total ~24-28 B; add padding/optional fields to stay <=32 B.
- **LocalPlayerState** (one per snapshot, always in packet 1): authoritative `pos 3xf32`, `vel 3xf32`, `yaw f32`, `pitch f32`, `flags`, `en`, `ap`, `last_acked_seq`, plus the `AcMotion`/combat phase and cooldown timers (`qb`, `wj`, `kick`, `blade`, `stagger`, EN-regen delay, glide phase/timers). Reconciliation replay starts from exact state instead of approximating timers; cost ~96-128 B at 30 Hz (~3-4 KB/s).
- **MissileCorrection** (unreliable, for homing only): `projectile_id u32`, `pos 3xs16`, `vel 3xs16`, `target NetId`. Cap count per snapshot; only nearby/locked missiles.
- **FireEvent** (unreliable, repeated for 2 ticks, deduped by `projectile_id`): `shooter NetId`, `projectile_id u32`, `weapon_slot u8`, `muzzle_pos 3xi16`, `aim_dir 3xi8` (octahedral), `fire_tick u32`, `seed u8`. Never stream per-bullet transforms.

### Lifecycle/world events (Channel 1, ReliableOrdered)

`JoinAccepted`, `PlayerJoined`, `PlayerLeft`, `LoadoutUpdate`, `MatchState`, `KillEvent`, `RespawnDrop`, `DestructibleBreak`. Keep this channel strictly low-frequency so a gatling firefight cannot head-of-line block a kill or a destructible transition.

### Combat feedback (Channel 3, ReliableUnordered)

`DamageEvent` (target, attacker, amount, attribute, effective, lethal, hit_pos, tick), `ProjectileHit`/`ProjectileExpire` (for correcting client visuals). Coalesce damage per target per tick; reliable-unordered delivery cannot block Channel 1. Clients apply these idempotently (ignore damage for an already-dead target) because they may arrive after a `KillEvent` from another channel.

### Hot-path events (Channel 0, Unreliable)

`FireEvent` and `MissileCorrection`; snapshots and inputs. Repeats/next snapshots cover loss.

### Rates and bandwidth budget

- Sim 60 Hz; input 60 Hz; snapshots 30/10/2 Hz by tier.
- 64 ACs all near: `64 x 32 B = 2048 B`, split into 2 SnapshotPackets (<=1100 B each) per snapshot at 30 Hz = **61 KB/s (~491 kbps)** per client; server egress **~31 Mbps**; plus ~3-4 KB/s local state.
- 24 players: ~23 remote x 32 B x 30 Hz = ~22 KB/s (~176 kbps) + inputs ~14 kbps + local state ~3-4 KB/s.
- Hard cap: **1100 B per SnapshotPacket, at most 2 packets per snapshot tick per client**; `LocalPlayerState` always in packet 1; when the record set exceeds 2 x 1100 B, drop tier-2 first, then tier-1, and increment a dropped-record metric.

## 6. Server runtime

Loop (fixed 60 Hz, catch-up capped at e.g. 4 ticks per iteration):
1. `transport.update(dt, &mut server)`; `server.update(dt)`.
2. Drain `ServerEvent`s and `receive_message` on `COMMAND`/`HOT`.
3. `InputQueue`: validate and store per-client frames by seq; allow at most 2 new ticks per server tick; drop duplicates/old/too-future; repeat the last input for at most N starved ticks, then idle.
4. `SimWorld::step` for all ACs, projectiles, props.
5. Push `HitboxHistory` for every AC (90-tick ring: `pos`, `yaw`, flags).
6. On every 2nd tick (30 Hz), build up to 2 independent `SnapshotPacket`s per client (each **<=1100 B**; `LocalPlayerState` always in packet 1), then `send_message(HOT, ...)`; lifecycle events go on `RELIABLE`, combat feedback on `COMBAT`.
7. `transport.send_packets(&mut server)`.
8. Sleep until the next tick; record tick timing.

### Input validation (anti-cheat baseline)

- Accept inputs only; never positions, health, damage, or hits.
- Clamp axes, rate-limit packets, bound seq advancement, validate fire against cooldown/ammo/EN, validate movement mode transitions server-side.
- Derive turn from absolute aim each tick: `turn = clamp(wrap(aim_yaw - motion.yaw) / (ctrl.turn_rate / TICK_HZ), -1, 1)`, and feed `aim_pitch` to the pitch spring. The server never accepts a client turn delta, so loss cannot accumulate aim drift beyond one tick's clamp.
- Client-supplied `lock_target` is only an interest-priority hint; server damage/lock authority does not trust it.

### Interest management (from day 1)

- Rebuild a uniform XZ spatial grid each snapshot tick from AC positions.
- Tier 0 (near): <=150 m, or locked by the client, or same team <=250 m, or damaged/attacked in the last 3 s -> 30 Hz, full record.
- Tier 1 (mid): <=400 m -> 10 Hz, full record.
- Tier 2 (far): <=800 m -> 2 Hz, reduced record (`net_id`, `pos`, `yaw`, flags).
- Cull beyond 800 m (team/objective markers may override).
- Build the global `AcRecord` list once per snapshot tick, then per-client filter/copy into reusable buffers; no per-client per-record allocation.

### Lag compensation

- `HitboxHistory` per AC, 90 ticks (1.5 s), stores `pos`, `yaw`, and enough state to reconstruct `damage::ac_hit_spheres()`.
- On a fire at `fire_tick`, compute `rewind_ticks = clamp(rtt_ticks/2 + INTERP_DELAY_TICKS + jitter_ticks, 0, MAX_REWIND_TICKS)`; default `INTERP_DELAY_TICKS = 4` (66 ms = 2 snapshot intervals at 30 Hz), `MAX_REWIND_TICKS = 12` (~200 ms) so 120 ms ping + jitter still fits.
- Hitscan and fast projectiles: validate against rewound hit spheres and current static terrain.
- Slow projectiles: authoritative forward simulation; use a small rewind window only where the weapon opts in.
- Apply damage/stagger/destruction only on the server; emit `DamageEvent`/`KillEvent`.

### Match, drop-in/drop-out, respawn

- `MatchConfig { mode: Deathmatch | TeamDeathmatch, max_players: 24, bot_fill: u8, friendly_fire, respawn_secs, score_limit, time_limit }`.
- Join: validate `PROTOCOL_VERSION`; assign `NetId`/team; send `JoinAccepted` on `BULK` (map, quantizer, config, all loadouts, destructible bitset + damaged hp) and `PlayerJoined` to others.
- Leave/timeout: `PlayerLeft`; despawn the AC; keep world/destructibles.
- 24/7 deathmatch: rounds reset by score/time; players may join mid-round.
- Death: schedule `respawn_at_tick`; choose a drop point deterministically from map spawn points + seeded PCG; send `RespawnDrop { netId, drop_point, approach_yaw, start_tick }`; clients evaluate locally.
- Loadout changes only on join/respawn; `LoadoutUpdate` once, never per tick.
- Custom COM voice/portrait: never transmitted.

### Dedicated vs listen server

- `acvd-server` lib exposes `run(config, shutdown)`.
- Dedicated binary: parse args, install Ctrl-C, call `run`.
- `acvd-game --listen`: start `run` on a `std::thread`, connect the client to `127.0.0.1:<port>` via renet_netcode; on exit set `shutdown`, join. No shared world state.
- Socket-free unit tests use `RenetServer::new_local_client`; network-condition tests and listen mode use loopback UDP + relay.
- Netcode auth: use `renet_netcode` connect tokens. For friend lobbies the host generates/shares the token; for tests use the unsecure auth path. Public matchmaking/token issuance is out of scope.

## 7. Client runtime

### Input sampling and prediction (local AC)

- Fixed 60 Hz input sampler writes `InputFrame` (absolute `aim_yaw`/`aim_pitch`) with a monotonically increasing `seq` into a 128-entry ring.
- Predict immediately with `acvd-sim::motion::step_tick`; render the local AC from the predicted state (no interpolation). The server still clamps the applied turn to `AcCtrlParam` turn speed, so a lost packet cannot leave the aim permanently ahead.
- Send the current + last 2 unacked frames every tick on `HOT`.
- On snapshot: read `LocalPlayerState` + `last_acked_seq`; drop acked frames.
  - Error within tolerance (e.g. 5 cm, 1 degree): smooth over ~100 ms.
  - Error above tolerance: reset to authoritative state, apply server destructible deltas, replay all unacked frames, then render.
- If dead/respawning, stop prediction until the respawn event/snapshot activates the AC.

### Remote interpolation

- Buffer snapshots keyed by `server_tick`; render at `estimated_server_tick - INTERP_DELAY_TICKS` (**4 ticks, 66 ms = 2 snapshot intervals at 30 Hz**; 5 under jitter) for real loss/jitter headroom.
- Lerp `pos`/`vel`, slerp/shortest-arc `yaw`, snap flags/pose/health.
- If the buffer starves, extrapolate at most 100 ms, then hold.
- Remote projectile visuals are local/deterministic from `FireEvent`; do not interpolate bullet transforms.

### View and events

- `NetView`: `HashMap<NetId, Entity>` for ACs, `HashMap<ProjectileId, Entity>` for visual projectiles, `HashMap<PropId, Entity>` for destructibles.
- Apply `LoadoutUpdate` on join/respawn only.
- `FireEvent` spawns the visual projectile via the same sim data; `ProjectileHit`/`ProjectileExpire` resolves/cleans it.
- `DamageEvent`/`KillEvent` drive HUD/sfx; `DestructibleBreak` updates local collision (`break_prop`) before replaying prediction and spawns the existing debris/FFX presentation.
- `RespawnDrop` plays the local drop-in presentation at `start_tick`.

## 8. Synthetic network conditioner

- `acvd-net/src/netem.rs`: a UDP relay thread between client and server datagrams.
- Config: `rtt_ms`, `jitter_ms`, `loss_pct`, `duplicate_pct`, `reorder_pct`, `seed`.
- Implements a delayed-packet priority queue; reorder by adding extra delay; loss/duplicate by seeded PCG.
- Transparent to renet_netcode because it forwards raw datagrams.
- Netem tests use unsecure auth and generate the connect token for the relay address; the relay forwards to the real server.
- Client flag: `--netem <rtt,jitter,loss,reorder,seed>` (debug/test only).
- Direct loopback remains the fast path for CI; conditioner is mandatory for lag/loss acceptance tests.

## 9. Phased tasks and acceptance gates

### Phase 0 - Headless sim decouple

1. Add `acvd-sim` to the workspace and create the module skeleton; add `glam = "0.32"` to workspace deps.
2. Move `collision.rs` and `damage.rs` into `acvd-sim`; replace `bevy::prelude` with glam; remove ECS derives. In `acvd-game`, add `CollisionRes`/re-exports and update systems.
3. Split `control.rs`: move `Pilot`'s non-camera fields to `acvd-sim::motion::AcMotion`, move `Input` to `InputFrame`, move `step_*`, `start_*`, `step_en`, `crush_contacted_props`, `check_water_return`, `body_stick`. Keep `FollowCam`, camera blends/shake/blur, and `state()` clip mapping in `acvd-game`.
4. Convert `Pilot` in `acvd-game` to a component wrapper `{ motion: AcMotion, cam: FollowCam, counters/visual fields }`; compile-driven field updates.
5. Move destructible sim data into `acvd-sim::map`; factor metadata loading out of `map::spawn`; keep Bevy entity spawning and debris/FFX in `acvd-game`.
6. Keep single-player behavior identical: call `motion.step_tick` once per 60 Hz accumulator step; no camera/visual changes.
7. Port existing movement unit tests to `acvd-sim`; add `movement_parity.rs` with a scripted input sequence and expected final state recorded **before** the refactor.
8. Add `tools/check-headless.ps1` asserting `cargo tree -p acvd-sim` has no forbidden crates.

Gate:
- `cargo test -p acvd-sim` passes.
- `cargo tree -p acvd-sim --edges normal` has no `bevy`, `wgpu`, `winit`, `cpal`, `alsa`, `acvd-render`.
- Single-player `--shot` matches the pre-refactor reference via `acvd-shot diff` / `tools\shot-check.ps1`.
- Movement telemetry (position/yaw/velocity/EN over a scripted run) is unchanged within epsilon.

### Phase 1 - `acvd-proto` + `acvd-net` + `acvd-server` skeleton + lag/loss sim

1. Add `acvd-proto`: bit writer/reader, varint/zigzag, quantizers, message structs, version constant, golden byte-size tests.
2. Add `acvd-net`: channel constants/config; `NetServer`/`NetClient` wrappers; update/send order per renet; metrics from `rtt`, `packet_loss`, `bytes_sent_per_sec`, `bytes_received_per_sec`.
3. Add `acvd-net::netem` relay with seeded RTT/jitter/loss/reorder/duplicate.
4. Add `acvd-server` lib+bin skeleton: fixed-tick loop, bind UDP, accept/join/leave, echo a dummy AC snapshot, graceful shutdown.
5. Add socket-free `new_local_client` integration test and loopback two-client test.
6. Add `tools/check-headless.ps1` gate for `acvd-server`.
7. Add an operator-built `Dockerfile`: multi-stage Rust build; generated tables and the owned disc are supplied as BuildKit secrets/mounts (never committed/published); runtime mounts the disc at `--disc`. Add a release build check for both Windows and Linux.

Gate:
- Two clients connect to the dedicated server on Windows and Linux, join, exchange snapshots/inputs, and disconnect cleanly.
- 120 ms RTT + 2% loss + 30 ms jitter: reliable join/loadout still completes; metrics report loss/rtt.
- `cargo tree -p acvd-server --edges normal` has no `bevy`, `wgpu`, `winit`, `cpal`, `alsa`, `image`, `acvd-render`.
- The same `acvd-server` binary starts headless on Windows, Linux, and in the operator-built Docker image; it fails fast with a clear message if the disc/generated data is missing.

### Phase 2 - Movement CSP, remote interpolation, late join

1. `acvd-sim::world`: `SimWorld`, `SimAc`, per-player input application, snapshot-record production.
2. Server: `InputQueue`, validation, per-tick stepping, `last_acked_input_seq`, join/late-join bulk state, player spawn/despawn.
3. Client: input sampler + ring, prediction, reconciliation/replay, remote interpolation buffer, `NetView`, local/remote transform updates.
4. Keep solo play on a `LocalSession` (direct sim, no transport); `--listen` uses the server thread + loopback.
5. Apply server destructible deltas to local collision before replay.

Gate:
- Scripted boost glide, wall jump, and kick under 80 ms and 120 ms RTT with 2% loss feel immediate and do not rubber-band beyond the configured tolerance.
- Remote ACs move smoothly with a 4 tick (66 ms) interpolation buffer; no visible snapping on normal loss.
- A client joining mid-match receives all ACs, loadouts, and positions at the correct tick.
- Drop-in/drop-out churn leaves no orphaned entities or stale inputs.

### Phase 3 - Combat, FireEvents, lag compensation, destructibles

1. Move weapon sim into `acvd-sim`; convert projectile/missile stepping to `step_tick`.
2. Server: per-AC weapon state, ammo/cooldown/EN validation, projectile/missile/melee simulation, damage/stagger, kill/respawn events.
3. Server: `HitboxHistory`, rewind policy, hitscan/fast-projectile validation, `DamageEvent`/`KillEvent`.
4. Server: broadcast `FireEvent` (unreliable + 2 repeats), missile corrections in snapshots, destructible break deltas + late-join bitset/hp.
5. Client: replace client-authoritative combat with FireEvent visuals and server events; keep existing FFX/audio/debris presentation.
6. Client: consume `DestructibleBreak` before replay; update local collision.

Gate:
- A 120 ms-ping sniper/rail shot at a strafing target validates through rewind and produces the correct damage.
- Fast projectiles hit at the shooter's view tick; slow projectiles are server-authoritative.
- No per-bullet transforms on the wire; only FireEvents + homing corrections.
- Destructible break state matches for early and late joiners; respawn drops are single deterministic events.
- Kill/respawn/loadout sequences are consistent for all clients.

### Phase 4 - Headless 64-bot soak and budgets

1. `acvd-server --bots N --duration S --metrics <json>`: deterministic bot driver (patrol, aim at nearest enemy, fire) producing `InputFrame`s; spawn up to 64 ACs.
2. Collect tick/sim/snapshot/encode/net timings, per-client kbps, RTT/loss, memory, dropped records.
3. Run cluster distribution and worst-case all-near; assert tick and bandwidth budgets.
4. Add 24-player drop-in/out churn for 10 minutes.

Gate (reference machine, configurable in the harness):
- p95 total tick < 4 ms, p99 < 8 ms at 64 ACs.
- No NaN/desync; all positions finite; each SnapshotPacket <=1100 B and at most 2 per snapshot tick per client (LocalPlayerState always in packet 1); per-client average <500 kbps, worst <1.5 Mbps; server egress <35 Mbps at 64 all-near.
- Memory stable over 10 minutes; no entity/message leaks.
- `cargo test -p acvd-server -- --ignored soak` or the release soak command passes and writes `private/soak/` metrics (gitignored).

## 10. File-level change map

New:
- `crates/acvd-sim/{Cargo.toml,src/{lib,tick,ids,input,motion,collision,damage,map,weapons,world,events,rng,stats}.rs,tests/{movement_parity,world_determinism}.rs}`
- `crates/acvd-proto/{Cargo.toml,src/{lib,bit,messages,quantize}.rs}`
- `crates/acvd-net/{Cargo.toml,src/{lib,channels,server,client,netem,metrics}.rs}`
- `crates/acvd-server/{Cargo.toml,src/{lib,main,input_queue,interest,lagcomp,match_state,snapshot,bots}.rs,tests/soak.rs}` (`match_state.rs`, not `match.rs`; `match` is a reserved keyword)
- `crates/acvd-game/src/net/{mod,input_sampler,prediction,interpolation,view,session,events}.rs`
- `tools/check-headless.ps1`
- `Dockerfile` (operator-built only; no disc/generated data committed)

Moved/split:
- `crates/acvd-game/src/{collision,damage}.rs` -> `acvd-sim`; game keeps re-exports/newtypes.
- `crates/acvd-game/src/control.rs` -> `acvd-sim/src/motion.rs` (movement) + game camera/view remainder.
- `crates/acvd-game/src/weapons.rs` -> `acvd-sim/src/weapons.rs` (sim) + game presentation remainder.
- `crates/acvd-game/src/map.rs` destructible metadata -> `acvd-sim/src/map.rs`; game keeps spawning/debris/FFX.
- `crates/acvd-game/src/targets.rs` -> sim `TargetHealth`/hit spheres + game `Target`/visuals.
- `crates/acvd-game/src/main.rs`: mode wiring (`--listen`, `--connect`, `--netem`, `--disc`).

Docs:
- Update `docs/open.md`/`docs/archive.md` per the one-task-per-chat rule.
- Add system rows to `sheets/systems.csv` for `acvd-sim`, `acvd-proto`, `acvd-net`, `acvd-server`, and multiplayer status.
- No new game constants without a sheet row or 360 address; netcode constants (tick/snapshot/budget) are design config, documented here and in code.

## 11. Verification commands

```powershell
$env:CARGO_TARGET_DIR = "$PWD\target"; $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

cargo check -p acvd-sim -p acvd-proto -p acvd-net -p acvd-server --message-format=short
cargo test -p acvd-sim
cargo test -p acvd-proto
cargo test -p acvd-net
cargo test -p acvd-server

cargo run --release -p acvd-server -- --bind 0.0.0.0:5000 --max-clients 64 --disc <path> --map m4000
cargo run --release -p acvd-server -- --bots 64 --duration 600 --metrics private\soak\64bots.json

# Headless gate: expect no output
cargo tree -p acvd-server --edges normal | Select-String -Pattern "bevy|wgpu|winit|cpal|alsa|image|acvd-render"

# Visual parity after the Phase 0 split
cargo run -p acvd-game -- 6007 --shot private\shots\net_phase0.png --wait 2
.\tools\shot-check.ps1 -Ref <pre-refactor-ref.png> -Shot private\shots\net_phase0.png
```

## 12. Risks and open questions

1. **Disc data on the server.** `acvd-sim` needs `acvd-formats` map collision and the generated `acvd-data` tables; both are disc-derived and gitignored. The server binary/image must be built by the disc owner and must never publish generated data. Mitigation: `--disc` at runtime, operator-built Docker image, optional private map-collision cache.
2. **f32 determinism.** Prediction/replay is same-binary; not guaranteed bit-exact across OS/compiler. This is intentional: server authority + correction, not lockstep. Do not claim cross-platform determinism.
3. **Netcode tokens.** Public deployment still needs a token-distribution path; friend lobbies can share a generated token. Matchmaking is out of scope.
4. **Pose mapping.** Sim emits `pose_state`; the client maps it to `acmotion`/booster clips. Keep `state()` in `acvd-game` and add a remote mapping table; verify remote animation visually.
5. **Destructible prediction divergence.** Clients must apply reliable break deltas before replay; otherwise local prediction may pass through a prop the server considers broken. The server correction is authoritative.
6. **Bots.** Phase 4 bots are load generators, not retail AI. `ai.rs` remains single-player/client scope until a later task.
7. **Conditioner vs secure netcode.** The relay must forward raw datagrams and tokens must point at the relay address (or use unsecure test auth); this is test tooling, not production routing.
8. **Listen-server thread shutdown.** The server thread owns the sim; signal with `Arc<AtomicBool>`, `disconnect_all`, join the thread. Never touch Bevy world from the server thread.

## 13. Out of scope

- Matchmaking, accounts, persistence, and dedicated-server orchestration.
- P2P/relay mesh, rollback netcode, or lockstep.
- Full anti-cheat beyond input validation/server authority.
- Retail AI parity for multiplayer bots.
- Voice/portrait networking (explicitly client-side).
