# Fetches the ACVD/ACV/ACFA paramdefs from soulsmods/Paramdex at a pinned commit into external/paramdex.
# They only supply English column names for disc defs that ship without internal names.
$ErrorActionPreference = 'Stop'
$commit = 'ff7245e524329bc3eab00036723d2bd53384cedf'
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'external\paramdex'

if (Test-Path (Join-Path $dest '.git')) {
    git -C $dest fetch --depth 1 origin $commit
} else {
    git clone --filter=blob:none --no-checkout https://github.com/soulsmods/Paramdex.git $dest
}
git -C $dest sparse-checkout set --no-cone /ACVD/ /ACV/ /ACFA/
git -C $dest checkout --quiet $commit
Write-Host "Paramdex ACVD/ACV/ACFA defs at $commit -> $dest"
