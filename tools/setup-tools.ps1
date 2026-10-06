# Installs the pinned reverse-engineering toolchain into external/.
# Requires git and a JDK 21 (JAVA_HOME). The Xenia build is separate: tools\xenia\setup.ps1.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$ext = Join-Path $root 'external'
New-Item -ItemType Directory -Force $ext | Out-Null

function Get-Pinned($url, $file, $sha256) {
    $path = Join-Path $ext $file
    if (-not (Test-Path $path)) { Invoke-WebRequest $url -UserAgent 'Wget' -OutFile $path }
    $actual = (Get-FileHash $path -Algorithm SHA256).Hash
    if ($actual -ne $sha256.ToUpper()) { Remove-Item $path; throw "$file hash mismatch: $actual" }
    $path
}

$ghidraDir = Join-Path $ext 'ghidra_12.1.4_PUBLIC'
if (-not (Test-Path $ghidraDir)) {
    $zip = Get-Pinned 'https://github.com/NationalSecurityAgency/ghidra/releases/download/Ghidra_12.1.4_build/ghidra_12.1.4_PUBLIC_20260921.zip' 'ghidra.zip' 'ddac49f903da9d5bac833e5cc79395098b9c33cfd3279be5f31bd00387d2d4db'
    Expand-Archive -Force $zip $ext
    Remove-Item $zip
}

$cli = Get-Pinned 'https://github.com/akiselev/ghidra-cli/releases/download/v0.2.2/ghidra-cli-v0.2.2-x86_64-pc-windows-msvc.zip' 'ghidra-cli.zip' '36D1B70CBC6FF20860953631D73707B9EA05A29BFC705C8222E4E1BDB3F608F8'
Expand-Archive -Force $cli (Join-Path $ext 'ghidra-cli')

Get-Pinned 'https://sourceforge.net/projects/unluac/files/Unstable/unluac_2025_12_23.jar/download' 'unluac.jar' '98BE0FA84AC73CA66DCE2842A2E4512226F4C611B6500DC96415571FC5538FCC' | Out-Null

& (Join-Path $PSScriptRoot 'fetch-paramdex.ps1')

Write-Host "tools ready under $ext"
