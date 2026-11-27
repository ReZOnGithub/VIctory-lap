# A profile-guided build of the layer: the compiler lays the code out by what
# a run of the title really did. Measured at the title's boat mission on a
# Ryzen 7 3700X / RTX 5070: 3 % more frames than the plain release build
# (21.0 to 21.7 fps, runs p0 and p1, 2026-11-27).
#
#   scripts\pgo.ps1 -Step train -Exe <the title's executable>
#       builds a counting build (slow: about half the frame rate) and runs it.
#       Play to where it matters for some minutes; it ends by itself after
#       -Seconds (330) and writes its counters.
#   scripts\pgo.ps1 -Step build
#       merges the counters and builds target\pgo\release\victory.exe from
#       them (some 12 minutes). scripts\play.ps1 -Build pgo runs it.
#
# The counters belong to the code as it was built: after a change to the
# sources, train again. Needs `rustup component add llvm-tools`.
param(
    [Parameter(Mandatory = $true)][ValidateSet('train', 'build')][string]$Step,
    [string]$Exe,
    [int]$Seconds = 330,
    [string]$Console = 'series-s'
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$data = Join-Path $root 'target\pgo-data'
$tools = Join-Path (& rustc --print sysroot) 'lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe'
if (-not (Test-Path $tools)) { Write-Host 'No llvm-profdata: run "rustup component add llvm-tools" first.'; exit 1 }
$agility = Join-Path $root 'target\release\D3D12'

Push-Location $root
try {
    if ($Step -eq 'train') {
        if (-not $Exe) { Write-Host 'Give the title''s executable with -Exe.'; exit 1 }
        New-Item -ItemType Directory -Force $data | Out-Null
        Remove-Item (Join-Path $data '*.profraw') -ErrorAction SilentlyContinue
        $env:RUSTFLAGS = "-Cprofile-generate=$($data -replace '\\', '/') --cfg victory_pgo"
        cargo build --locked --release -p victory --features diagnostics --target-dir target\pgo-train
        if ($LASTEXITCODE -ne 0) { exit 1 }
        $env:RUSTFLAGS = $null
        $out = Join-Path $root 'target\pgo-train\release'
        if ((Test-Path $agility) -and -not (Test-Path (Join-Path $out 'D3D12'))) { Copy-Item -Recurse $agility $out }
        $env:VICTORY_EXIT_AFTER_S = "$Seconds"
        $env:LLVM_PROFILE_FILE = (Join-Path $data 'game-%p.profraw')
        & (Join-Path $out 'victory.exe') run $Exe --execute --console $Console
        $env:VICTORY_EXIT_AFTER_S = $null
        $env:LLVM_PROFILE_FILE = $null
        $made = @(Get-ChildItem (Join-Path $data 'game-*.profraw') -ErrorAction SilentlyContinue)
        if ($made.Count -eq 0) { Write-Host 'No counters were written: the run must end by itself (-Seconds), not be closed.'; exit 1 }
        Write-Host "Counters written: $($made.Count) file(s). Now: scripts\pgo.ps1 -Step build"
    }
    else {
        $raw = @(Get-ChildItem (Join-Path $data 'game-*.profraw') -ErrorAction SilentlyContinue)
        if ($raw.Count -eq 0) { Write-Host 'No counters: run scripts\pgo.ps1 -Step train first.'; exit 1 }
        $merged = Join-Path $data 'merged.profdata'
        & $tools merge -o $merged @($raw | ForEach-Object { $_.FullName })
        if ($LASTEXITCODE -ne 0) { exit 1 }
        $env:RUSTFLAGS = "-Cprofile-use=$($merged -replace '\\', '/')"
        cargo build --locked --release -p victory --features diagnostics --target-dir target\pgo
        $built = $LASTEXITCODE
        $env:RUSTFLAGS = $null
        if ($built -ne 0) { exit 1 }
        $out = Join-Path $root 'target\pgo\release'
        if ((Test-Path $agility) -and -not (Test-Path (Join-Path $out 'D3D12'))) { Copy-Item -Recurse $agility $out }
        Write-Host "Built $out\victory.exe. Run it with: scripts\play.ps1 -Build pgo -Exe <the title's executable>"
    }
}
finally {
    $env:RUSTFLAGS = $null
    Pop-Location
}
