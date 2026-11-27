# Xenia probes (agent-driven)

Runtime questions about the Xbox 360 build go through this project's Xenia, **driven by the
agent**, not a human with a pad. The emulator runs the same `ACV2.pe` Ghidra reads (base
`0x82000000`), so probe addresses are the Ghidra ones with no translation. There is no other
emulator; never cite RPCS3, EBOOT or PS3 addresses.

This is the full how-to for any model working in this repo (Cursor Grok, Claude, GPT, …). Short
syntax lives in `tools/xenia/probes.example.txt`; the scripts are `tools/xenia/run.ps1` and
`tools/xenia/drive.py`.

## Quick start

From the repo root, in PowerShell:

```powershell
# 1. Find the instruction (static first): bounds, callers, decompile.
& target\release\acvd-index.exe q func 0x827f3d38
# 2. Write a probe list under private\xenia\ (gate on env_bgm so menus stay out).
@"
0x82800128 env_bgm max=1 r4
0x827f3d38 sky_begin max=3 every=60 after=env_bgm r4 r5
"@ | Set-Content private\xenia\mytask.txt
# 3. Boot the disc, drive the menus into AC TEST, log hits, close (about 150 s).
.\tools\xenia\run.ps1 private\xenia\mytask.txt -Recipe tools\xenia\recipes\ac_test.txt
# 4. Read private\xenia\mytask.jsonl (one JSON object per hit).
```

## When to probe

Use a probe when static reading cannot answer it:

- Does this path fire in the AC test / on this map?
- What value is in a register or struct at that moment, and in what units?
- How often does it run (once per draw, per tick, per mission load)?
- What world / view / shader matrix is actually uploaded?

Do **not** probe instead of `acvd-index` / ghidra-cli. Look the function up first, pick one
instruction, then probe that.

Hand the pad to the user only when no recipe can reach the state (a specific live mission, an
unscripted menu). Write the probe file, give them `.\tools\xenia\run.ps1 …` without `-Recipe`,
and read the `.jsonl` after. That is the exception.

## Setup (once)

```powershell
# Needs VS 2022 C++, Python 3, Vulkan SDK (VULKAN_SDK). Safe to re-run.
.\tools\xenia\setup.ps1
```

That clones `external/xenia-canary` (gitignored), applies `tools/xenia/acvd-probe.patch` on
branch `acvd-debug`, and builds `external\xenia-canary\build\bin\Windows\Release\xenia_canary.exe`.
`run.ps1` throws if that binary is missing.

The 360 ISO must be at `armoredcoredumps\Armored Core - Verdict Day (USA)\Armored Core - Verdict Day (USA).iso`.
A signed-in Xenia profile must already exist in **slot 1** (the scripted pad is port 1).

## The run you actually issue

Always this shape for gameplay:

```powershell
.\tools\xenia\run.ps1 private\xenia\TASK.txt -Recipe tools\xenia\recipes\ac_test.txt
```

- Starts Xenia with the probe list, `protect_zero=false`, `readback_resolve=full`, no game patches.
- Puts the scripted controller on port 1.
- Plays the recipe (menus, AC TEST, optional walk / look / fire).
- Closes Xenia when the recipe finishes.
- Prints a per-label hit count.
- Hits land in `private\xenia\TASK.jsonl` (same stem as the probe file unless `-Log` is set).
- Guest-frame PNGs from `shot` land where the recipe says (usually `private/xenia/shots/`).

`private/xenia/` is gitignored. Probe lists and jsonl belong there. Committed recipes live in
`tools/xenia/recipes/`.

Typical wall times: `ac_test.txt` ≈ 150 s, `ac_test_walk.txt` a bit longer. Give the shell
`block_until_ms` ≥ 400000. Do not start a second Xenia.

`-Seconds N` is the **no-input** variant (boot far enough to hit a function, then kill). It
does not drive menus. Do not use it for AC TEST.

`-Drive` without `-Recipe` is the live pad-file mode: you append steps yourself with
`py tools\xenia\drive.py <log>.input …`. `-Recipe` already implies `-Drive`.

### Agents whose shell cannot block for 3 minutes

`run.ps1 -Recipe` blocks until the recipe ends, then kills Xenia and prints the counts. If your
harness times commands out sooner (some CLI agents cap at 60-120 s) or detaches long jobs,
launch it in its own process and poll:

```powershell
Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-File','tools\xenia\run.ps1',
  'private\xenia\mytask.txt','-Recipe','tools\xenia\recipes\ac_test.txt' `
  -RedirectStandardOutput private\xenia\mytask.out
# poll every ~30 s until the run is over:
Get-Process xenia_canary -ErrorAction SilentlyContinue   # gone = finished
Get-Content private\xenia\mytask.out                       # per-label counts at the end
```

Never start a second run while `xenia_canary` is alive; wait for it or `Stop-Process -Name
xenia_canary`. The run needs a desktop session (it opens a window); it does not need focus.

## Probe file

One probe per line. `#` starts a comment. The hook runs **before** the instruction at
`ADDRESS` executes.

```
ADDRESS LABEL [max=N] [every=N] [after=LABEL[:N]] READ...
```

| token | meaning |
|---|---|
| `ADDRESS` | 360 address, `0x82……`. One probe per address; a second line at the same PC is dropped. |
| `LABEL` | unique name. `after=` matches this string. |
| `max=N` | log at most N hits (default 200; `max=0` = no cap). |
| `every=N` | log 1 of N executions (first after the gate, then every Nth). |
| `after=LABEL` | stay silent until that probe has **executed** once (`seen`, counted even past its `max`). |
| `after=LABEL:N` | wait for N executions of that probe. |
| `READ` | values copied into the JSON line. |

### Reads

| form | JSON |
|---|---|
| `r0`…`r31` | 32-bit hex string, `"0x........"` |
| `f0`…`f31` | PPC FPR as a double; non-finite → `null` |
| `lr` `ctr` | hex string |
| `[BASE+OFF]:TYPE` | big-endian guest load. `TYPE` = `u8` `u16` `u32` `s8` `s16` `s32` `f32` `f64`. `u32` is hex; the rest are JSON numbers. |
| `[[r3+0x4]+0x1bc]:u32` | pointer chase; inner `[]` loads are always `u32`. Offset may be `-0x10`. |

Unreadable guest memory logs `null`. There are **no VMX / Altivec register reads**; if Ghidra
says `bad instruction data`, probe the scalar stores around that block instead.

PPC ABI: `r3` = first argument / this, `r4` `r5` `r6`… further args, `lr` = caller. Probing the
first instruction of a function sees the incoming args. Probing a `bl` site sees args **before**
the call (the callee has not run).

Hot paths (draw setup, per-bone, per-vertex) need `every=` and a modest `max=` or the log fills
and the game stutters. Per-draw work in a mission should sit behind `after=env_bgm` so the
title / workshop menus do not pollute it:

```
0x82800128 env_bgm max=1 r4
0x82c0a160 draw_setup every=11 after=env_bgm r3
```

`0x82800128` is the type-400 (BGM) env-event branch; it fires when a **mission** env applies,
including AC TEST, not the front-end.

Write the list under `private/xenia/` (UTF-8, one line per probe). Example:

```powershell
@"
0x82800128 env_bgm max=1 r4
0x827f3d38 sky_begin max=3 every=60 after=env_bgm [r5+0x400]:f32 [r5+0x404]:f32 [r5+0x408]:f32
"@ | Set-Content private\xenia\sky_begin.txt
```

## Recipes (the controller you drive)

`drive.py` speaks a slightly higher-level language than the emulator's command file. `-Recipe`
feeds a recipe into the run's `.input` file as it goes. The emulator window does **not** need
focus.

Committed recipes:

| file | what it does |
|---|---|
| `tools/xenia/recipes/ac_test.txt` | boot → workshop → AC TEST, standing at m4000 start, one shot |
| `tools/xenia/recipes/walkabout.txt` | after AC TEST: stand, walk, look, jump, more shots |
| `tools/xenia/recipes/ac_test_walk.txt` | `run ac_test.txt` then `run walkabout.txt` |
| `tools/xenia/recipes/ac_test_lockhunt.txt` | after AC TEST: level the camera and sweep it through a full 360° in 8 steps, shooting the lock sight each stop (default start has only allies, so it finds no lockable enemy) |

Reuse `ac_test.txt` and append your own recipe (`run ac_test.txt` then extra steps) rather than
re-encoding the boot sequence. Waits in `ac_test.txt` include shader-compile slack on a cold
cache; do not shrink them.

### Recipe steps (`drive.py`)

Button names: `A B X Y LB RB START BACK LS RS UP DOWN LEFT RIGHT GUIDE`.

| step | effect |
|---|---|
| `press BTN [BTN…] [ms]` | hold 120 ms (or `ms`), release, settle 250 ms |
| `hold BTN [BTN…] SEC` | hold SEC seconds, then release |
| `stick L\|R X Y [SEC]` | thumbstick −1..1; with SEC, recentre after |
| `trigger L\|R V [SEC]` | trigger 0..1; with SEC, release after |
| `down` / `up BTN…` | raw hold / release (no settle) |
| `release` | everything neutral |
| `wait SEC` | sleep |
| `shot PATH` | save the **guest** framebuffer as PNG, wait until the file exists (10 s timeout) |
| `run FILE` | nested recipe, path relative to this file |
| `# …` | comment |

Face-button map in this game (AC TEST / workshop): `A` confirm, `B` back, `START` title
advance, D-pad moves the workshop cursor. In-game: left stick move, right stick camera, `A`
jump, `RB` / triggers fire (confirm on a live pad if a binding is in doubt). `stick L 0 1`
is forward.

A tiny add-on recipe:

```
# tools/xenia/recipes/ac_test_look_up.txt
run ac_test.txt
stick R 0 1 4
shot private/xenia/shots/ac_test_up.png
```

Then: `.\tools\xenia\run.ps1 private\xenia\TASK.txt -Recipe tools\xenia\recipes\ac_test_look_up.txt`

## Reading the log

Each hit is one JSON object:

```json
{"n":2,"hit":1,"ms":122447.558,"tid":6,"pc":"0x827D1EC4","label":"sky_view","lr":"0x8299F804","[r4+0x0]:f32":1.35799539}
```

| field | |
|---|---|
| `n` | global hit index across all probes |
| `hit` | this probe's logged hit (1-based, after `every=` / `max=`) |
| `ms` | host milliseconds since Xenia started |
| `tid` | guest thread |
| `pc` | probed address |
| `label` | your label |
| `lr` | link register at the hit (caller if you probed a function entry) |
| plus one key per READ | the READ text is the key |

`run.ps1 -Recipe` prints counts at the end (`1 env_bgm`, `3 sky_begin`, …). **Zero hits** on a
gameplay probe usually means: the recipe never reached the mission (`env_bgm` also 0), the
address is wrong, or `after=` never opened. Check `env_bgm` first. The sibling `.xenia.log`
has parse errors (`ACVD probes line N: cannot parse`).

A probe log line is sheet evidence: cite the file (`private/xenia/sky_obj.txt` / `.jsonl`),
not a paraphrase.

Printing a probe's float reads four per row (handy for matrices):

```powershell
Get-Content private\xenia\mytask.jsonl | ForEach-Object {
  $j = $_ | ConvertFrom-Json
  $v = @($j.PSObject.Properties | Where-Object { $_.Name -like '`[*' } | ForEach-Object { '{0,10:N3}' -f $_.Value })
  "$($j.label) hit $($j.hit) lr $($j.lr)"
  for ($k = 0; $k -lt $v.Count; $k += 4) { $v[$k..($k + 3)] -join ' ' }
}
```

Generating many reads (16 floats of a matrix) instead of typing them:

```powershell
$m = (0..15 | ForEach-Object { "[r4+0x{0:x}]:f32" -f ($_ * 4) }) -join ' '
"0x827d1ec4 proj max=6 every=30 after=env_bgm $m"
```

## Worked example: where does the 360 put the sky dome?

The question (`docs/open.md`, Map look) could not be answered statically because the matrix
maths is VMX128. The chain that answered it, in five runs of `ac_test.txt`:

1. `acvd-index q const Sky` → the Map_Sky material hook `0x82bd5278`; it calls `0x827d1d90`,
   which rewrites the matrix at device `+0x4a0`.
2. Probe the `bl` inside it (`0x827d1ec4`, reads `[r4+…]`): the matrix is the projection with
   its depth column zeroed (`sky_view.txt`). Its `lr` values showed the real caller was the
   sky **pass** `0x827f3d38`, not the material hook (which had 0 hits: a finding).
3. Probe the pass begin (`r5` = device) and dump the device transform block: camera / view
   (`sky_world.txt`).
4. The sky vertex shader (extract with `cargo run --release -p acvd-formats --example discls --
   --members shader/flver_shader.bnd private\tmp\flvsh`, then `py private\tmp\xenosdis.py
   private\tmp\flvsh\Normal\Flver_Sky.vpo`) names its constants: c0-c3 `VC_MatrixWVP`, c4-c7 `VC_MatrixCamera`, c12+ `VC_aObjMatrix`.
5. Gate a per-draw probe on the pass (`after=sky_begin`) and read the vertex-constant shadow
   (`sky_obj.txt`): object matrix = scale 100 at the camera X/Z.

Pattern worth reusing: gate a hot per-draw probe on a once-per-pass probe so the first few
hits are the draws you care about.

## Comparing a guest shot with the rewrite

A recipe `shot` is the visual reference. Capture the matching rewrite frame and diff the two
instead of eyeballing:

```powershell
cargo run -p acvd-game -- --map m4000 --shot private\shots\check.png --wait 2
cargo run -p acvd-shot -- diff private\xenia\shots\ac_test_start.png private\shots\check.png
```

`acvd-shot` resizes the candidate onto the reference canvas, reports mean error, over-threshold
pixels, luma bias, gradient error and the worst tiles, and writes `<cand>_diff.png`,
`<cand>_side.png` and `<cand>_compare.json`. It exits 1 when the defaults are exceeded, so it can
gate a loop. Useful flags: `--crop X,Y,W,H` (detail), `--ignore X,Y,W,H` (HUD or a known-different
region, repeatable), `--align N` (small camera offset), `--fit letterbox|crop` (aspect mismatch).
`.\tools\shot-check.ps1 -Ref <ref.png> -Shot <out.png> -GameArgs '--map','m4000'` runs both steps.

This is only the appearance check. Numeric values and layouts still come from the disc
(`sheets/*.csv`, `acvd-index`), and menu/HUD ground truth is the DRB + TPF assets rendered by
`acvd-menu`, not a screenshot.

## Known addresses

| address | what | typical reads |
|---|---|---|
| `0x82800128` | type-400 env event: a mission env applied (gate for in-sortie probes) | `r4` |
| `0x827ffa80` | `Env_applyEvent`, every env event by type | `r4` (event record) |
| `0x82c0a160` | per-draw setup (`r3` draw context, `r4` device) | `[[r4+0x384]+0x10]+…` |
| `0x827f3d38` | sky pass begin (`r5` device; `+0x3d0` view block, `+0x4a0` projection) | `[r5+0x400]:f32` |
| `0x82c2bc48` / `0x82c2b4d8` | light / fog shader-constant upload | see `sheets/map_env.csv` |

Device shader-constant shadow: `[[dev+0x384]+0x10]` + `0x780 + 16*n` is vertex constant
`c<n>`; pixel constants start at `+0x1780` (pixel `c19` at `+0x18b0`).

## Generated caves (`tools/xenia-probe`)

`tools/xenia-probe` generates a Xenia patch TOML with up to four leaf caves (FFX, camera, map
warp, and the optional loadout gate) plus a fixed host ring (`0x83803400`, magic `AXPR`; header
`0x70` bytes, data `+0x100`) and launches Xenia directly with `--apply_patches=true`. `run.ps1` also accepts `-ApplyPatches`; it stays off by default.

```powershell
# Static: PE hash, hook words, branch/cave ranges, patch shape. No Xenia.
py -3 tools\xenia-probe\xenia_probe.py --verify --map m3120
# => mission 3120, mission_source map_mission, warp 0; be32 30 / arrays 2;
#    caves ffx 204, camera 224, mapwarp 0

# Live: Mission 3120, 18 FFX records, camera override at o9341's map anchor.
py -3 tools\xenia-probe\xenia_probe.py --map m3120 --look-at o9341 --dump-ffx --annotate `
  --shot private\xenia\shots\m3120_o9341.png --out-dir <run-name>
```

- **Default mission path:** `--map mXXXX` uses `mission/chXXXX.xml` whenever it exists
  (`m3120` -> mission 3120, warp 0). Only maps with no `chXXXX.xml` fall back to the AC Test
  warp (mission 6000 + map-string rewrite). AC Test + `m3120` warp loads the map assets but
  returns to the menu/black screen; it is not a playable mission.
- **FFX:** hook `0x82B88A78`, cave `0x8260E290..0x8260E490`. Records are 24 bytes at
  ring+`0x100`; the header holds `write_index` / `dropped` / `ring_cap` / `ring_recsz`.
- **Camera:** hook `0x82BFB338` (`addi r6,r31,0x20`) in `CamFx_stateFollow`, cave
  `0x8260E490..0x8260E690`. `r31` is the `CamFx` object; its shown eye is `+0x20` and look-at
  `+0x30`, and `0x82BFB344` builds the view matrix from those two vectors. The cave publishes
  the pointer, clears the camera-clip flag at block+`0x60`, and (when `CAM_ENABLE` is set)
  writes the host vectors. `eye_error_m` / `target_error_m` are read back from the same
  `+0x20` / `+0x30` after the write, so they measure the actual view input. The earlier
  `0x828603A0` / `CamCtrl` hook only changed camera inputs and did not move the rendered view;
  that is a dead end.
- **Map warp (fallback only):** hook `0x82B40614` (formatter `0x82B405A0` tail), cave
  `0x8260E700..`; at the hook `r3` is the string object and `[r3+4]` is the destination buffer.
  The cave increments `warp_calls` / `warp_map` / `warp_msb` at header `0x60` / `0x64` /
  `0x68`. AC Test warp counters proved the string rewrite (`map=18`, `msb=4`), but the
  fallback mission itself is not playable.
- **Loadout arm gate:** `--loadout-arm N` patches the arms slot only after the AC Test sortie
  loader signals `0x825DCB68 actest_sortie_load`. The generated cave is gated by ring header
  `+0x5C` (`0x8380345C`, boot word `0`, otherwise unused in non-warp mode); the host waits for
  the probe label, then writes `1` with `WriteProcessMemory`. The cave is 304 B and rewrites
  `0x824556F0` (`r3` = assembly, `r4` = slot, return descriptor at `+8`) to arm id `N`, mapping
  `410 -> 7xxx` and suppressing category-10 slots. Do not combine `--loadout-arm` with
  `--warp`/`--map` fallback warp; the gate deliberately shares the unused warp word and
  `xenia_probe.py` rejects the combination. Verify without launching Xenia:
  `py -3 tools\xenia-probe\xenia_probe.py --verify --loadout-arm 7420`.
- **Animated look-at caveat:** `--look-at o9341` resolves its **MSB placement**
  (`o9341_1000` at `0,0,0`, `m3120_map.msb`). The m3120 bomber is TAE/ANI-animated (behavior
  `811` -> TAE `0x8c1` -> clip 221, speed 0.4): its root route starts ~24 km away at
  `(-24288, 1039, -13029)` and reaches `(-89, 1039, -143)` at frame 900, 1 km up. A shot at the
  static anchor therefore shows the water/cliff view, not the bomber. Track the runtime
  animation root if a model-visible shot is required; do not use MSB `t` for a moving part.
- **Latest live result:** `env_bgm` 121083.576 ms, camera ready 1516 ms, `eye=(20,0.05,0)`,
  `eye_error_m=4.4e-7`, `target_error_m=0`, FFX 18 valid, `o9341` authored 0 FFX.

## Agent checklist

1. `acvd-index q func` / `q callers` the site; decompile or disassemble that range.
2. Pick **one** instruction (entry, a known `bl`, or the store you care about). Confirm it
   with `py private/tmp/x360dis.py LO HI` if the decompiler lies (VMX128).
3. Write `private\xenia\TASK.txt` with a unique label, `max=` / `every=` / `after=env_bgm` as
   needed, and only the reads you will use.
4. Pick or write a recipe. Default: `tools\xenia\recipes\ac_test.txt`. Do not ask the user to
   play unless the recipe cannot reach the state.
5. `.\tools\xenia\run.ps1 private\xenia\TASK.txt -Recipe <recipe>` and wait.
6. Read the `.jsonl`. If the gate probe has hits and yours does not, the instruction did not
   run in that state — that is a finding, not a tool failure.
7. Put 360 addresses + the probe file in the sheet row and `docs/open.md`. Record a failed
   reach as `Xenia re-check pending` plus the recipe path.

## Pitfalls

- **One probe per PC.** Combine reads on that line; do not add a second probe at the same
  address (the second is ignored, with a Xenia log error).
- **`after=` needs the named label in the same file**, declared above or below — it is resolved
  after parse. A typo leaves the probe silent forever.
- **Menus also execute game code.** Ungated draw / camera probes fill the log with workshop
  hits. Use `after=env_bgm` for in-sortie data.
- **Cold shader compile** makes the first AC TEST load slow; the committed waits cover it.
  If `env_bgm` is 0, lengthen the last `wait` in a **copy** of the recipe under `private/xenia/`,
  do not silently shorten the committed one.
- **Guest shots, not host window shots.** Recipe `shot` captures the 360 framebuffer. Host
  screenshots of the Xenia window are not evidence.
- **Do not commit** `private/`, `external/xenia-canary`, or the ISO. Recipes under
  `tools/xenia/recipes/` are the thing to commit when a new unattended path is reusable.
- `run.ps1` always passes `--apply_patches=false`. Do not add game patches to “make a probe
  work”.
