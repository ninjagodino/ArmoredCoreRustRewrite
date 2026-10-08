# Boots the 360 Verdict Day disc in the project's Xenia with a probe list and writes the hits as
# JSON lines. Probe syntax: tools/xenia/probes.example.txt. With -Seconds the emulator is closed
# after that long and a per-probe hit count is printed; without it, close the window yourself.
#   .\tools\xenia\run.ps1 private\xenia\camera.txt -Seconds 90
# -Drive puts the scripted controller (tools/xenia/drive.py) on -Port in place of a real pad;
# its command file is the log path with extension .input. -Recipe plays a drive.py recipe once
# the emulator is up, then closes it (implies -Drive).
#   .\tools\xenia\run.ps1 private\xenia\envlook.txt -Recipe tools\xenia\recipes\ac_test.txt
param(
    [Parameter(Mandatory)][string]$Probes,
    [string]$Log,
    [int]$Seconds = 0,
    [switch]$Drive,
    [string]$Recipe,
    [int]$Port = 1, # the signed-in profile (logged_profile_slot_1_xuid) is in slot 1
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
if ($Recipe) { $Recipe = (Resolve-Path $Recipe).Path; $Drive = $true }
$input_file = [IO.Path]::ChangeExtension($Log, ".input")
Remove-Item $Log -ErrorAction SilentlyContinue

# protect_zero=false: the game crashes leaving any mission without it (Xenia compatibility issue
# #278). readback_resolve=full: AC parts render black otherwise. Neither touches guest code; no
# game patches are loaded.
$xargs = @(
    "--acvd_probes=`"$Probes`"", "--acvd_probe_log=`"$Log`"",
    "--protect_zero=false", "--readback_resolve=full", "--apply_patches=false",
    "--log_file=`"$([IO.Path]::ChangeExtension($Log, '.xenia.log'))`""
)
if ($Drive) { $xargs += "--acvd_input=`"$input_file`"", "--acvd_input_port=$Port" }
$xargs += $Extra + @("`"$Iso`"")
$p = Start-Process -FilePath "$bin\xenia_canary.exe" -WorkingDirectory $bin -ArgumentList $xargs -PassThru
"xenia pid $($p.Id), hits -> $Log"
if ($Drive) { "controller -> py tools\xenia\drive.py `"$input_file`" STEP..." }
if ($Recipe) {
    py "$PSScriptRoot\drive.py" $input_file $Recipe
    if ($LASTEXITCODE) { "recipe failed; emulator left open" ; return }
    Stop-Process -Id $p.Id -Force
} elseif ($Seconds -gt 0) {
    if (-not $p.WaitForExit($Seconds * 1000)) { Stop-Process -Id $p.Id -Force }
}
if ($Recipe -or $Seconds -gt 0) {
    if (Test-Path $Log) {
        Get-Content $Log | ForEach-Object { ($_ | ConvertFrom-Json).label } |
            Group-Object | ForEach-Object { "{0,6}  {1}" -f $_.Count, $_.Name }
    } else {
        "no hits"
    }
}
