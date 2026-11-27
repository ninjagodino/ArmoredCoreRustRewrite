# Manual test launcher: rebuilds (incrementally) and starts a scenario, so every launch runs the
# current code. `.\demo.ps1` shows a menu; `.\demo.ps1 <acvd-game args>` skips it, e.g.
# `.\demo.ps1 --map m3200 --water 5`. `-Release` uses the optimized build; `-Viewer` the model
# viewer. Dev profile already disables Bevy debug-assertions so AC TEST stays ~120 fps in debug.
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

function EnsureMovie([string]$name) {
    if (Test-Path "private\movies\$name\manifest.json") { return }
    Write-Host "== caching opening movie '$name' (acvd-movies cache)" -ForegroundColor Cyan
    cargo run --release -p acvd-movies -- cache $name
    if ($LASTEXITCODE) {
        Write-Host "movie cache failed; the boot flow will skip the opening" -ForegroundColor Yellow
    }
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
  1  Opening: press-start splash + prologue movie, then 3D Garage
  2  Pilot an AC directly in AC TEST (m4000, default design; F1 = Garage)
  3  Pilot: pick design id and map
  4  Pilot on a flat plane with test water at y=2
  5  Model viewer (FLVER browser)
  6  Regenerate sheets (after editing sheets/*.csv)
  7  Custom acvd-game arguments
  q  Quit

 In Opening: Enter/Space/Start at PRESS START; Enter/Space/Esc skips the movie after 0.6 s.
 In Garage: W/S or Up/Down navigate, Enter/Click select, Esc back, Right-drag orbit, V view mode.
 In Sortie: WASD move, Q/E turn, Shift boost, Space jump, F/C fire, G shoulder, Z scan, B recon,
 F1 return to Garage, F9 mute BGM. Close the window to come back here.
"@
    switch ((Read-Host ' choice').Trim()) {
        '1' { EnsureMovie 'prologue'; Launch 'acvd-game' @('--boot', '--garage') }
        '2' { Launch 'acvd-game' @('--no-boot') }
        '3' {
            $a = @('--no-boot')
            $id = (Read-Host ' design id (blank = first)').Trim(); if ($id) { $a += $id }
            $map = (Read-Host ' map folder, e.g. m3200 (blank = m4000 AC test)').Trim(); if ($map) { $a += '--map', $map }
            Launch 'acvd-game' $a
        }
        '4' { Launch 'acvd-game' @('--plane', '--water', '2') }
        '5' {
            $m = (Read-Host ' model name (blank = first)').Trim()
            Launch 'acvd-viewer' $(if ($m) { @($m) } else { @() })
        }
        '6' { try { Sheets } catch { Write-Host $_ -ForegroundColor Red } }
        '7' { Launch 'acvd-game' ((Read-Host ' args').Trim() -split '\s+' | Where-Object { $_ }) }
        'q' { return }
        default { Write-Host ' ?' }
    }
}
