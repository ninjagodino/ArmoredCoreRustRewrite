# Decompiles every Lua 5.0 chunk (.lc) under `script/` (script.bhd and the load bundles) on the
# owned 360 disc with unluac into private/lua360 (the AI scripts keep their LogAI state names).
param([string]$Disc = (Join-Path (Split-Path -Parent $PSScriptRoot) 'armoredcoredumps\Armored Core - Verdict Day (USA)\Armored Core - Verdict Day (USA).iso'))
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$jar = Join-Path $root 'external\unluac.jar'
$java = if ($env:JAVA_HOME) { Join-Path $env:JAVA_HOME 'bin\java.exe' } else { 'java' }

$chunks = Join-Path $root 'private\tmp\lc360'
$out = Join-Path $root 'private\lua360'
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
cargo run --release -q -p acvd-formats --example discls -- --disc $Disc --extract script/ $chunks
if ($LASTEXITCODE -ne 0) { throw 'extracting script/ from the ISO failed' }

$files = Get-ChildItem $chunks -Recurse -Filter *.lc
$failed = @()
foreach ($f in $files) {
    $rel = $f.FullName.Substring($chunks.Length + 1)
    $dest = Join-Path $out ([IO.Path]::ChangeExtension($rel, '.lua'))
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    & $java -jar $jar --output $dest $f.FullName 2>$null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $dest)) { $failed += $rel }
}
$failed | Set-Content (Join-Path $out '_failed.txt')
Write-Host "decompiled $($files.Count - $failed.Count)/$($files.Count) Lua chunks -> $out"
