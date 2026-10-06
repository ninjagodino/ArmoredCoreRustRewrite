# Imports the unpacked 360 executable (private/x360/vd/ACV2.pe) into the writable Ghidra project
# ghidra-cli uses (private/ghidra360cli, project ACV2), applies the .pdata function bounds, and
# builds the static fact index (private/index). Stop the ghidra-cli bridge first: it locks the project.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$ghidra = Join-Path $root 'external\ghidra_12.1.4_PUBLIC'
$proj = Join-Path $root 'private\ghidra360cli'
$pe = Join-Path $root 'private\x360\vd\ACV2.pe'
New-Item -ItemType Directory -Force $proj | Out-Null
& (Join-Path $ghidra 'support\analyzeHeadless.bat') $proj ACV2 `
    -import $pe -overwrite `
    -processor 'PowerPC:BE:64:A2ALT-32addr' -cspec default `
    -scriptPath (Join-Path $root 'tools\ghidra') -postScript X360Pdata.java `
    -log (Join-Path $proj 'analyze.log') -scriptlog (Join-Path $proj 'scripts.log') -max-cpu 8
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
cargo run --release -p acvd-index -- --root $root build
