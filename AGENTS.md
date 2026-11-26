# ACVD Rust Rewrite — Agent Reference & Tool Manual

Source-only Rust/Bevy 0.19 clean-room reconstruction of *Armored Core: Verdict Day* (Xbox 360, title `4E4D0864`, `default.xex` -> `private/x360/vd/ACV2.pe`, image base `0x82000000`, `file_offset = addr - 0x82000000`). `README.md` covers the legal boundary and the sheet pipeline in full.

- **Open work:** `docs/open.md` — the task list, and the file the next chat reads first.
- **Finished work:** `docs/archive.md` — completed tasks with their findings.
- This file is the single source of truth for invariants, layout, and tooling. Do not copy it into a second rules file; point at it instead.

## Working Style

- **One task per chat.** Pick one item from `docs/open.md`, finish it, then before the chat ends update that file with the findings (status, 360 addresses, file layouts, dead ends) and move the finished task to `docs/archive.md` with a one-line summary. The next chat starts from `docs/open.md`, not from chat history.
- **Commit and push at the end of every task, but only if the change does not risk breaking previously working systems**: after updating `docs/open.md`/`docs/archive.md` with the build and tests passing, stage only the files this task touched (other chats may be editing in parallel), one commit describing the task, then `git pull --rebase` and `git push`. If tests, audit, or a manual smoke check expose risk to existing systems, leave the work uncommitted and record the risk in `docs/open.md`. The GitHub repo is public: never `git add -f` or commit anything under the gitignored disc-derived paths (`armoredcoredumps/`, `ACVD Unbound/`, `private/`, `external/`, `crates/acvd-data/src/generated/`, `settings.json`).
- Spreadsheet-first: game facts live in `sheets/*.csv`, each row with its evidence (360 address, disc file, or breakpoint). Code reads sheets or generated data; don't hard-code constants without a sheet row or a comment citing the 360 address.
- Static RE uses the Xbox 360 build (see `.cursor/rules/decompile-view.mdc`). Runtime questions on the 360 ("does this path fire?") go to the project's Xenia probes (`tools/xenia/run.ps1`, full guide in `docs/xenia-probes.md`); runtime tracing on the rewrite goes to Rust scene probes first (see `.cursor/rules/rust-probing.mdc`: `--shot` + `SHOT` telemetry and headless Bevy `World` / disc-asset tests). The PS3 executable is never read or cited.
- **Never place effects from screenshots.** Light shafts, beams, lens flares, and any world-space effect must be traced from the 360 build, not back-projected from a screenshot pixel: one view leaves the depth unknown, so the object lands at the wrong spot from every other angle. Trace the count and positions with `acvd-index q` xrefs first, then `py tools\x360dis\x360dis.py --func <addr>` (`tools/x360dis/ppc_vmx128.py` decodes Xenon VMX128 that Ghidra cannot), then a Xenia probe only for runtime values. Screenshots are the final appearance check (`tools\shot-check.ps1` / `acvd-shot diff`), never the source of a placement.
- Before any `cargo` command, set `CARGO_TARGET_DIR` (see `.cursor/rules/cargo-target.mdc`).

## Hard Invariants (NEVER Violate)

1. **Xbox 360 ONLY**: Never read, run, or cite PS3 / RPCS3 / `EBOOT` / `TOC` / `01.02` / `FUN_00..` / `FUN_01..`. `acvd-sheets preflight` & `acvd-index audit` fail on PS3 citations. `ACVD Unbound/` (old PS3 dump) is no longer read.
2. **Public Repo Legal Boundary**: NEVER `git add -f` or commit anything in `armoredcoredumps/`, `ACVD Unbound/`, `private/`, `external/`, `crates/acvd-data/src/generated/`, or `settings.json`.
3. **Spreadsheet-First**: Never edit `crates/acvd-data/src/generated/` directly. Add verified game facts + 360 evidence to `sheets/*.csv` and run `acvd-sheets all`. Don't hard-code game constants without a sheet row or 360 address comment.
4. **Cargo Target Dir**: Before ANY `cargo` command in a shell session, run:
   `$env:CARGO_TARGET_DIR = "$PWD\target"; $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"`
   (Workspace `[profile.dev]` already has `opt-level = 1` + optimized `acvd-formats`/`acvd-render`, running ~120 fps in debug.)
5. **Token Hygiene**: NEVER read `docs/open.md`, `docs/archive.md`, `sheets/*.csv`, or `.jsonl` logs in full. Always grep/filter with `Select-String`, `acvd-index q`, or PowerShell slices. Use `--message-format=short` on `cargo check` and filter `cargo test -p <crate> -- <filter>`.
6. **Coordinates & Endianness**: All 360 disc formats are **big-endian**. FLVER is left-handed CW; `acvd-render` mirrors X (`[-x, y, z]`) into Bevy right-handed CCW. FLVER semantic 6 (`TangentS8`) stores bitangent `+dP/dv` with `w = ±1`; all normal maps use `flip_normal_map_y: false`.
7. **UI Sliders**: Any slider/gauge/stepper must support hover-to-scroll (`garage::wheel_step_direction` / `UiAction::wheel_adjust()`) and set `state.wheel_consumed = true` so the 3D garage camera never zooms.
8. **Task End**: Update `docs/open.md` + `docs/archive.md` (and `sheets/systems.csv` if system status changed), run tests + `acvd-index audit`, stage ONLY touched files, commit, `git pull --rebase`, `git push`.
9. **Never Delete Through Junctions/Reparse Points**: Before ANY destructive removal (`git worktree remove`, `Remove-Item -Recurse`, `rm -rf`, cleaning a shared path), enumerate reparse points first and stop if any resolve outside the target: `Get-ChildItem -LiteralPath <path> -Recurse -Force -Attributes ReparsePoint`. A worktree must never contain junctions into `armoredcoredumps/`, `private/`, `external/`, or `crates/acvd-data/src/generated/`; if one exists, unlink only the junction (e.g. `cmd /c rmdir <link>`) and verify the real target is untouched. Deleting through a junction already destroyed the 360 ISO once and was unrecoverable — check first, every time.

## Workspace Map

- `crates/acvd-formats`: Big-endian disc readers (`xdvdfs`, `bhd5`, `bnd3`, `dcx` DFLT+EDGE, `vfs::Disc` w/ `path|entry` + 192MiB LRU, `flver`, `tpf`+Xenos untile/DXN/CTX1/HT-BC3, `param`/`paramdef`/`layout`, `acv_parts`, `accolor`, `ani`, `tae`, `dbp`, `drb`, `env`, `ffx`, `fmg`, `ccm`/`fontdef`, `fsb`/`fev`/`xma`, `hmd`, `jcon`, `mlb`, `msb`, `nav` HNAV+HTR).
- `crates/acvd-data`: Generated structs/statics from `sheets/` + disc (`src/generated/` gitignored). O(1) `OnceLock` lookup helpers (`ac_part`, `part_field`, `ac_state`, `tuning`, `model`, `textures_named`, `assets_in`).
- `crates/acvd-render`: Disc -> Bevy meshes/textures (`Packs`, `FlverCache`, `menu`, `text` bicubic CCM, `ui_kit` `AcvdUiKit`, `app` orbit/`--shot`).
- `crates/acvd-viewer`: FLVER model browser (`acvd-viewer`, plus `acvd-menu` for DRB menu/HUD layouts).
- `crates/acvd-game`: Full runtime (`garage`, `garage_assemble`, `assemble`, `paint`, `decal`, `pose`, `boost`, `hanger`, `bay`, `control`, `collision`, `fcs`, `scan`, `weapons`, `damage`, `targets`, `ai`, `map`, `env`, `blur`, `lens_flare`, `debris`, `sfx`, `sound`, `hud`, `pause`, `settings`).
- `tools/acvd-sheets`: `extract` -> `preflight` -> `gen` (`all` runs all 3; `dump <asset>` extracts to `private/dump/`).
- `tools/acvd-index`: Static fact index over `ACV2.pe` (`build` -> `private/index/*.csv`, `q ...`, `audit` -> `private/index/audit.md`).
- `tools/acvd-shot`: Numeric screenshot comparison (`diff <ref.png> <cand.png>`): resizes onto the reference canvas, optional translation search, per-tile tone/edge metrics, writes `_diff.png` / `_side.png` / `_compare.json`, exits 1 on threshold miss.
- `tools/x360dis`: Xenon/VMX128 disassembler straight from the disc image (`py tools\x360dis\x360dis.py --func 0xADDR [--refs|--trace|--stats]`); `ppc_vmx128.py` + `vmx128.txt` decode the VMX128 encodings Ghidra drops.
- `tools/ghidra`: Ghidra scripts (`X360Pdata.java`, `DecompileRefs.java`, `X360Vmx128.java` — annotates VMX128 words with correct text + constant-pool xrefs, `--fix` restores 4-byte alignment in desynced functions).
- `tools/xenia`: The project's Xenia Canary fork (`acvd-probe.patch`, built into `external/xenia-canary`): `setup.ps1` builds it, `run.ps1` boots the 360 disc and logs probe hits.
- `sheets/`: Committed source-of-truth CSVs (`systems.csv` = per-system status).
- `private/`: Gitignored: disc-derived data, Ghidra projects, `x360/vd/ACV2.pe`, `lua360/` (decompiled scripts), `tmp/` (RE helper scripts, debug shots).
- `armoredcoredumps/`: The disc images (gitignored); every tool reads the 360 ISO in it.

## Commands

```powershell
$env:CARGO_TARGET_DIR = "$PWD\target"; $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo run --release -p acvd-sheets -- all          # after editing sheets
cargo run -p acvd-game -- [design id] [--shot private\shots\x.png --wait 2] [--water <y>] [--hold w,shift]
cargo run -p acvd-viewer -- [model name]
cargo run -p acvd-viewer --bin acvd-menu -- [layout] [dialog] # DRB menu/HUD layouts
cargo run -p acvd-shot -- diff private\xenia\shots\ac_test_start.png private\shots\x.png
.\tools\shot-check.ps1 -Ref private\xenia\shots\ac_test_start.png -Shot private\shots\x.png -GameArgs '--map','m4000'
.\demo.bat                               # manual-test menu: rebuilds, then launches a scenario
```

`acvd-game` flags and controls are documented at the top of `crates/acvd-game/src/main.rs`.
Verify visual changes with `--shot`; compare it against the 360 guest shot with `acvd-shot diff`
(or `tools\shot-check.ps1`, which runs both) instead of eyeballing. Screenshots are the final
appearance check: numeric values still come from `sheets/*.csv` / `acvd-index`, and UI/menu ground
truth is the DRB + TPF assets rendered by `acvd-menu`.

## Tool Playbook (ALWAYS Run Existing Tools — Never Guess Offsets)

### 1. Static RE: Fact Index FIRST (instant, tiny output)
```powershell
$x = "target\release\acvd-index.exe"   # build: cargo build --release -p acvd-index
& $x q func 0x82821af0                 # function bounds containing ADDR
& $x q callers 0x82821af0              # direct calls, vtable slots, const refs
& $x q callees 0x82821af0              # outgoing calls from FUNC
& $x q field 0x284 [FUNC]              # struct reads/writes at +0x284
& $x q const 0x82004408 [FUNC]         # address or ASCII string refs (q const kosi)
& $x q imm 0x38 [FUNC]                 # immediate li/cmpwi/cmplwi
& $x q slot 0x820958d0 0x4c            # read 360 word at TABLE+OFF
& $x q vcall 0xd0 [FUNC]               # virtual calls to slot +0xd0
& $x q switch FUNC|SITE                # jump tables & cases
& $x q name EyeDist                    # search PARAMDEF / .dbp / Lua param names
& $x audit                             # REQUIRED after editing sheets/*.csv
```

Always pass `--limit 40` on broad queries to save context.

### 2. Decompile / Disassemble (sub-second via resident ghidra-cli)
```powershell
$g = "$PWD\external\ghidra-cli\ghidra.exe"
& $g status
# If stopped, NEVER run `& $g start` inline (it hangs stdout). Always detach:
Start-Process $g start -WindowStyle Hidden -RedirectStandardOutput private\tmp\bridge.out -RedirectStandardError private\tmp\bridge.err
& $g decompile 0x82bf7878              # or by renamed symbol
& $g x-ref to 0x8285cf68
& $g rename FUN_82bf7878 CamFx_sideMoving
# Stop bridge before any headless Ghidra script: `& $g stop`
```

- **VMX128 / "bad instruction data"**: Ghidra fails on VMX128. Immediately use `py tools\x360dis\x360dis.py --func 0xADDR [--refs|--trace|--stats]` (decoder: `tools\x360dis\ppc_vmx128.py`, table `vmx128.txt`); `--range LO HI` for arbitrary spans. `vmx128.txt` is generated from Xenia's `ppc-instructions.xml` by `tools\x360dis\gen_vmx128_table.py` and reproduces its LookupOpcode group order (primary 4/5/6 mix Altivec, VMX128 and VX128-only forms). Ghidra itself cannot be taught VMX128 with a language: SLEIGH token fields are one contiguous bit range (`slghparse.y` `fielddef`), while VMX128 scatters v0-v127 across the Altivec XO field. Use `& $g script run tools\ghidra\X360Vmx128.java -- tools\x360dis\vmx128.txt <func...> [--fix]` to write the correct text as comments and create READ/WRITE refs for `lv*128`/`st*128` constant pools (`--dump-refs` shows the `lis/addi` chain; refs are only made when the chain is lis-derived and Ghidra's scalar disassembly is 4-byte aligned).
- **Xenos shaders** (`.vpo`/`.fpo`): Extract with `cargo run --release -q -p acvd-formats --example discls -- --members shader/flver_shader.bnd private\tmp\flvsh`, disassemble with `py private\tmp\xenosdis.py <file>`.
- **Decompiled Lua 5.0**: Already in `private\lua360\` (Select-String there; rebuild via `.\tools\decompile-lua.ps1`).

### 3. Runtime Truth on 360: Unattended Xenia Probes

Use ONLY when static RE cannot determine runtime values, units, call frequency, or VMX matrices.

Write the probe list to `private\xenia\TASK.txt` (1 probe per PC, gate sortie probes on `env_bgm`):

```
0x82800128 env_bgm max=1 r4
0x82822ea0 my_probe max=40 every=2 after=env_bgm r3 f1 [r3+0x88]:f32 [[r3+0x4]+0x1bc]:u32
```

Types: `u8 u16 u32 s8 s16 s32 f32 f64` (big-endian; NO VMX register reads). PPC ABI: r3=this/arg0, r4..r10=args, lr=caller.

Run unattended (takes ~150–200 s; never run two Xenia instances at once):

```powershell
.\tools\xenia\run.ps1 private\xenia\TASK.txt -Recipe tools\xenia\recipes\ac_test.txt
```

Recipes in `tools\xenia\recipes\`: `ac_test.txt`, `ac_test_walk.txt`, `boost_vfx.txt`, `boost_glide.txt`, `en_drain.txt`, `quick_boost.txt`, `wall_kick.txt`. For custom actions, write a small recipe starting with `run ac_test.txt`.

Filter `private\xenia\TASK.jsonl` with PowerShell (`ConvertFrom-Json | Select-Object -First 20`), never dump huge logs raw. Cite the probe file in `sheets/*.csv`.

### 4. Rust Rewrite Probing & Verification

Headless tests (fastest, works even while `acvd-game.exe` is open):

```
cargo test -p acvd-game -- <test_name> --nocapture
```

Live scene telemetry + shot:

```
cargo run -p acvd-game -- [design_id] --shot private\shots\x.png --wait 2 [--hold w,shift] [--hold-delay 1.0] [--garage] [--garage-screen <top|assemble|paint|decals|emblem|view|options>] [--view-action <walk|boost|r_arm|r_bay|shoulder>] [--map m4000] [--test actest2] [--burst 4]
```

Filter `SHOT` / `PERF` lines from stderr to inspect exact live entity states/timers/FPS.

Sheets rebuild (after editing `sheets/*.csv`):

```
cargo run --release -p acvd-sheets -- all
```
