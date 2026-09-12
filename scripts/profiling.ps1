#Requires -Version 5.1
<#
.SYNOPSIS
Run Cargo with performance collection compiled in.

.DESCRIPTION
Uses a separate target/profiling directory and preserves platform compiler flags.
The application still needs --enable-profiling to activate runtime-gated probes.

    ./scripts/profiling.ps1 build -p app --release
    ./scripts/profiling.ps1 test -p nmt_profiling
    ./scripts/profiling.ps1 test -p app --lib agent_tab::transcript::profiling

Quote '--' when forwarding arguments to the Rust test runner or Clippy.
PowerShell consumes an unquoted -- before the script can receive it.
#>
param()

$CargoArgs = @($args)
if ($CargoArgs.Count -eq 0) { throw 'Provide a Cargo command, such as build or test.' }

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$config = Join-Path $PSScriptRoot 'profiling.toml'
$targetDir = Join-Path $repo 'target/profiling'
$originalEncoded = [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')
$originalFlags = [Environment]::GetEnvironmentVariable('RUSTFLAGS', 'Process')
$hadEncoded = Test-Path Env:CARGO_ENCODED_RUSTFLAGS
$hadFlags = Test-Path Env:RUSTFLAGS

try {
    # Environment flags override Cargo configuration, so extend the active source.
    if ($hadEncoded) {
        $separator = [char]31
        $encodedPrefix = if ($originalEncoded) { "$originalEncoded${separator}" } else { '' }
        $env:CARGO_ENCODED_RUSTFLAGS = "${encodedPrefix}--cfg${separator}enable_profiling"
    } elseif ($hadFlags) {
        $env:RUSTFLAGS = "$originalFlags --cfg enable_profiling".TrimStart()
    }

    Push-Location -LiteralPath $repo
    try {
        $command = $CargoArgs[0]
        $arguments = @('--config', $config, '--target-dir', $targetDir)
        $arguments += @($CargoArgs | Select-Object -Skip 1)
        if ($command -eq 'build') {
            & "$PSScriptRoot/build.ps1" -CargoArgs $arguments
        } else {
            & cargo $command @arguments
        }
        $result = $LASTEXITCODE
    } finally {
        Pop-Location
    }
} finally {
    # Removing an absent value explicitly avoids creating an empty override.
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

exit $result
