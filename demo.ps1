# Manual test launcher: rebuilds (incrementally) and starts a scenario, so every launch runs the
# current code. `.\demo.ps1` shows a menu; `.\demo.ps1 <acvd-game args>` skips it, e.g.
# `.\demo.ps1 --map m3200 --water 5`. `-Release` uses the optimized build, `-Viewer` the model viewer.
param([switch]$Release, [switch]$Viewer, [Parameter(ValueFromRemainingArguments)][string[]]$Rest)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$env:CARGO_TARGET_DIR = "$PSScriptRoot\target"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$profileDir = if ($Release) { 'release' } else { 'debug' }
$cargoFlags = if ($Release) { @('--release') } else { @() }

function Sheets {
    Write-Host "== regenerating sheets (acvd-sheets all)" -ForegroundColor Cyan
    cargo run --release -p acvd-sheets -- all
    if ($LASTEXITCODE) { throw "acvd-sheets failed (preflight errors block gen)" }
}

function Launch([string]$crate, [string[]]$launchArgs) {
    Write-Host "== building $crate ($profileDir)" -ForegroundColor Cyan
    cargo build @cargoFlags -p $crate
    if ($LASTEXITCODE) { Write-Host "build failed; fix the errors above" -ForegroundColor Red; return }
    Write-Host "== $crate $($launchArgs -join ' ')" -ForegroundColor Cyan
    & "$env:CARGO_TARGET_DIR\$profileDir\$crate.exe" @launchArgs
}

if (-not (Test-Path "crates\acvd-data\src\generated")) { Sheets }

if ($Viewer) { Launch 'acvd-viewer' $Rest; return }
if ($Rest) { Launch 'acvd-game' $Rest; return }

while ($true) {
    Write-Host @"

 ACVD rewrite demo ($profileDir build)
  1  Pilot an AC in the AC test map (m4000, default design)
  2  Pilot: pick design id and map
  3  Pilot on a flat plane with test water at y=2
  4  Model viewer (FLVER browser)
  5  Regenerate sheets (after editing sheets/*.csv)
  6  Custom acvd-game arguments
  q  Quit

 In game: WASD move, Q/E turn, Up/Down pitch, Shift boost, Space jump, F/C fire right/left,
 M mouselook, P clip browser, Left/Right previous/next design, R reframe.
 Close the window to come back here.
"@
    switch ((Read-Host ' choice').Trim()) {
        '1' { Launch 'acvd-game' @() }
        '2' {
            $a = @()
            $id = (Read-Host ' design id (blank = first)').Trim(); if ($id) { $a += $id }
            $map = (Read-Host ' map folder, e.g. m3200 (blank = m4000 AC test)').Trim(); if ($map) { $a += '--map', $map }
            Launch 'acvd-game' $a
        }
        '3' { Launch 'acvd-game' @('--plane', '--water', '2') }
        '4' {
            $m = (Read-Host ' model name (blank = first)').Trim()
            Launch 'acvd-viewer' $(if ($m) { @($m) } else { @() })
        }
        '5' { try { Sheets } catch { Write-Host $_ -ForegroundColor Red } }
        '6' { Launch 'acvd-game' ((Read-Host ' args').Trim() -split '\s+' | Where-Object { $_ }) }
        'q' { return }
        default { Write-Host ' ?' }
    }
}
