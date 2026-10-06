# Armored Core: Verdict Day — Rust rewrite

A source-only Rust reconstruction of Armored Core: Verdict Day, built spreadsheet-first: sheets
are the source of truth, and every piece of game data code is generated from a sheet row.

Game code is reverse-engineered from the Xbox 360 build only: its executable (`default.xex`,
unpacked to `private/x360/vd/ACV2.pe`) is read statically through Ghidra and the fact index
(`tools/acvd-index`), and runtime questions go to Xenia probes on that same executable. Disc
files are read from the 360 ISO through `acvd_formats::vfs::Disc` (XDVDFS, the BHD5/BDT
archives, the BHF3 script archive and the BND3 load bundles).

## Legal boundary

This repository contains no disc image, executable, or retail data. You dump your own legally
owned disc and every derived file stays in ignored local directories (`armoredcoredumps/`,
`private/`, `external/`, `crates/acvd-data/src/generated/`). Do not upload or redistribute those
files.

## Pipeline

```powershell
tools\setup-tools.ps1                          # pinned Ghidra, ghidra-cli, unluac, Paramdex
cargo run --release -p acvd-sheets -- all      # extract -> preflight -> gen (refuses to gen if preflight has errors)
cargo build -p acvd-data                       # compile the generated struts
tools\analyze-x360.ps1                         # Ghidra project of ACV2.pe with .pdata bounds, then the fact index
cargo run --release -p acvd-index -- audit     # every 360 address cited in sheets/ checked against the index
tools\decompile-lua.ps1                        # Lua 5.0 AI/scene scripts -> private/lua360
tools\xenia\setup.ps1                          # the project's Xenia build (runtime probes: tools\xenia\run.ps1)
```

Every tool reads `armoredcoredumps/Armored Core - Verdict Day (USA)/Armored Core - Verdict Day
(USA).iso` unless given `--disc <360 ISO>` (or `ACVD_DISC` for `acvd-sheets`).

- `extract` reads every PARAMDEF and PARAM on the disc into `private/sheets` (JSON grouped by
  system, CSV per file, schema CSV per type). Each row is re-encoded and compared byte-for-byte with
  the disc, so a decoded row is proof the sheet represents it exactly.
- `preflight` overlaps every sheet, checkmarks each row × column intersection, and writes
  `private/preflight/report.md`. Errors block `gen`; unimplemented work is listed as pending.
- `gen` writes one struct per PARAM type and one strut per row into `acvd-data`.
- Code and runtime evidence in any sheet cites 360 addresses or Xenia probe files; preflight
  rejects PS3 executable addresses and RPCS3 references.

## Sheets (`sheets/`, committed)

| sheet | purpose |
|---|---|
| `target.csv` | target identification: engine, language, runtime, build |
| `systems.csv` | every game system with its data and runtime status |
| `formats.csv` | every file extension on the disc, its format, system and parser status |
| `groups.csv` | def-name prefix → system group for the generated JSON/code |
| `column_overrides.csv` | names, bit widths or dropped columns where the disc def is unnamed or wrong |
| `type_aliases.csv` | PARAM type strings whose def ships under another type string |
| `excluded_files.csv` | PARAM files no shipped def describes, with the evidence |

Every correction row carries its evidence. Column names come from, in order: `column_overrides.csv`,
the disc def's internal names (formats 103/104), then [Paramdex](https://github.com/soulsmods/Paramdex)
ACVD/ACV/ACFA defs, accepted only when field count and every field type match the disc def.
