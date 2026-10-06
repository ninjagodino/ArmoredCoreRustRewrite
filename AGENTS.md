# Armored Core: Verdict Day — Rust rewrite

Source-only Rust/Bevy reconstruction of ACVD (Xbox 360, title 4E4D0864), reverse-engineered from
the owned 360 disc image. `README.md` covers the legal boundary and the sheet pipeline in full.

## Working style

- **One task per chat.** Pick one item from `docs/status.md`, finish it, then update that file
  (status, findings, 360 addresses, open questions) before the chat ends. The next chat starts
  from that file, not from chat history.
- **Commit and push at the end of every task**, after updating `docs/status.md` and with the
  build and tests passing: stage only the files this task touched (other chats may be editing
  in parallel), one commit describing the task, then `git pull --rebase` and `git push`. The
  GitHub repo is public: never `git add -f` or commit anything under the gitignored disc-derived
  paths (`armoredcoredumps/`, `ACVD Unbound/`, `private/`, `external/`, `crates/acvd-data/src/generated/`).
- Spreadsheet-first: game facts live in `sheets/*.csv`, each row with its evidence (360 address,
  disc file, or breakpoint). Code reads sheets or generated data; don't hard-code constants
  without a sheet row or a comment citing the 360 address.
- Static RE uses the Xbox 360 build (see `.cursor/rules/decompile-view.mdc`). Runtime questions
 ("does this path fire?") go to the project's Xenia probes on that same 360 build
 (`tools/xenia/run.ps1`), not long static hunts. The PS3 executable is never read or cited.
- Before any `cargo` command, set `CARGO_TARGET_DIR` (see `.cursor/rules/cargo-target.mdc`).

## Layout

| path | what |
|---|---|
| `crates/acvd-formats` | disc format readers: DCX, BND3, FLVER, TPF (+ Xenos untile), PARAM/PARAMDEF, `.ani`, `.dbp`, `acvparts.bin`, FSB/XMA, the 360 ISO (XDVDFS, BHD5/BDT, BHF3), VFS (`vfs::Disc`, `path|entry` asset paths) |
| `crates/acvd-data` | generated structs/rows from the sheets (`src/generated/` is never committed) |
| `crates/acvd-render` | disc → Bevy meshes/textures, orbit camera, `--shot` screenshot mode |
| `crates/acvd-viewer` | FLVER model browser |
| `crates/acvd-game` | runtime: AC assembly (`assemble`), posing/motion (`pose`), piloting + follow camera (`control`), map `.hmd` collision (`collision`), DRB lock-sight HUD (`hud`) |
| `tools/acvd-sheets` | `extract` → `preflight` → `gen` (`all` runs every step); preflight errors block gen |
| `tools/acvd-index` | static fact index of `ACV2.pe` (`build` → `private/index`, `q ...` lookups, `audit` of sheet citations) |
| `tools/ghidra` | Ghidra scripts (`X360Pdata.java`, `DecompileRefs.java`, ...) |
| `tools/xenia` | the project's Xenia Canary fork (`acvd-probe.patch`, built into `external/xenia-canary`): `setup.ps1` builds it, `run.ps1` boots the 360 disc and logs probe hits |
| `sheets/` | committed source-of-truth CSVs (`systems.csv` = per-system status) |
| `private/` | gitignored: disc-derived data, Ghidra projects, `x360/vd/ACV2.pe`, `lua/` (decompiled scripts), `tmp/` (RE helper scripts, debug shots) |
| `armoredcoredumps/` | the disc images (gitignored); every tool reads the 360 ISO in it. `ACVD Unbound/` (old PS3 dump, gitignored) is no longer read |

## Commands

```powershell
$env:CARGO_TARGET_DIR = "$PWD\target"; $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo run --release -p acvd-sheets -- all          # after editing sheets
cargo run -p acvd-game -- [design id] [--shot private\shots\x.png --wait 2] [--water <y>] [--hold w,shift]
cargo run -p acvd-viewer -- [model name]
cargo run -p acvd-viewer --bin acvd-menu -- [layout] [dialog] # DRB menu/HUD layouts
.\demo.bat                               # manual-test menu: rebuilds, then launches a scenario
```

`acvd-game` flags and controls are documented at the top of `crates/acvd-game/src/main.rs`.
Verify visual changes with `--shot` and look at the PNG.
