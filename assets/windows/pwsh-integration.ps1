# PowerShell shell integration (terminal-prompt-placement /
# shell-integration-prompt-hiding).
#
# Wraps the prompt and the line reader to emit FinalTerm OSC 133 marks (OSC + BEL,
# ConPTY-safe) so our PTY sniffer can:
#   - capture the prompt (;A -> ;B) for the fixed prompt dock, and
#   - bound the prompt/echo region (;A -> ;C) it withholds while prompt-hiding.
#
# Dot-sourced at session start via `-NoExit -Command ". '<script>'"`. Benign when
# prompt-hiding is off: the engine ignores the unknown OSC, so the grid is
# unchanged. Prompt and command marks install together behind a PSReadLine check so
# a shell without PSReadLine emits no marks (region stays None, nothing withheld)
# rather than `;A/;B` without `;C` (which would strand output in the hidden region).

if (Get-Module PSReadLine) {
    $Global:__YtOriginalPrompt = $function:prompt
    $Global:__YtRanCommand = $false
    function Global:prompt {
        # Exit status of the command this prompt follows, read FIRST: any later
        # statement (the $osc7 -eq below resets $?) would clobber it. $? decides
        # success; $LASTEXITCODE supplies the number for native failures. A failing
        # cmdlet leaves $LASTEXITCODE stale/unset, so map it to the sentinel 1.
        $success = $?
        $lastExit = $global:LASTEXITCODE
        $code = if ($success) { 0 } elseif ($lastExit -is [int] -and $lastExit -ne 0) { $lastExit } else { 1 }
        $e = [char]27
        $b = [char]7
        # OSC 7 reports the cwd (git status indicator/sidebar retargeting);
        # only filesystem locations, so Registry:: etc. don't emit bogus paths.
        $osc7 = ''
        if ($pwd.Provider.Name -eq 'FileSystem') {
            $osc7 = "$e]7;file:///$($pwd.ProviderPath -replace '\\','/')$b"
        }
        # Boundary protocol (always on since split blocks became authoritative):
        # clear the host screen after each non-empty command so ConPTY's cursor
        # row resets with the engine's per-block clear. Without this, PSReadLine
        # echoes at ConPTY's ever-growing absolute row -> blank rows pile up
        # above the input in every new block.
        $clearAtBoundary = ($Global:__YtRanCommand -eq $true)
        $Global:__YtRanCommand = $false
        # The prompt is proof no full-screen program is running: after a real
        # command, leave the alternate screen in case it died inside one
        # (vtebench Ctrl-C, killed vim) — otherwise conhost never emits the
        # exit, the engine's ALT_SCREEN latches on, and block mode can't
        # re-engage. Placement is load-bearing: the ?1049l must come AFTER the
        # ;D mark (conhost's ?1049l restores the cursor saved at the last
        # ?1049h even when already on the main buffer; before ;D that yanked
        # the cursor above the command's final output and the harvest kept
        # only vtebench's "Results:" line) and BEFORE the boundary clear,
        # which immediately re-homes whatever the restore did to the cursor.
        $clear = if ($clearAtBoundary) { "$e[?1049l$e[2J$e[3J$e[H" } else { '' }
        # ;D;<code> ends the previous command's output region carrying its exit
        # status, ;A starts the prompt, ;B ends it (command input begins).
        "$osc7$e]133;D;$code$b$clear$e]133;A$b" + (& $Global:__YtOriginalPrompt) + "$e]133;B$b"
    }

    # `clear`/`cls` are aliases of Clear-Host. Under the boundary protocol the
    # engine's scrollback is always empty, so the terminal cannot infer a user
    # clear from a history collapse — announce it in-band (`;K`) BEFORE the
    # actual erase so the terminal drops its frozen history in sync.
    # Braces are required: a hyphenated name ends the bare $function: token.
    $Global:__YtOriginalClearHost = ${function:Clear-Host}
    function Global:Clear-Host {
        $e = [char]27
        $b = [char]7
        [Console]::Write("$e]133;K$b")
        & $Global:__YtOriginalClearHost
    }

    $Global:__YtOriginalReadLine = $function:PSConsoleHostReadLine
    function Global:PSConsoleHostReadLine {
        $line = & $Global:__YtOriginalReadLine
        $Global:__YtRanCommand = -not [string]::IsNullOrWhiteSpace($line)
        $e = [char]27
        $b = [char]7
        # ;C marks the transition from command input to command output.
        [Console]::Write("$e]133;C$b")
        $line
    }

    # Synthetic empty ;A;B;C cycle at session start: the first real prompt's
    # leading ;D then completes an ordered A->B->C->D lifecycle, so the sniffer
    # grants boundary trust (and the fixed-bottom dock engages) at the first
    # prompt instead of after the first Enter.
    $e = [char]27
    $b = [char]7
    [Console]::Write("$e]133;A$b$e]133;B$b$e]133;C$b")
}
