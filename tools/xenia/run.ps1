# Boots the 360 Verdict Day disc in the project's Xenia with a probe list and writes the hits as
# JSON lines. Probe syntax: tools/xenia/probes.example.txt. With -Seconds the emulator is closed
# after that long and a per-probe hit count is printed; without it, close the window yourself.
#   .\tools\xenia\run.ps1 private\xenia\camera.txt -Seconds 90
param(
    [Parameter(Mandatory)][string]$Probes,
    [string]$Log,
    [int]$Seconds = 0,
    [string]$Iso = "armoredcoredumps\Armored Core - Verdict Day (USA)\Armored Core - Verdict Day (USA).iso",
    [string[]]$Extra = @()
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
$bin = "$root\external\xenia-canary\build\bin\Windows\Release"
if (-not (Test-Path "$bin\xenia_canary.exe")) { throw "run tools\xenia\setup.ps1 first" }
$Probes = (Resolve-Path $Probes).Path
if (-not $Log) { $Log = [IO.Path]::ChangeExtension($Probes, ".jsonl") }
$Log = [IO.Path]::GetFullPath($Log)
$Iso = (Resolve-Path (Join-Path $root $Iso)).Path
Remove-Item $Log -ErrorAction SilentlyContinue

# protect_zero=false: the game crashes leaving any mission without it (Xenia compatibility issue
# #278). readback_resolve=full: AC parts render black otherwise. Neither touches guest code; no
# game patches are loaded.
$xargs = @(
    "--acvd_probes=`"$Probes`"", "--acvd_probe_log=`"$Log`"",
    "--protect_zero=false", "--readback_resolve=full", "--apply_patches=false",
    "--log_file=`"$([IO.Path]::ChangeExtension($Log, '.xenia.log'))`""
) + $Extra + @("`"$Iso`"")
$p = Start-Process -FilePath "$bin\xenia_canary.exe" -WorkingDirectory $bin -ArgumentList $xargs -PassThru
"xenia pid $($p.Id), hits -> $Log"
if ($Seconds -gt 0) {
    if (-not $p.WaitForExit($Seconds * 1000)) { Stop-Process -Id $p.Id -Force }
    if (Test-Path $Log) {
        Get-Content $Log | ForEach-Object { ($_ | ConvertFrom-Json).label } |
            Group-Object | ForEach-Object { "{0,6}  {1}" -f $_.Count, $_.Name }
    } else {
        "no hits"
    }
}
