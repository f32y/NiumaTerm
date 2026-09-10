#Requires -Version 5.1
$ErrorActionPreference = 'Stop'
$profilingScript = Join-Path $PSScriptRoot 'profiling.ps1'
$originalEncoded = [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')
$originalFlags = [Environment]::GetEnvironmentVariable('RUSTFLAGS', 'Process')
$hadEncoded = Test-Path Env:CARGO_ENCODED_RUSTFLAGS
$hadFlags = Test-Path Env:RUSTFLAGS
$originalLocation = Get-Location
$testState = @{ ExitCode = 0; Throw = $false; Invocation = $null }

$cargoMock = {
    $testState.Invocation = @{
        Arguments = @($args)
        Encoded = $env:CARGO_ENCODED_RUSTFLAGS
        Flags = $env:RUSTFLAGS
        Location = (Get-Location).Path
    }
    if ($testState.Throw) { throw 'mock cargo failure' }
    $global:LASTEXITCODE = $testState.ExitCode
}.GetNewClosure()
Set-Item -Path Function:cargo -Value $cargoMock

function Assert-Equal($Actual, $Expected, [string]$Scenario) {
    if ((ConvertTo-Json -InputObject $Actual -Compress) -cne (ConvertTo-Json -InputObject $Expected -Compress)) {
        throw "$Scenario produced an unexpected value: $Actual"
    }
}

try {
    $separator = [char]31
    foreach ($source in @('config', 'empty_plain', 'empty_encoded', 'plain', 'encoded')) {
        Remove-Item -LiteralPath Env:RUSTFLAGS, Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
        if ($source -in @('plain', 'encoded')) { $env:RUSTFLAGS = '-C opt-level=1' }
        if ($source -eq 'encoded') { $env:CARGO_ENCODED_RUSTFLAGS = "--cfg${separator}existing_probe" }
        if ($source -eq 'empty_plain') { $env:RUSTFLAGS = '' }
        if ($source -eq 'empty_encoded') { $env:CARGO_ENCODED_RUSTFLAGS = '' }
        $flagsPresent = Test-Path Env:RUSTFLAGS
        $encodedPresent = Test-Path Env:CARGO_ENCODED_RUSTFLAGS
        $flags = [Environment]::GetEnvironmentVariable('RUSTFLAGS', 'Process')
        $encoded = [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')
        $forwarded = @('test', '-p', 'nmt_profiling', '--', '--ignored')
        & $profilingScript @forwarded
        Assert-Equal $LASTEXITCODE 0 "$source exit status"
        Assert-Equal $testState.Invocation.Arguments @(
            'test', '--config', (Join-Path $PSScriptRoot 'profiling.toml'),
            '--target-dir', (Join-Path (Split-Path $PSScriptRoot -Parent) 'target/profiling'),
            '-p', 'nmt_profiling', '--', '--ignored'
        ) "$source argument forwarding"
        $expectedEncoded = if ($encodedPresent) {
            $prefix = if ($encoded) { "$encoded${separator}" } else { '' }
            "${prefix}--cfg${separator}enable_profiling"
        } else { $encoded }
        $expectedFlags = if ($flagsPresent -and -not $encodedPresent) { "$flags --cfg enable_profiling".TrimStart() } else { $flags }
        Assert-Equal $testState.Invocation.Encoded $expectedEncoded "$source encoded flags"
        Assert-Equal $testState.Invocation.Flags $expectedFlags "$source plain flags"
        Assert-Equal ([Environment]::GetEnvironmentVariable('RUSTFLAGS', 'Process')) $flags "$source flag restoration"
        Assert-Equal ([Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')) $encoded "$source encoded restoration"
        Assert-Equal (Test-Path Env:RUSTFLAGS) $flagsPresent "$source flag presence restoration"
        Assert-Equal (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) $encodedPresent "$source encoded presence restoration"
        Assert-Equal (Get-Location).Path $originalLocation.Path "$source location restoration"
    }

    $testState.ExitCode = 0
    Push-Location -LiteralPath $PSScriptRoot
    try {
        & $profilingScript build -p app --release
        Assert-Equal $LASTEXITCODE 0 'build forwarding'
        Assert-Equal $testState.Invocation.Arguments @(
            'build', '--message-format=json-render-diagnostics', '--config',
            (Join-Path $PSScriptRoot 'profiling.toml'), '--target-dir',
            (Join-Path (Split-Path $PSScriptRoot -Parent) 'target/profiling'),
            '-p', 'app', '--release'
        ) 'build arguments'
        Assert-Equal $testState.Invocation.Location (Split-Path $PSScriptRoot -Parent) 'build workspace location'
        Assert-Equal (Get-Location).Path $PSScriptRoot 'build location restoration'
    } finally {
        Pop-Location
    }

    $testState.ExitCode = 17
    & $profilingScript check -p nmt_profiling
    Assert-Equal $LASTEXITCODE 17 'cargo failure forwarding'
    $testState.Throw = $true
    $failed = $false
    try { & $profilingScript check -p nmt_profiling } catch { $failed = $true }
    Assert-Equal $failed $true 'cargo exception forwarding'
    Assert-Equal ([Environment]::GetEnvironmentVariable('RUSTFLAGS', 'Process')) $flags 'failure flag restoration'
    Assert-Equal ([Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')) $encoded 'failure encoded restoration'
    Assert-Equal (Get-Location).Path $originalLocation.Path 'failure location restoration'
    Write-Host 'Profiling script checks passed: arguments, compiler flags, restoration, and failures.'
} finally {
    if ($hadEncoded) {
        [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', $originalEncoded, 'Process')
    } else {
        Remove-Item -LiteralPath Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    }
    if ($hadFlags) {
        [Environment]::SetEnvironmentVariable('RUSTFLAGS', $originalFlags, 'Process')
    } else {
        Remove-Item -LiteralPath Env:RUSTFLAGS -ErrorAction SilentlyContinue
    }
}

exit 0
