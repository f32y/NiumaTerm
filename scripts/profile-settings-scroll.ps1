#Requires -Version 7.0
<#
.SYNOPSIS
Measure settings scrolling in an isolated native window.

.DESCRIPTION
Build with profiling enabled, then collect sidebar and theme scrolling on the
current display. The test window opens visibly and exits after the samples.
Use -ExtraThemes 100 to exercise a larger theme directory.
#>
param(
    [switch]$Build,
    [ValidateRange(0, 1000)][int]$ExtraThemes = 0,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if ($Build) {
    & "$PSScriptRoot/profiling.ps1" build -p app --bin NiumaTerm --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repo "target/settings-scroll/$(Get-Date -Format 'yyyyMMdd-HHmmss')"
}
$output = [IO.Path]::GetFullPath($OutputDirectory)
$null = New-Item -ItemType Directory -Path $output -Force
$config = Join-Path $output 'config'
$themes = Join-Path $config 'Test/themes'
$null = New-Item -ItemType Directory -Path $themes -Force
$source = Get-Content -LiteralPath (Join-Path $repo 'crates/config/src/builtin_themes/modern_dark.toml') -Raw
for ($index = 0; $index -lt $ExtraThemes; $index++) {
    $name = 'Profile Theme {0:d4}' -f $index
    $theme = $source -replace '(?m)^name = .*$', ('name = "{0}"' -f $name)
    Set-Content -LiteralPath (Join-Path $themes "profile-$index.toml") -Value $theme -Encoding utf8
}

$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = Join-Path $repo 'target/profiling/release/NiumaTerm.exe'
$start.ArgumentList.Add('--testing')
$start.ArgumentList.Add('--enable-profiling')
$start.Environment['NMT_CONFIG_HOME'] = $config
$start.Environment['NMT_SETTINGS_SCROLL_PROFILE'] = '1'
$start.UseShellExecute = $false
$process = [Diagnostics.Process]::Start($start)
try {
    if (-not $process.WaitForExit(60000)) { throw 'Settings scrolling did not finish within 60 seconds.' }
    if ($process.ExitCode -ne 0) { throw "Settings scrolling exited with code $($process.ExitCode)." }
} finally {
    if (-not $process.HasExited) { $process.Kill() }
    $process.Dispose()
}

$log = Join-Path $output 'app.log'
Copy-Item -LiteralPath "$env:LOCALAPPDATA/NiumaTerm/Test/logs/app.log" -Destination $log
$scenario = ''
$started = [DateTimeOffset]::MinValue
$samples = @(foreach ($line in Get-Content -LiteralPath $log) {
    if ($line -match '^(\S+).*starting native scrolling sample scenario="([^"]+)"') {
        $started = [DateTimeOffset]::Parse($Matches[1])
        $scenario = $Matches[2]
    } elseif ($scenario -and $line -match '^(\S+).*frames ([\d.]+)/s \(display ([\d.]+)Hz\).*draw avg ([\d.]+)ms max ([\d.]+)ms.*present avg ([\d.]+)ms') {
        $elapsed = ([DateTimeOffset]::Parse($Matches[1]) - $started).TotalSeconds
        if ($elapsed -ge 2 -and $elapsed -le 8) {
            [pscustomobject]@{
                Scenario = $scenario
                ElapsedSeconds = $elapsed
                FramesPerSecond = [double]$Matches[2]
                DisplayHz = [double]$Matches[3]
                DrawAverageMs = [double]$Matches[4]
                DrawMaximumMs = [double]$Matches[5]
                PresentAverageMs = [double]$Matches[6]
            }
        }
    }
})
if ($samples.Count -eq 0) { throw "No native frame samples were found in $log." }
$samples | Export-Csv -LiteralPath (Join-Path $output 'samples.csv') -NoTypeInformation
$samples | Group-Object Scenario | ForEach-Object {
    [pscustomobject]@{
        Scenario = $_.Name
        FPS = [math]::Round(($_.Group.FramesPerSecond | Measure-Object -Average).Average, 1)
        DrawMs = [math]::Round(($_.Group.DrawAverageMs | Measure-Object -Average).Average, 2)
        WorstDrawMs = ($_.Group.DrawMaximumMs | Measure-Object -Maximum).Maximum
    }
} | Format-Table
Write-Host "Samples and log: $output"
