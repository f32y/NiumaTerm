#Requires -Version 5.1
<#
.SYNOPSIS
Set the creation and last-write times of built binaries to the HEAD commit time.

.DESCRIPTION
Cargo stamps artifacts with the wall-clock time of the link step, so two builds
of the same commit produce files that differ in metadata. Rewriting the
timestamps to the HEAD committer date makes the artifact's mtime identify the
source revision instead of the moment the machine happened to link it.

Cargo has no post-build hook, so this runs as a separate step:

    cargo build --release; ./scripts/stamp-build-time.ps1 -Profile release
#>
[CmdletBinding()]
param(
    [string]$Profile = 'debug',
    # Cross-compiled artifacts live under target/<triple>/<profile>/.
    [string]$Target,
    # Output directory to stamp; overrides -Profile/-Target when given.
    [string]$Dir
)

$ErrorActionPreference = 'Stop'

# Only artifacts this workspace links. The output directory also collects files
# that a build script merely copied in, such as the prebuilt OpenConsole.exe,
# whose timestamps say nothing about the revision being built.
$names = @('NiumaTerm.exe', 'NmtAgentHook.exe', 'NmtShellExtension.dll', 'tree_sitter.dll')

$repo = git rev-parse --show-toplevel
if ($LASTEXITCODE -ne 0) { throw 'not a git repository' }

$iso = git log -1 --format=%cI HEAD
if ($LASTEXITCODE -ne 0) { throw 'cannot read HEAD commit time' }
$stamp = [datetimeoffset]::Parse($iso).LocalDateTime

if (-not $Dir) {
    $Dir = if ($Target) { "$repo/target/$Target/$Profile" } else { "$repo/target/$Profile" }
}
if (-not (Test-Path $Dir)) { throw "no build directory at $Dir" }

foreach ($name in $names) {
    # A partial build (`cargo build -p ...`) legitimately produces only some of
    # these, so a missing artifact is not an error.
    $f = Get-Item -LiteralPath (Join-Path $Dir $name) -ErrorAction SilentlyContinue
    if (-not $f) { continue }
    $f.CreationTime = $stamp
    $f.LastWriteTime = $stamp
    Write-Host "$($f.Name) -> $($stamp.ToString('s'))"
}
