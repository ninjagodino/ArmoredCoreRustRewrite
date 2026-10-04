# Armored Core: Verdict Day — Rust rewrite

A source-only Rust reconstruction of Armored Core: Verdict Day (PS3, BLUS31194), built
spreadsheet-first: sheets are the source of truth, and every piece of game data code is generated
from a sheet row.

## Legal boundary

This repository contains no disc image, executable, or retail data. You dump your own legally
owned disc and every derived file stays in ignored local directories (`ACVD Unbound/`, `private/`,
`external/`, `crates/acvd-data/src/generated/`). Do not upload or redistribute those files.

## Pipeline

```powershell
tools\setup-tools.ps1                          # pinned Ghidra, ghidra-cli, Ps3GhidraScripts, RPCS3, unluac, Paramdex; decrypts EBOOT
cargo run --release -p acvd-sheets -- all      # extract -> preflight -> gen (refuses to gen if preflight has errors)
cargo build -p acvd-data                       # compile the generated struts
tools\analyze-eboot.ps1                        # headless Ghidra analysis of the decrypted EBOOT
tools\decompile-lua.ps1                        # Lua 5.0 AI/scene scripts -> private/lua
```

`--disc <dump root>` (or `ACVD_DISC`) points at the dump; it defaults to `ACVD Unbound/`.

- `extract` reads every PARAMDEF and PARAM on the disc into `private/sheets` (JSON grouped by
  system, CSV per file, schema CSV per type). Each row is re-encoded and compared byte-for-byte with
  the disc, so a decoded row is proof the sheet represents it exactly.
- `preflight` overlaps every sheet, checkmarks each row × column intersection, and writes
  `private/preflight/report.md`. Errors block `gen`; unimplemented work is listed as pending.
- `gen` writes one struct per PARAM type and one strut per row into `acvd-data`.

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
