#Requires -Version 5.1
<#
.SYNOPSIS
Run `cargo build` and stamp the resulting executables with the HEAD commit time.

.DESCRIPTION
All arguments are forwarded to cargo, so this is a drop-in replacement:

    ./scripts/build.ps1 --release
    ./scripts/build.ps1 -p niuma-term --profile dist

Asking cargo for JSON artifact messages locates the output directory without
reimplementing cargo's profile and target-triple flag parsing. Human-readable
diagnostics and progress still go to stderr as usual.
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArgs)

$ErrorActionPreference = 'Stop'

# Build scripts are reported as executables too, under their own
# target/<profile>/build/<pkg>-<hash>/ directory, so they must be excluded before
# the profile root can be identified. A cdylib such as shell_extension.dll
# reports a null `executable`, so fall back to `filenames`. Cargo uplifts every
# linkable artifact into the profile root and leaves intermediate rlibs in deps/,
# which is the remaining directory to ignore.
$dirs = cargo build --message-format=json-render-diagnostics @CargoArgs |
    ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.kind -notcontains 'custom-build' } |
    ForEach-Object { if ($_.executable) { $_.executable } else { $_.filenames } } |
    Split-Path -Parent |
    Where-Object { (Split-Path $_ -Leaf) -ne 'deps' } |
    Select-Object -Unique

if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# @() because a single-element result arrives as a bare string, and indexing a
# string would yield its first character. -First 1 is not an option above: it
# stops the pipeline early and kills cargo before it reports its exit status.
$dir = @($dirs)[0]
if ($dir) {
    & "$PSScriptRoot/stamp-build-time.ps1" -Dir $dir
} else {
    Write-Host 'no linked artifacts produced; nothing to stamp'
}
