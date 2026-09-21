# NiumaTerm shell integration for bash: FinalTerm OSC 133 marks.
#
# The terminal reads these to bound the prompt (`;A` -> `;B`), the echoed
# command line (`;B` -> `;C`) and the command's output (`;C` -> `;D <exit>`).
# An ordered A->B->C->D lifecycle is what earns boundary trust, which is what
# turns a session into command blocks with a fixed prompt dock; without the
# marks the terminal falls back to heuristic prompt sniffing.
#
# The terminal starts bash with `--norc --noprofile` and
# `HISTCONTROL=ignorespace`, then puts one leading-space `source` of this file
# into the terminal's input queue before the shell exists. So this file owns
# startup: it replays the user's own startup files in bash's order and installs
# its hooks last — after everything, which is the point.
#
# Unlike zsh, the injected line cannot be hidden. Suppressing it means starting
# without a line editor, and bash 3.2 cannot put one back: `set -o emacs` flips
# the option, but readline was never initialized at startup and the shell stops
# printing prompts altogether. So readline stays on, readline echoes the line,
# and the screen is cleared below instead — which also takes the login banner
# with it.
#
# Written for bash 3.2, which is the version macOS ships.
#
# Deliberately absent, unlike the PowerShell integration: no screen clear at
# the block boundary and no `?1049l` alternate-screen recovery. The terminal
# clears its own grid when it freezes a block, and there is no second host-side
# grid here to keep in step. Leaving the alternate screen would be actively
# wrong under job control: a suspended full-screen program is sitting at a
# prompt with the alternate screen still its own, and `fg` must find it intact.

# First, before anything can print: drop the echoed bootstrap line, the prompt
# it was typed at, and the login banner above them. Scrollback goes too, since
# the line may have wrapped and there is nothing worth keeping from before the
# session started.
printf '\033[H\033[2J\033[3J'

# The leading space on the `source` line kept it out of history. That setting
# existed for that one line and the decision has already been taken, so the
# value the session would otherwise have had is restored before the user's own
# files can have an opinion about it.
if [ -n "${NMT_SAVED_HISTCONTROL+x}" ]; then
  HISTCONTROL=$NMT_SAVED_HISTCONTROL
  export HISTCONTROL
else
  unset HISTCONTROL
fi
unset NMT_SAVED_HISTCONTROL

# Replay the startup sequence `--norc --noprofile` suppressed, in bash's order.
# `shopt -q login_shell` is the shell's own answer, and it is the shell the
# user actually gets — there is no `exec` between here and them, so functions,
# aliases and traps defined below reach the session intact.
#
# Sourced at the top level rather than from a helper function: a `local` in the
# user's own files has to reach the shell, not a function scope.
if shopt -q login_shell; then
  [ -r /etc/profile ] && . /etc/profile
  for __nmt_profile in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$__nmt_profile" ]; then
      . "$__nmt_profile"
      break
    fi
  done
  unset __nmt_profile
else
  # `/etc/bash.bashrc` is only in the startup sequence when bash was compiled
  # with SYS_BASHRC, which Debian and its derivatives do and there is no
  # efficient way to test for. Sourcing it when it exists matches the systems
  # that ship one; systems without the file are unaffected.
  [ -r /etc/bash.bashrc ] && . /etc/bash.bashrc
  [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi

# Set before the DEBUG trap is installed so the rest of this file, and the
# first prompt's own hooks, are not mistaken for a user command.
__nmt_in_prompt=1

# The terminal strips `file://<host>` and takes the remainder verbatim, so the
# path travels unencoded — percent-encoding would arrive literal.
__nmt_report_cwd() {
  printf '\033]7;file://%s%s\007' "$HOSTNAME" "$PWD"
}

# Runs first in PROMPT_COMMAND: `$?` is the finished command's status and any
# other hook would overwrite it.
__nmt_precmd() {
  local exit_code=$?

  __nmt_in_prompt=1
  # The number the next saved line will get; `__nmt_preexec` compares it to
  # tell a freshly saved line from a stale history entry.
  __nmt_hist_next=$HISTCMD

  if [ -z "$__nmt_primed" ]; then
    __nmt_primed=1
    # An empty A->B->C cycle, so the `;D` below completes an ordered lifecycle
    # and boundary trust is granted at this first prompt instead of after the
    # first command.
    printf '\033]133;A\007\033]133;B\007\033]133;C\007'
  fi

  # `;D` closes the previous command's output region carrying its status,
  # `;A` opens the prompt.
  printf '\033]133;D;%s\007' "$exit_code"
  __nmt_report_cwd
  printf '\033]133;A\007'
}

__nmt_prompt_end_mark='\[\033]133;B\007\]'

# Runs last in PROMPT_COMMAND: a prompt framework rebuilds PS1 from its own
# hook, so the mark is re-applied per prompt rather than appended once. `\[\]`
# tells bash the sequence occupies no columns, keeping the prompt's width
# arithmetic correct.
__nmt_prompt_ready() {
  # Any earlier copy is stripped first: a framework that rebuilt PS1 around one
  # would otherwise leave it stranded mid-prompt, ending the prompt region
  # before the prompt does.
  PS1="${PS1//"$__nmt_prompt_end_mark"/}$__nmt_prompt_end_mark"

  # The prompt is drawn after this hook, so anything the user runs next is
  # theirs.
  __nmt_in_prompt=
}

# A DEBUG trap the user's own files installed — bash-preexec, atuin — is
# captured here, because bash allows one handler per signal and installing ours
# would otherwise silently replace theirs. `trap -p` prints the handler wrapped
# as `trap -- '<command>' DEBUG`; stripping only that wrapper leaves the
# command with bash's own quoting intact, which is what `eval` re-parses.
__nmt_previous_debug_trap=$(trap -p DEBUG)
__nmt_previous_debug_trap=${__nmt_previous_debug_trap#trap -- \'}
__nmt_previous_debug_trap=${__nmt_previous_debug_trap%\' DEBUG}

# `;C` — command input ends, its output begins. DEBUG fires before every simple
# command, including the ones the prompt hooks and the command's own pipeline
# run, so only the first one after a prompt is the user's.
__nmt_preexec() {
  # Theirs runs first and unconditionally: it is entitled to every command we
  # filter out, and it sees `$BASH_COMMAND` as its own trap would have.
  if [ -n "$__nmt_previous_debug_trap" ]; then
    eval "$__nmt_previous_debug_trap"
  fi

  if [ -n "$__nmt_in_prompt" ]; then
    return
  fi
  __nmt_in_prompt=1
  # DEBUG also fires before the first prompt hook after an empty line. An
  # empty `cmdline` tells the terminal nothing ran, so it builds no block.
  if [ "$BASH_COMMAND" = __nmt_precmd ]; then
    printf '\033]133;C;cmdline=\007'
    return
  fi

  # `;C` carries the accepted line so the block title does not depend on the
  # screen echo. `$BASH_COMMAND` is only the first simple command of the line
  # (`a | b` gives `a`); the whole line is the newest history entry, but only
  # when the line was saved. Inside the trap `$HISTCMD` equals the number
  # recorded at the prompt exactly then; `ignorespace` and `ignoredups` leave
  # the previous entry on top, and a bare `;C` lets the terminal title the
  # block from the echo instead. The two subshells cost about a millisecond
  # per command; a pure-shell base64 would be forty lines to save that.
  local cmdline=
  if [ "$HISTCMD" = "$__nmt_hist_next" ]; then
    cmdline=$(HISTTIMEFORMAT= builtin history 1)
    # `history` prints `%5d%c %s`: the number, `*` or a space, a space, the line.
    cmdline=${cmdline#*$HISTCMD}
    # `tr` folds the wrapping GNU base64 adds; BSD base64 emits one line already.
    cmdline=$(printf '%s' "${cmdline:2}" | base64 | tr -d '\n')
  fi
  # The terminal scans a mark of at most 16 KiB; a longer line only loses its
  # title.
  if [ -n "$cmdline" ] && [ "${#cmdline}" -le 16000 ]; then
    printf '\033]133;C;cmdline=%s\007' "$cmdline"
  else
    printf '\033]133;C\007'
  fi
}

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

PROMPT_COMMAND="__nmt_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; __nmt_prompt_ready"

trap '__nmt_preexec' DEBUG
