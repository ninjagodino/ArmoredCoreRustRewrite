# Builds the project's Xenia Canary (branch acvd-debug = upstream + acvd-probe.patch) into
# external/xenia-canary. Needs Visual Studio 2022 (C++), Python 3, and the Vulkan SDK (VULKAN_SDK
# set, or -VulkanSdk). Safe to re-run: an existing checkout is only rebuilt.
param(
    [string]$VulkanSdk = $env:VULKAN_SDK,
    [string]$Base = "0b0d57a1bc55f62e2bd43516bea96c848a9a6f96"
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path "$PSScriptRoot\..\..").Path
$xenia = "$root\external\xenia-canary"

if (-not $VulkanSdk -or -not (Test-Path "$VulkanSdk\Bin\spirv-opt.exe")) {
    throw "Vulkan SDK not found: set VULKAN_SDK or pass -VulkanSdk (needs Bin\spirv-opt.exe)"
}
$env:VULKAN_SDK = $VulkanSdk
$env:PATH = "$VulkanSdk\Bin;$env:PATH"
$python = (Get-Command python -All | Where-Object { $_.Source -notmatch "WindowsApps" } | Select-Object -First 1).Source
if (-not $python) { throw "Python 3 not found on PATH" }

if (-not (Test-Path "$xenia\.git")) {
    git clone --filter=blob:none https://github.com/xenia-canary/xenia-canary.git $xenia
}
Push-Location $xenia
try {
    if (-not (git branch --list acvd-debug)) {
        git switch -c acvd-debug $Base
        git -c user.name=acvd -c user.email=acvd@local am "$PSScriptRoot\acvd-probe.patch"
    } else {
        git switch acvd-debug
    }
    $subs = (Select-String -Path .gitmodules -Pattern "^\s*path = (.+)$").Matches |
        ForEach-Object { $_.Groups[1].Value } | Where-Object { $_ -notmatch "xbyak_aarch64" }
    git submodule update --init --depth 1 --jobs 8 -- $subs
    & $python xenia-build.py build --config release
    if ($LASTEXITCODE) { throw "xenia build failed" }
    New-Item -ItemType File -Force "build\bin\Windows\Release\portable.txt" | Out-Null
} finally {
    Pop-Location
}
"built $xenia\build\bin\Windows\Release\xenia_canary.exe"
