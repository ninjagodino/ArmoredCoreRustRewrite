# Runs the rewrite with --shot, then compares the PNG against a 360 guest shot.
# One command for the visual-parity loop:
#   .\tools\shot-check.ps1 -Ref private\xenia\shots\ac_test_start.png -Shot private\shots\check.png `
#       -GameArgs '--map','m4000','--wait','2'
# GameArgs are passed after the acvd-game `--`. Use -Release for a release build, -NoFail to keep
# the comparison's exit code at 0.
param(
    [Parameter(Mandatory)][string]$Ref,
    [Parameter(Mandatory)][string]$Shot,
    [string[]]$GameArgs = @(),
    [string]$Disc = "",
    [switch]$Release,
    [switch]$NoFail
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$env:CARGO_TARGET_DIR = Join-Path $root "target"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

$common = @("run", "-q")
if ($Release) { $common += "--release" }

$game = $common + @("-p", "acvd-game", "--", "--shot", $Shot)
if ($Disc) { $game += @("--disc", $Disc) }
$game += $GameArgs

Push-Location $root
$code = 1
try {
    & cargo @game
    if ($LASTEXITCODE -ne 0) { throw "acvd-game exited $LASTEXITCODE" }

    $diff = $common + @("-p", "acvd-shot", "--", "diff", $Ref, $Shot)
    if ($NoFail) { $diff += "--no-fail" }
    & cargo @diff
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $code
