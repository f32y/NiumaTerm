#Requires -Version 5.1
<#
.SYNOPSIS
Measure the ConPTY throughput ceiling for a vtebench benchmark on this machine.

.DESCRIPTION
Builds benches/conpty-pipe-probe (a consumer that only drains the ConPTY
output pipe with blocking ReadFile calls) and runs vtebench under it. The
reported sample time is the best any terminal can reach with this ConPTY,
power plan, and grid size; compare NiumaTerm's vtebench numbers against it
before attributing a difference to terminal code. The active power plan is
printed first because it moved medium_cells from 26 ms to 49 ms on the same
hardware.

Requires target/release/conpty.dll and OpenConsole.exe, which the app's
release build copies next to the executable (cargo build -p app --release).

    ./scripts/conpty-pipe-probe.ps1
    ./scripts/conpty-pipe-probe.ps1 -Benchmark light_cells -Repeat 3
    ./scripts/conpty-pipe-probe.ps1 -Configs 'anon 0 64','named 256 64' -Spin 2

Each config is "<anon|named> <pipe_kib> <read_kib>": pipe kind, kernel pipe
buffer size (0 = system default), and the probe's ReadFile buffer size.
#>
param(
    [string]$Benchmark = 'medium_cells',
    [string]$VtebenchDir = 'D:\Park\vtebench',
    [int]$Cols = 138,
    [int]$Rows = 33,
    [int]$WarmUp = 3,
    [int]$MaxSecs = 10,
    [string[]]$Configs = @('anon 0 64'),
    [int]$Repeat = 1,
    [int]$Spin = 0
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$probeDir = Join-Path $repo 'benches\conpty-pipe-probe'
$probeExe = Join-Path $probeDir 'target\release\conpty-pipe-probe.exe'
$conptyDll = Join-Path $repo 'target\release\conpty.dll'
$vtebench = Join-Path $VtebenchDir 'target\release\vtebench.exe'
$benchPath = Join-Path $VtebenchDir "benchmarks\$Benchmark"

if (-not (Test-Path $conptyDll)) {
    throw "missing $conptyDll; run 'cargo build -p app --release' first so conpty.dll and OpenConsole.exe are staged"
}
if (-not (Test-Path $vtebench)) { throw "missing $vtebench" }
if (-not (Test-Path $benchPath)) { throw "missing benchmark $benchPath" }

cargo build --release --manifest-path (Join-Path $probeDir 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'probe build failed' }

powercfg /getactivescheme
Write-Host "cpu clock: $((Get-CimInstance Win32_Processor).CurrentClockSpeed) MHz"

$env:PROBE_CONPTY_DLL = $conptyDll
$env:PROBE_SPIN = "$Spin"

for ($i = 1; $i -le $Repeat; $i++) {
    foreach ($config in $Configs) {
        $parts = $config -split '\s+'
        if ($parts.Count -ne 3) { throw "config '$config' must be '<anon|named> <pipe_kib> <read_kib>'" }
        Write-Host "===== run $i/$Repeat config='$config' spin=$Spin benchmark=$Benchmark"
        & $probeExe $parts[0] $parts[1] $parts[2] $Cols $Rows -- `
            $vtebench --benchmarks $benchPath --warmup $WarmUp --max-secs $MaxSecs
    }
}
