#Requires -Version 5.1
<#
.SYNOPSIS
Launch NiumaTerm with frame-pacing diagnostics on and stream the per-second
digest to this console.

.DESCRIPTION
Builds with --cfg enable_profiling, starts the app with --enable-profiling, and tails the probe output out of the
app log:

    ./scripts/frame-stats.ps1                 # release build, runs until closed
    ./scripts/frame-stats.ps1 -Seconds 30     # stop the app after 30 seconds
    ./scripts/frame-stats.ps1 -DebugBuild     # measure a debug build instead

Measure a release build: at opt-level 0 the scene build alone exceeds a 144Hz
frame budget, so a debug run reports its own slowness rather than the app's.

Reading the digest: `vsync` counts ticks the frame pump saw, `req` the redraws
it asked for, `frames` the frames actually presented. Each step down from the
display refresh rate localizes the stall.
#>
[CmdletBinding()]
param(
    [switch]$DebugBuild,
    [switch]$NoBuild,
    [int]$Seconds = 0
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path $PSScriptRoot -Parent
$profileDir = if ($DebugBuild) { 'debug' } else { 'release' }
$exe = Join-Path $repo "target/profiling/$profileDir/NiumaTerm.exe"

if (-not $NoBuild) {
    $buildArgs = @('build', '-p', 'app', '--bin', 'NiumaTerm')
    if (-not $DebugBuild) { $buildArgs += '--release' }
    & "$PSScriptRoot/profiling.ps1" @buildArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

# The app rotates app.log to app-prev1.log on startup, so the tail must attach
# to the file the new process creates, not the one sitting there now.
$log = Join-Path $env:LOCALAPPDATA 'NiumaTerm/Test/logs/app.log'
$launchedAt = [DateTime]::UtcNow

$app = Start-Process -FilePath $exe -ArgumentList '--testing', '--enable-profiling' -WindowStyle Hidden -PassThru

try {
    while (-not $app.HasExited) {
        $logFile = Get-Item -LiteralPath $log -ErrorAction SilentlyContinue
        if ($logFile -and $logFile.LastWriteTimeUtc -ge $launchedAt) { break }
        Start-Sleep -Milliseconds 100
    }
    if (-not $logFile -or $logFile.LastWriteTimeUtc -lt $launchedAt) {
        throw "app exited before writing $log"
    }

    if ($Seconds -gt 0) {
        $stop = Start-Job -ArgumentList $app.Id, $Seconds -ScriptBlock {
            param($id, $seconds)
            Start-Sleep -Seconds $seconds
            Stop-Process -Id $id -ErrorAction SilentlyContinue
        }
    }

    Write-Host "streaming frame stats from $log (Ctrl-C to stop)" -ForegroundColor Cyan
    Get-Content $log -Wait -Tail 0 | Where-Object { $_ -match 'frame_stats|vsync provider|slow main-thread|slow window message' }
} finally {
    if ($stop) { Remove-Job $stop -Force -ErrorAction SilentlyContinue }
    if (-not $app.HasExited) { Stop-Process -Id $app.Id -ErrorAction SilentlyContinue }
}
