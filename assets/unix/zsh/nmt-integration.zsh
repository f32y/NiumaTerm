# NiumaTerm shell integration for zsh: FinalTerm OSC 133 marks.
#
# The terminal reads these to bound the prompt (`;A` -> `;B`), the echoed
# command line (`;B` -> `;C`) and the command's output (`;C` -> `;D <exit>`).
# An ordered A->B->C->D lifecycle is what earns boundary trust, which is what
# turns a session into command blocks with a fixed prompt dock; without the
# marks the terminal falls back to heuristic prompt sniffing.
#
# The terminal starts zsh with NO_RCS, no line editor and HIST_IGNORE_SPACE,
# then puts one leading-space `source` of this file into the terminal's input
# queue before the shell exists. So this file owns startup: it restores what
# those launch flags suppressed, replays the user's own startup files in zsh's
# order, and installs its hooks last — after everything, which is the point.
#
# Deliberately absent, unlike the PowerShell integration: no screen clear at
# the block boundary and no `?1049l` alternate-screen recovery. The terminal
# clears its own grid when it freezes a block, and there is no second host-side
# grid here to keep in step. Leaving the alternate screen would be actively
# wrong under job control: a suspended full-screen program is sitting at a
# prompt with the alternate screen still its own, and `fg` must find it intact.

# The leading space on the `source` line kept it out of history. That option
# existed for that one line and the decision has already been taken, so the
# session gets the setting back before the user's own files can have an
# opinion about it.
unsetopt histignorespace

# The line editor was off so the line discipline governed the echo of the
# bootstrap line. It has to come back before the user's files run: their
# `bindkey` and `zle -N` calls configure an editor that has to exist.
setopt zle

# Replay the startup sequence NO_RCS suppressed, in zsh's own order. Only
# `/etc/zshenv` still ran, so `$ZDOTDIR` already holds whatever it chose, and a
# user who sets `ZDOTDIR` from their own `.zshenv` is picked up by re-reading
# it between files. `.zprofile` and `.zlogin` belong to login shells alone.
#
# Sourced at the top level rather than from a helper function: a `typeset` or
# `local` in the user's own files has to reach the shell, not a function scope.
__nmt_zdotdir=${ZDOTDIR:-$HOME}
[[ -r $__nmt_zdotdir/.zshenv ]] && source $__nmt_zdotdir/.zshenv

__nmt_zdotdir=${ZDOTDIR:-$HOME}
if [[ -o login ]]; then
  [[ -r /etc/zprofile ]] && source /etc/zprofile
  [[ -r $__nmt_zdotdir/.zprofile ]] && source $__nmt_zdotdir/.zprofile
  __nmt_zdotdir=${ZDOTDIR:-$HOME}
fi

[[ -r /etc/zshrc ]] && source /etc/zshrc
[[ -r $__nmt_zdotdir/.zshrc ]] && source $__nmt_zdotdir/.zshrc

__nmt_zdotdir=${ZDOTDIR:-$HOME}
if [[ -o login ]]; then
  [[ -r /etc/zlogin ]] && source /etc/zlogin
  [[ -r $__nmt_zdotdir/.zlogin ]] && source $__nmt_zdotdir/.zlogin
fi
unset __nmt_zdotdir

autoload -Uz add-zsh-hook

# The terminal strips `file://<host>` and takes the remainder verbatim, so the
# path travels unencoded — percent-encoding would arrive literal.
__nmt_report_cwd() {
  printf '\033]7;file://%s%s\007' "$HOST" "$PWD"
}

__nmt_prompt_end_mark=$'%{\033]133;B\007%}'

__nmt_precmd() {
  # Read first: any later command overwrites $?.
  local exit_code=$?

  if [[ -z "$__nmt_primed" ]]; then
    __nmt_primed=1
    # An empty A->B->C cycle, so the `;D` below completes an ordered lifecycle
    # and boundary trust is granted at this first prompt instead of after the
    # first command. Emitted here rather than while `.zshrc` runs, where a
    # prompt framework's instant prompt would flag the output.
    printf '\033]133;A\007\033]133;B\007\033]133;C\007'
  elif [[ -z "$__nmt_command_started" ]]; then
    # An empty line, or one abandoned with Ctrl-C, runs no command, so
    # `preexec` never fired and the command region opened by the last `;B` is
    # still open. Close it here: a `;D` arriving straight after a `;B` is an
    # out-of-order lifecycle and costs the terminal its boundary trust. The
    # empty `cmdline` says nothing ran, so the terminal builds no block.
    printf '\033]133;C;cmdline=\007'
  fi
  __nmt_command_started=

  # `;D` closes the previous command's output region carrying its status,
  # `;A` opens the prompt.
  printf '\033]133;D;%s\007' "$exit_code"
  __nmt_report_cwd
  printf '\033]133;A\007'

  # The prompt ends with `;B`. Re-applied per prompt rather than appended once
  # at load because a prompt framework rebuilds PS1 in its own `precmd`; this
  # hook is registered last, so it sees the final PS1 for this prompt. Any
  # earlier copy is stripped first: a framework that rebuilt PS1 around one
  # would otherwise leave it stranded mid-prompt, ending the prompt region
  # before the prompt does. `%{%}` tells zsh the sequence occupies no columns,
  # keeping the prompt's width arithmetic and right-prompt placement correct.
  PS1="${PS1//"$__nmt_prompt_end_mark"/}$__nmt_prompt_end_mark"

  # zsh draws the right prompt after the left one, so its bytes land after the
  # `;B` above and would be captured as part of the command the user typed.
  # Closing RPROMPT with a second `;B` re-opens the command region past them:
  # the terminal clears the echo it has accumulated at every `;B`, so what
  # survives is what was typed after the whole prompt. Left alone when there is
  # no right prompt, so an empty one is not conjured into existence.
  if [[ -n "${RPROMPT-}" ]]; then
    RPROMPT="${RPROMPT//"$__nmt_prompt_end_mark"/}$__nmt_prompt_end_mark"
  fi
}

# `;C` — command input ends, its output begins. It carries the accepted line
# (`$1`, as typed) so the block title does not depend on the screen echo. The
# terminal scans a mark of at most 16 KiB; a longer line only loses its title.
# `tr` folds the wrapping GNU base64 adds; BSD base64 emits one line already.
__nmt_preexec() {
  __nmt_command_started=1
  local cmdline
  cmdline=$(printf '%s' "$1" | base64 | tr -d '\n')
  if (( ${#cmdline} > 16000 )); then
    printf '\033]133;C\007'
  else
    printf '\033]133;C;cmdline=%s\007' "$cmdline"
  fi
}

add-zsh-hook precmd __nmt_precmd
add-zsh-hook preexec __nmt_preexec

# The terminal keeps no scrollback of its own once blocks are authoritative, so
# a user clear is invisible to it unless announced in band. `;K` goes out
# before the erase, so the frozen blocks drop in step with the screen.
__nmt_announce_clear() {
  printf '\033]133;K\007'
}

clear() {
  __nmt_announce_clear
  command clear "$@"
}

__nmt_clear_screen() {
  __nmt_announce_clear
  zle .clear-screen
}

zle -N clear-screen __nmt_clear_screen
