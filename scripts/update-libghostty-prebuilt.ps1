<#
.SYNOPSIS
    Rebuild the checked-in libghostty-vt prebuilt static archive from source.

.DESCRIPTION
    The `libghostty-vt-sys` crate can either build libghostty-vt from the pinned
    Ghostty sources via Zig (the default) or link the checked-in prebuilt package
    under `third_party/libghostty-vt-sys/prebuilt/<target>/`. The prebuilt package
    is what CI and `NiumaTerm_USE_PREBUILT_LIBGHOSTTY=1` builds link against, so it
    must be regenerated whenever the vendored source patches change the C ABI
    (e.g. a new `patches/000N-*.patch` adds an exported symbol).

    This script rebuilds the crate from source in the same optimize mode as the
    committed artifact (ReleaseFast), strips build-machine debug paths from the
    freshly-built archive, copies it and the headers into the prebuilt directory,
    and (unless skipped) verifies the prebuilt link path still builds.

    Only the ghostty static archive and headers are refreshed; the bundled
    `simdutf.lib` / `highway.lib` dependency archives are unchanged.

.PARAMETER Target
    Rust target triple whose prebuilt package to refresh. Defaults to
    x86_64-pc-windows-msvc (the only checked-in prebuilt).

.PARAMETER Optimize
    Zig optimize mode for the vendored build (LIBGHOSTTY_VT_SYS_OPTIMIZE).
    Defaults to ReleaseFast to match the committed archive.

.PARAMETER ZigDir
    Optional directory containing `zig(.exe)` to prepend to PATH for the build.
    Omit if `zig` is already on PATH.

.PARAMETER SkipVerify
    Skip the final `NiumaTerm_USE_PREBUILT_LIBGHOSTTY=1` link check.

.EXAMPLE
    pwsh scripts/update-libghostty-prebuilt.ps1 -ZigDir <directory-containing-zig>
#>
[CmdletBinding()]
param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [ValidateSet("Debug", "ReleaseSafe", "ReleaseFast", "ReleaseSmall")]
    [string]$Optimize = "ReleaseFast",
    [string]$ZigDir,
    [switch]$SkipVerify
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path

$prebuiltDir = Join-Path $repoRoot "third_party/libghostty-vt-sys/prebuilt/$Target"
if (-not (Test-Path -LiteralPath $prebuiltDir -PathType Container)) {
    throw "No prebuilt package for target '$Target' at $prebuiltDir"
}

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repoRoot "target" }
# `cargo build --target <triple>` nests artifacts under target/<triple>/, even when
# the triple equals the host, so the build-script output dir is target/<triple>/debug/build.
$buildDir = Join-Path $targetDir "$Target/debug/build"

# Prepend a caller-supplied Zig directory to PATH for this process only.
if ($ZigDir) {
    $ZigDir = (Resolve-Path -LiteralPath $ZigDir).Path
    $env:PATH = "$ZigDir$([System.IO.Path]::PathSeparator)$env:PATH"
}
if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    throw "zig not found on PATH. Install Zig or pass -ZigDir <dir containing zig.exe>."
}

function Invoke-Checked {
    param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string[]]$Args)
    & $Exe @Args
    if ($LASTEXITCODE -ne 0) {
        throw "$Exe $($Args -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Find-LlvmTool {
    param([Parameter(Mandatory)][string]$Name)

    $override = [Environment]::GetEnvironmentVariable($Name.ToUpperInvariant().Replace("-", "_"))
    if ($override) { return (Resolve-Path -LiteralPath $override).Path }

    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }

    foreach ($base in @(
        "$env:ProgramFiles/Microsoft Visual Studio",
        "${env:ProgramFiles(x86)}/Microsoft Visual Studio"
    )) {
        $tool = Get-ChildItem -Path $base -Recurse -Filter "$Name.exe" -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "VC\\Tools\\Llvm\\x64\\bin" } |
            Select-Object -First 1
        if ($tool) { return $tool.FullName }
    }

    throw "Could not find $Name. Install the Visual Studio C++ Clang tools or set $($Name.ToUpperInvariant().Replace('-', '_'))."
}

function Copy-StrippedArchive {
    param([Parameter(Mandatory)][string]$Source, [Parameter(Mandatory)][string]$Destination)

    $ar = Find-LlvmTool "llvm-ar"
    $objcopy = Find-LlvmTool "llvm-objcopy"
    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "niumaterm-archive-$([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $tempDir | Out-Null

    try {
        $members = @(& $ar t $Source)
        if ($LASTEXITCODE -ne 0 -or $members.Count -eq 0) {
            throw "Could not list archive members in $Source"
        }
        $names = @($members | ForEach-Object { [System.IO.Path]::GetFileName($_) })
        if (($names | Sort-Object -Unique).Count -ne $names.Count) {
            throw "Archive contains duplicate member names and cannot be safely repacked: $Source"
        }

        Push-Location $tempDir
        try {
            Invoke-Checked $ar @("x", $Source)
            foreach ($name in $names) {
                Invoke-Checked $objcopy @("--strip-debug", $name)
            }
            Invoke-Checked $ar (@("rcs", $Destination) + $names)
        }
        finally {
            Pop-Location
        }
    }
    finally {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Push-Location $repoRoot
try {
    # 1. Force a clean vendored build in the requested optimize mode so the
    #    simdutf-localized archive is regenerated from the current sources/patches.
    Write-Host "==> Building libghostty-vt from source (optimize=$Optimize, target=$Target)"
    Remove-Item Env:NiumaTerm_USE_PREBUILT_LIBGHOSTTY -ErrorAction SilentlyContinue
    $env:LIBGHOSTTY_VT_SYS_OPTIMIZE = $Optimize

    Invoke-Checked cargo @("clean", "-p", "libghostty-vt-sys")
    Invoke-Checked cargo @("build", "-p", "libghostty-vt-sys", "--target", $Target)

    # 2. Locate the freshly-built simdutf-localized static archive. The prebuilt
    #    link path ships simdutf/highway separately and does not re-localize, so
    #    the localized variant (not ghostty-install/lib) is the correct source.
    Write-Host "==> Locating built artifacts under $buildDir"
    $lib = Get-ChildItem -Path $buildDir -Recurse -Filter "ghostty-vt-static.lib" -ErrorAction Stop |
        Where-Object { $_.FullName -match "simdutf-localized" } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $lib) {
        throw "Could not find a simdutf-localized ghostty-vt-static.lib under $buildDir"
    }

    $incDir = Get-ChildItem -Path $buildDir -Recurse -Directory -Filter "include" -ErrorAction Stop |
        Where-Object { $_.FullName -match "ghostty-install" -and (Test-Path (Join-Path $_.FullName "ghostty/vt.h")) } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $incDir) {
        throw "Could not find a ghostty-install/include directory under $buildDir"
    }

    Write-Host "    lib:     $($lib.FullName) ($([math]::Round($lib.Length / 1MB, 2)) MiB)"
    Write-Host "    include: $($incDir.FullName)"

    # 3. Copy the archive + headers into the prebuilt package.
    Write-Host "==> Updating prebuilt package at $prebuiltDir"
    Copy-StrippedArchive $lib.FullName (Join-Path $prebuiltDir "lib/ghostty-vt-static.lib")
    Copy-Item -Path (Join-Path $incDir.FullName "*") -Destination (Join-Path $prebuiltDir "include") -Recurse -Force

    # 4. Verify the prebuilt link path still builds against the refreshed archive.
    if (-not $SkipVerify) {
        Write-Host "==> Verifying prebuilt link path (NiumaTerm_USE_PREBUILT_LIBGHOSTTY=1)"
        Remove-Item Env:LIBGHOSTTY_VT_SYS_OPTIMIZE -ErrorAction SilentlyContinue
        $env:NiumaTerm_USE_PREBUILT_LIBGHOSTTY = "1"
        Invoke-Checked cargo @("clean", "-p", "libghostty-vt-sys")
        Invoke-Checked cargo @("build", "-p", "nmt_terminal", "--target", $Target)
        Write-Host "    prebuilt link OK"
    }

    Write-Host ""
    Write-Host "Prebuilt updated. Review and commit:"
    Write-Host "  git add third_party/libghostty-vt-sys/prebuilt/$Target"
    Write-Host "  git status --short -- third_party/libghostty-vt-sys/prebuilt/$Target"
}
finally {
    Pop-Location
    Remove-Item Env:LIBGHOSTTY_VT_SYS_OPTIMIZE -ErrorAction SilentlyContinue
    Remove-Item Env:NiumaTerm_USE_PREBUILT_LIBGHOSTTY -ErrorAction SilentlyContinue
}
