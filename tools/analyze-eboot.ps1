# Imports the decrypted EBOOT into a headless Ghidra project at private/ghidra/ACVD with PS3 NID/TOC setup.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$ghidra = Join-Path $root 'external\ghidra_12.1.4_PUBLIC'
$proj = Join-Path $root 'private\ghidra'
New-Item -ItemType Directory -Force $proj | Out-Null
& (Join-Path $ghidra 'support\analyzeHeadless.bat') $proj ACVD `
    -import (Join-Path $root 'private\exe\EBOOT.elf') -overwrite `
    -processor 'PowerPC:BE:64:A2ALT-32addr' -cspec default `
    -preScript AnalyzePs3Binary.java -postScript DefinePS3Syscalls.java `
    -log (Join-Path $proj 'analyze.log') -scriptlog (Join-Path $proj 'scripts.log') -max-cpu 8
