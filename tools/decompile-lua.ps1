# Decompiles every Lua 5.0 chunk (.lc) on the owned disc into private/lua with unluac.
param([string]$Disc = (Join-Path (Split-Path -Parent $PSScriptRoot) 'ACVD Unbound'))
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$usrdir = Join-Path $Disc 'PS3_GAME\USRDIR'
$out = Join-Path $root 'private\lua'
$jar = Join-Path $root 'external\unluac.jar'
$java = if ($env:JAVA_HOME) { Join-Path $env:JAVA_HOME 'bin\java.exe' } else { 'java' }

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
