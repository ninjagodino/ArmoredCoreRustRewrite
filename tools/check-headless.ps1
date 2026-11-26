# Fails if a headless crate's normal dependency tree pulls in a GPU/window/audio/image crate.
# Phase 0 gates acvd-sim; Phase 1 adds the GPU-free dedicated server.
#   .\tools\check-headless.ps1
#   .\tools\check-headless.ps1 -Packages acvd-sim,acvd-server
param(
    [string[]]$Packages = @("acvd-sim", "acvd-server"),
    [string[]]$ExtraForbidden = @(),
    [switch]$Dev
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$env:CARGO_TARGET_DIR = Join-Path $root "target"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

$forbidden = @("bevy", "wgpu", "winit", "cpal", "alsa", "rodio", "image", "acvd-render") + $ExtraForbidden
$pattern = ($forbidden | ForEach-Object { [regex]::Escape($_) }) -join "|"
$edges = if ($Dev) { "all" } else { "normal" }

Push-Location $root
try {
    $bad = 0
    foreach ($pkg in $Packages) {
        $tree = & cargo tree -p $pkg --edges $edges -q
        if ($LASTEXITCODE -ne 0) { throw "cargo tree -p $pkg exited $LASTEXITCODE" }
        $hits = @($tree | Select-String -Pattern $pattern)
        if ($hits.Count -gt 0) {
            Write-Host "FAIL $pkg depends on a forbidden crate:"
            $hits | ForEach-Object { Write-Host ("  " + $_.Line.Trim()) }
            $bad++
        } else {
            Write-Host "PASS $pkg ($edges): no $($forbidden -join ", ")"
        }
    }
} finally {
    Pop-Location
}
exit ([int]($bad -gt 0))
