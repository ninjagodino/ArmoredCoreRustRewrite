# Installs the pinned reverse-engineering toolchain into external/ and decrypts the owned EBOOT.
# Requires git, a JDK 21 (JAVA_HOME), and Windows tar (bsdtar) for the RPCS3 .7z.
param([string]$Disc = (Join-Path (Split-Path -Parent $PSScriptRoot) 'ACVD Unbound'))
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

# Ps3GhidraScripts has no 12.1.4 build; the 12.1.2 build loads once its declared version matches.
$ps3 = Get-Pinned 'https://github.com/clienthax/Ps3GhidraScripts/releases/download/1.0111/ghidra_12.1.2_PUBLIC_20260712_Ps3GhidraScripts.zip' 'ps3scripts.zip' 'C1106EF993FED87420746F46F85B0DE80E523C11A49A50001A16C4FB43CAA5E2'
Expand-Archive -Force $ps3 (Join-Path $ext 'ps3scripts')
$extDir = Join-Path $ghidraDir 'Ghidra\Extensions\Ps3GhidraScripts'
Copy-Item -Recurse -Force (Join-Path $ext 'ps3scripts\Ps3GhidraScripts') (Split-Path $extDir)
$props = Join-Path $extDir 'extension.properties'
(Get-Content $props) -replace '^version=.*', 'version=12.1.4' | Set-Content $props

# Ps3GhidraScripts README: r2 (TOC) must be preserved across calls for usable decompilation.
$cspec = Join-Path $ghidraDir 'Ghidra\Processors\PowerPC\data\languages\ppc_64_32.cspec'
$text = Get-Content $cspec -Raw
if ($text -notmatch '<unaffected>\s*<register name="r2"/>') {
    Set-Content $cspec ($text -replace '<unaffected>', "<unaffected>`r`n        <register name=`"r2`"/>") -NoNewline
}

Get-Pinned 'https://sourceforge.net/projects/unluac/files/Unstable/unluac_2025_12_23.jar/download' 'unluac.jar' '98BE0FA84AC73CA66DCE2842A2E4512226F4C611B6500DC96415571FC5538FCC' | Out-Null

$rpcs3 = Join-Path $ext 'rpcs3'
if (-not (Test-Path (Join-Path $rpcs3 'rpcs3.exe'))) {
    $7z = Get-Pinned 'https://github.com/RPCS3/rpcs3-binaries-win/releases/download/build-d5213de6dd329b4f61ad0b95c1a3dc7d6622aa3b/rpcs3-v0.0.43-20184-d5213de6_win64_msvc.7z' 'rpcs3.7z' '4932C746AE5F5F803E75F9951B8A85F796D2310D2A5C7993C84AFAD8DE0E0ED2'
    New-Item -ItemType Directory -Force $rpcs3 | Out-Null
    tar -xf $7z -C $rpcs3
}

& (Join-Path $PSScriptRoot 'fetch-paramdex.ps1')

# Decrypt a private copy of the owned EBOOT; the disc dump itself is never written to.
$exe = Join-Path $root 'private\exe'
New-Item -ItemType Directory -Force $exe | Out-Null
if (-not (Test-Path (Join-Path $exe 'EBOOT.elf'))) {
    Copy-Item (Join-Path $Disc 'PS3_GAME\USRDIR\EBOOT.BIN') (Join-Path $exe 'EBOOT.BIN') -Force
    Start-Process (Join-Path $rpcs3 'rpcs3.exe') -ArgumentList '--decrypt', "`"$(Join-Path $exe 'EBOOT.BIN')`"" -Wait
}
Write-Host "tools ready under $ext; decrypted executable at $exe\EBOOT.elf"
