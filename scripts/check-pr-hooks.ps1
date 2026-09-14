#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^(?:[0-9a-f]{40}|[0-9a-f]{64})$')]
    [string]$Base,

    [Parameter(Mandatory)]
    [ValidatePattern('^(?:[0-9a-f]{40}|[0-9a-f]{64})$')]
    [string]$Head,

    [Parameter(Mandatory)]
    [string]$Branch
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Git {
    param([string[]]$Arguments)

    $output = & git @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "git $($Arguments[0]) failed with exit code $LASTEXITCODE"
    }
    return $output
}

if ($env:GIT_DIR -or $env:GIT_WORK_TREE -or $env:GIT_INDEX_FILE) {
    throw 'Run from a regular checkout without Git directory or index overrides.'
}

$sourceRoot = Invoke-Git -Arguments @('rev-parse', '--show-toplevel')
$sourceHead = Invoke-Git -Arguments @('rev-parse', 'HEAD')
if ($sourceHead -ne $Head) {
    throw 'The source checkout must be at the PR head commit.'
}

$null = Invoke-Git -Arguments @('check-ref-format', '--branch', $Branch)
$null = Invoke-Git -Arguments @('merge-base', $Base, $Head)
$commits = @(Invoke-Git -Arguments @('rev-list', '--reverse', '--topo-order', "$Base..$Head"))
$bash = Join-Path $env:ProgramFiles 'Git/bin/bash.exe'

if (-not (Test-Path -LiteralPath $bash -PathType Leaf)) {
    throw "Required hook dependency is missing: $bash"
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$scratch = Join-Path $tempRoot ('niumaterm-pr-hooks-' + [Guid]::NewGuid().ToString('N'))
$repo = Join-Path $scratch 'repo'
$hookRoot = (Join-Path $scratch 'hooks').Replace('\', '/')
$messageFile = Join-Path $scratch 'commit-message.txt'
$location = Get-Location

try {
    $null = New-Item -ItemType Directory -Path $scratch

    # A separate clone lets hooks see each commit's index and working files
    # without resetting the caller's branch or including uncommitted changes.
    $null = Invoke-Git -Arguments @(
        '-c', 'core.longpaths=true', 'clone', '--quiet', '--shared', '--no-checkout',
        '--', $sourceRoot, $repo
    )
    Set-Location -LiteralPath $repo
    $null = Invoke-Git -Arguments @('config', 'core.longpaths', 'true')
    $null = Invoke-Git -Arguments @('config', 'core.autocrlf', 'false')
    $null = Invoke-Git -Arguments @('checkout', '--quiet', '--detach', $Head)
    $null = New-Item -ItemType Directory -Path $hookRoot
    Copy-Item -LiteralPath "$repo/.githooks/pre-commit", "$repo/.githooks/commit-msg" -Destination $hookRoot

    foreach ($commit in $commits) {
        Write-Host "::group::Git hooks for $commit"
        try {
            $message = Invoke-Git -Arguments @('show', '--no-patch', '--format=%B', $commit)
            [IO.File]::WriteAllText($messageFile, ($message -join "`n") + "`n")
            & $bash --noprofile --norc "$hookRoot/commit-msg" ($messageFile.Replace('\', '/'))
            if ($LASTEXITCODE -ne 0) {
                throw "commit-msg rejected $commit"
            }

            $null = Invoke-Git -Arguments @('checkout', '--quiet', '--force', '-B', $Branch, $commit)
            $parent = Invoke-Git -Arguments @('rev-parse', "$commit^1")
            # The commit tree stays staged while HEAD identifies its first
            # parent, including for merge commits with additional parents.
            $null = Invoke-Git -Arguments @('reset', '--soft', $parent)

            & $bash --noprofile --norc "$hookRoot/pre-commit"
            if ($LASTEXITCODE -ne 0) {
                throw "pre-commit rejected $commit"
            }
        } finally {
            Write-Host '::endgroup::'
        }
    }

    Write-Host "Git hooks passed for $($commits.Count) PR commit(s)."
} finally {
    Set-Location -LiteralPath $location.Path
    if (Test-Path -LiteralPath $scratch) {
        $resolved = (Resolve-Path -LiteralPath $scratch).Path
        if ([IO.Path]::GetFullPath($resolved) -ne $scratch -or
            [IO.Path]::GetDirectoryName($resolved) -ne $tempRoot.TrimEnd('\', '/')) {
            throw 'Refusing to remove an unexpected temporary checkout path.'
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
