#Requires -Version 7.0
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$runner = Join-Path $PSScriptRoot 'check-pr-hooks.ps1'
$hookRoot = Join-Path (Split-Path $PSScriptRoot -Parent) '.githooks'
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$scratch = Join-Path $tempRoot ('niumaterm hook tests ' + [Guid]::NewGuid().ToString('N'))
$repo = Join-Path $scratch 'repo'
$location = Get-Location

function Invoke-Git {
    param([string[]]$Arguments)

    $output = & git @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "git $($Arguments[0]) failed with exit code $LASTEXITCODE"
    }
    return $output
}

function Write-File {
    param([string]$Path, [string]$Content)

    $absolute = Join-Path $repo $Path
    $null = New-Item -ItemType Directory -Force -Path (Split-Path $absolute -Parent)
    [IO.File]::WriteAllText($absolute, "$Content`n")
}

function New-Commit {
    param([string]$Message)

    $null = Invoke-Git -Arguments @('add', '--all')
    $null = Invoke-Git -Arguments @('commit', '--quiet', '-m', $Message)
    return Invoke-Git -Arguments @('rev-parse', 'HEAD')
}

function Assert-Runner {
    param(
        [string]$Base,
        [string]$Branch = 'feature',
        [bool]$Success = $true,
        [string]$Expected
    )

    $headCommit = Invoke-Git -Arguments @('rev-parse', 'HEAD')
    $output = & pwsh -NoLogo -NoProfile -NonInteractive -File $runner -Base $Base -Head $headCommit -Branch $Branch 2>&1
    $exitCode = $LASTEXITCODE
    $outputText = $output -join "`n"
    if (($exitCode -eq 0) -ne $Success -or -not $outputText.Contains($Expected)) {
        throw "Unexpected hook result (exit $exitCode); expected: $Expected`n$outputText"
    }
    Write-Host "PASS: $Expected"
}

try {
    $null = New-Item -ItemType Directory -Path "$repo/.githooks"
    Copy-Item -LiteralPath "$hookRoot/pre-commit", "$hookRoot/commit-msg" -Destination "$repo/.githooks"
    Set-Location -LiteralPath $repo
    $null = Invoke-Git -Arguments @('init', '--quiet', '--initial-branch=main')
    $null = Invoke-Git -Arguments @('config', 'user.name', 'HookTest')
    $null = Invoke-Git -Arguments @('config', 'user.email', 'test@example.com')
    $null = Invoke-Git -Arguments @('config', 'core.autocrlf', 'false')
    # Invalid fixture commits must exist before the runner can reject them.
    $null = Invoke-Git -Arguments @('config', 'core.hooksPath', '.git/no-hooks')
    $null = Invoke-Git -Arguments @('config', 'commit.gpgsign', 'false')
    Write-File -Path 'README.md' -Content 'Baseline'
    $baseline = New-Commit -Message 'chore: initialize fixture'

    # The base branch can advance independently; its commits are not PR commits.
    Write-File -Path 'base.txt' -Content 'Base branch only'
    $advancedBase = New-Commit -Message 'invalid base subject'
    $null = Invoke-Git -Arguments @('checkout', '--quiet', '-B', 'feature', $baseline)
    Write-File -Path '.agents/note.md' -Content 'Keep changes focused.'
    $null = New-Commit -Message 'docs: add guidance'
    Write-File -Path 'src/change.ps1' -Content 'Write-Output 1'
    $null = New-Commit -Message 'chore: add command'
    Write-File -Path 'third_party/gpui/widget.cpp' -Content 'int value = 1;'
    $null = New-Commit -Message 'chore: update widget'

    Write-File -Path 'README.md' -Content 'Staged local edit'
    $null = Invoke-Git -Arguments @('add', 'README.md')
    Write-File -Path 'README.md' -Content 'Unstaged local edit'
    Write-File -Path 'untracked.txt' -Content 'Untracked local edit'
    Write-File -Path '.githooks/pre-commit' -Content "#!/bin/sh`nexit 99"
    $beforeHead = Invoke-Git -Arguments @('rev-parse', 'HEAD')
    $beforeStatus = (Invoke-Git -Arguments @('status', '--porcelain=v1')) -join "`n"
    $beforeIndex = (Invoke-Git -Arguments @('diff', '--cached')) -join "`n"
    $beforeWorktree = (Invoke-Git -Arguments @('diff')) -join "`n"
    Assert-Runner -Base $advancedBase -Expected 'Git hooks passed for 3 PR commit(s).'
    if ((Invoke-Git -Arguments @('rev-parse', 'HEAD')) -ne $beforeHead -or
        ((Invoke-Git -Arguments @('status', '--porcelain=v1')) -join "`n") -ne $beforeStatus -or
        ((Invoke-Git -Arguments @('diff', '--cached')) -join "`n") -ne $beforeIndex -or
        ((Invoke-Git -Arguments @('diff')) -join "`n") -ne $beforeWorktree -or
        [IO.File]::ReadAllText("$repo/untracked.txt") -ne "Untracked local edit`n") {
        throw 'The runner modified the source checkout.'
    }
    Write-Host 'PASS: source checkout and local edits are preserved'
    Remove-Item -LiteralPath "$repo/untracked.txt"

    $null = Invoke-Git -Arguments @('checkout', '--quiet', '--force', '-B', 'feature', $baseline)
    Write-File -Path 'change.txt' -Content 'Invalid message'
    $null = New-Commit -Message 'invalid subject'
    Assert-Runner -Base $baseline -Success $false -Expected 'commit-msg rejected'

    $null = Invoke-Git -Arguments @('checkout', '--quiet', '--force', '-B', 'feature', $baseline)
    Write-File -Path '.agents/note.md' -Content 'Keep changes focused.'
    Write-File -Path 'src/change.ps1' -Content 'Write-Output 1'
    $null = New-Commit -Message 'chore: mix changes'
    Assert-Runner -Base $baseline -Success $false -Expected 'AI docs must be committed separately'

    $null = Invoke-Git -Arguments @('checkout', '--quiet', '--force', '-B', 'feature', $baseline)
    Write-File -Path 'openspec/changes/demo/proposal.md' -Content 'Unfinished proposal'
    $null = New-Commit -Message 'docs: add proposal'
    Remove-Item -LiteralPath "$repo/openspec/changes/demo/proposal.md"
    $null = New-Commit -Message 'docs: remove proposal'
    Assert-Runner -Base $baseline -Success $false -Expected 'unfinished spec documents cannot be committed'

    $null = Invoke-Git -Arguments @('checkout', '--quiet', '--force', '-B', 'main', $baseline)
    Write-File -Path '.agents/note.md' -Content 'Keep changes focused.'
    $null = New-Commit -Message 'docs: add guidance'
    Assert-Runner -Base $baseline -Branch 'main' -Success $false -Expected 'refusing protected-path commit on main'

    $null = Invoke-Git -Arguments @('checkout', '--quiet', '--force', '-B', 'side', $baseline)
    Write-File -Path 'src/side.ps1' -Content 'Write-Output 1'
    $null = New-Commit -Message 'chore: add side command'
    $null = Invoke-Git -Arguments @('checkout', '--quiet', '-B', 'feature', $baseline)
    Write-File -Path 'src/feature.ps1' -Content 'Write-Output 2'
    $null = New-Commit -Message 'chore: add feature command'
    $null = Invoke-Git -Arguments @('merge', '--quiet', '--no-ff', 'side', '-m', 'chore: merge side branch')
    Assert-Runner -Base $baseline -Expected 'Git hooks passed for 3 PR commit(s).'

    Write-Host 'All PR hook runner tests passed.'
} finally {
    Set-Location -LiteralPath $location.Path
    if (Test-Path -LiteralPath $scratch) {
        $resolved = (Resolve-Path -LiteralPath $scratch).Path
        if ([IO.Path]::GetFullPath($resolved) -ne $scratch -or
            [IO.Path]::GetDirectoryName($resolved) -ne $tempRoot.TrimEnd('\', '/')) {
            throw 'Refusing to remove an unexpected test directory.'
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
