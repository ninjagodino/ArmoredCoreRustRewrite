# Decompiles every Lua 5.0 chunk (.lc) on the owned disc with unluac: the PS3 dump into
# private/lua, or with -Disc <360 ISO> the `script/` chunks of script.bhd into private/lua360
# (same little-endian Lua 5.0 header; the 360 AI scripts keep the LogAI calls the PS3 build strips).
param([string]$Disc = (Join-Path (Split-Path -Parent $PSScriptRoot) 'ACVD Unbound'))
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$jar = Join-Path $root 'external\unluac.jar'
$java = if ($env:JAVA_HOME) { Join-Path $env:JAVA_HOME 'bin\java.exe' } else { 'java' }

if ($Disc -like '*.iso') {
    $usrdir = Join-Path $root 'private\tmp\lc360'
    $out = Join-Path $root 'private\lua360'
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    cargo run --release -q -p acvd-formats --example discls -- --disc $Disc --extract script/ $usrdir
    if ($LASTEXITCODE -ne 0) { throw 'extracting script/ from the ISO failed' }
} else {
    $usrdir = Join-Path $Disc 'PS3_GAME\USRDIR'
    $out = Join-Path $root 'private\lua'
}

$files = Get-ChildItem $usrdir -Recurse -Filter *.lc
$failed = @()
foreach ($f in $files) {
    $rel = $f.FullName.Substring($usrdir.Length + 1)
    $dest = Join-Path $out ([IO.Path]::ChangeExtension($rel, '.lua'))
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    & $java -jar $jar --output $dest $f.FullName 2>$null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $dest)) { $failed += $rel }
}
$failed | Set-Content (Join-Path $out '_failed.txt')
Write-Host "decompiled $($files.Count - $failed.Count)/$($files.Count) Lua chunks -> $out"
