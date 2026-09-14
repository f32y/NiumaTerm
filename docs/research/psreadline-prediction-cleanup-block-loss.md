# PSReadLine prediction cleanup drops command blocks

Updated: 2026-09-14. Implementation baseline: `a90c9978`.

With PSReadLine's history prediction in `ListView` mode, a command such as
`ls` ran, printed its output, and then vanished from the block list once the
next prompt appeared. Three captured logs showed the same path, and a minimal
counterexample confirmed it: the same `ls`, with one extra erase of the row
below the input, went from one saved block to zero.

This document records the mechanism, the fix, the shell-side protocol the fix
introduced, and the alternatives that were considered and rejected.

## Background: how a command block is captured

The terminal reads the PTY byte stream once, in `PromptSniffer`
(`crates/terminal/src/prompt_sniffer.rs`), before the bytes reach the engine.
The sniffer recognizes the FinalTerm OSC 133 lifecycle marks the integration
scripts emit:

| Mark | Meaning | Region that follows |
|---|---|---|
| `ESC ] 133 ; A BEL` | prompt starts | `Prompt` |
| `ESC ] 133 ; B BEL` | prompt ends, user input begins | `Command` |
| `ESC ] 133 ; C BEL` | input accepted, command output begins | `Output` |
| `ESC ] 133 ; D ; <exit> BEL` | command finished | `None` |

An ordered `A -> B -> C -> D` cycle earns boundary trust. Once trusted, each
`;D` produces a `CommandCapture`: the block's metadata (command text, exit
code, launch directory, timestamps). The block store marries that metadata to
the rows the engine froze for the same sequence number, and the block list
draws a header above the rows, for example `ls · ✓ 12ms`.

The command text never came from the shell. The sniffer accumulated the raw
bytes of the `Command` region, everything between `;B` and `;C`, and at `;C`
ran `render_command_echo` over them. That function emulates a single
terminal line: printable characters land at the cursor column, `CR`, `CUP`,
`CHA`, `CUF` and `CUB` move the cursor, `EL` and `ECH` erase. The emulation
exists because PSReadLine redraws the input on nearly every keystroke for
syntax highlighting, and ConPTY reprojects each redraw at absolute columns, so
a plain concatenation of printables would repeat the line once per redraw.

The emulation has a documented ceiling: it has no notion of rows. Every
cursor movement and erase is applied to the one emulated line, whatever row
the real cursor is on.

## Root cause

Before the fix, the emulated text did two jobs. It was the block title, and it
was also the gate that decided whether a block existed at all:

- at `;C`, `command_started_edge` required a non-empty echo, so an empty echo
  meant no in-flight block;
- at `;D`, the capture was skipped when `command.is_empty()`.

The gate was there for a real reason. An empty Enter also runs a full
`A -> B -> C -> D` cycle, carries no useful metadata, and Warp builds no block
for it either. Reading the echo was the only way the terminal had to tell an
empty Enter from a real command.

PSReadLine's `ListView` prediction draws candidate rows below the input. When
the user accepts a line, PSReadLine erases those rows before the shell runs
the command. The erase is ordinary: move the cursor to the next row, `EL`, move
back. Collapsed onto one emulated line, that sequence reads as "erase the
input from column 0", and the echo for `ls` becomes the empty string.

The chain then completes on its own:

1. `;C` arrives with an empty echo, so no in-flight block starts.
2. `ls` prints its output into the `Output` region as usual.
3. `;D` arrives; the capture is skipped because the command text is empty.
4. The integration script clears the screen for the next prompt, which is the
   normal block-boundary behavior. With no block to freeze the rows into, the
   output is gone.

The minimal counterexample is the exact byte sequence the sniffer's
regression test now feeds:

```text
ESC ] 133;A BEL  PS>  ESC ] 133;B BEL
ESC [ 1;5H  ls               input on row 1
ESC [ 2;1H  ESC [ K          prediction row erased on row 2
ESC [ 1;7H  CR LF
ESC ] 133;C BEL  Cargo.toml CR LF  ESC ] 133;D;0 BEL
ESC [ 2J ESC [ 3J ESC [ H    prompt-side clear
```

Without the `ESC [ 2;1H ESC [ K` pair, one block is saved. With it, none was.

## Fix

The fix separates the two jobs the echo used to do.

**An estimate never decides whether output is kept.** The empty-text
condition is gone from both the `;C` edge and the `;D` capture. A trusted,
completed cycle always produces a `CommandCapture`. Its `command` field is now
`Option<String>`; when the shell reported nothing and the echo reads as
empty, the block is saved with no title rather than not at all. The header
renderer already treated a missing command as "draw no header", so the block
keeps its rows, its exit-code accent, and its place in history.

**The shell reports the accepted line on the `;C` mark.** The `;C` mark may
now carry one parameter:

```text
ESC ] 133 ; C ; cmdline=<base64 of the UTF-8 line> BEL
```

Either terminator works, `BEL` or `ESC \`. The sniffer decodes the value once,
when it parses the mark; the output byte stream is never touched. The decoded
text has three readings:

| `cmdline` value | Block | Title |
|---|---|---|
| absent (legacy `;C`) | saved | echo estimate, or none if it reads empty |
| present, decodes, whitespace-only after trim | not saved | n/a: the shell says nothing ran |
| present, decodes, non-empty | saved | the decoded text |
| present but undecodable | saved | echo estimate, or none |

An undecodable value counts as absent on purpose. Nothing about keeping the
block may depend on a parse succeeding; a broken shell script can cost a
title, never output.

Only the shell's own report can say "nothing ran". That is the one bit the old
gate was approximating from the screen, and it is the one bit an estimate
cannot supply safely.

### Shell integrations

**PowerShell** (`assets/windows/pwsh-integration.ps1`). The
`PSConsoleHostReadLine` wrapper already sits at the moment the line is
accepted. It base64-encodes `[string]$line` and emits `;C;cmdline=<value>`.
An empty line encodes to an empty value, which is how the terminal learns no
command ran. Above 16000 encoded characters the wrapper emits a bare `;C`
instead; the title then degrades to the echo estimate and nothing else changes.

**zsh** (`assets/unix/zsh/nmt-integration.zsh`). `preexec` receives the
accepted line as typed in `$1`. It pipes the line through `base64`, folds the
line wrapping GNU `base64` adds with `tr -d '\n'` (BSD `base64` on macOS emits
one line already), and applies the same size cut-off. The empty-line path in
`precmd`, which closes a `;B` that `preexec` never followed, now emits
`;C;cmdline=` so an abandoned or empty line produces no block.

**bash** (`assets/unix/bash/nmt-integration.bash`). bash has no `preexec`; the
integration uses a `DEBUG` trap, and inside that trap `$BASH_COMMAND` is only
the first simple command of the line (`a | b` yields `a`). The whole line is
the newest history entry, but that entry is stale whenever the line was not
saved: `HISTCONTROL=ignorespace` for a leading-space line, `ignoredups` for a
repeat. The trap tells the two apart with `$HISTCMD`:

- `__nmt_precmd` records `__nmt_hist_next=$HISTCMD`, the number the next saved
  line will receive;
- inside the trap, `$HISTCMD` equals that recorded value exactly when the line
  was saved, and equals the previous entry's number otherwise.

When the numbers match, the trap reads `HISTTIMEFORMAT= builtin history 1`,
strips the `%5d%c %s` prefix (number, `*` or space, space), and encodes the
rest. When they differ, it emits a bare `;C`, and the title comes from the
echo, which readline draws plainly enough for the estimate to be right. The
empty-line case is detected as before: the first `DEBUG` after an empty Enter
fires for `__nmt_precmd` itself.

This behavior was checked in an interactive bash 5.3 with
`HISTCONTROL=ignorespace:ignoredups`, decoding every `;C` mark the script
emitted:

| Input | Mark emitted |
|---|---|
| `echo one` | `cmdline=echo one` |
| `echo two \| cat` | `cmdline=echo two \| cat` (whole pipeline) |
| ` echo hidden` (leading space) | bare `;C` |
| `echo two \| cat` again | bare `;C` |
| empty Enter | `cmdline=` (empty) |
| a quoted two-line command | both lines, newline preserved |

Cost, bash and zsh: two subshells plus `base64` and `tr` per accepted line,
about a millisecond on Linux, and only on Enter. A pure-shell base64 would be
around forty lines to save that. PowerShell uses the in-process
`[Convert]::ToBase64String`.

### Sniffer changes

`parse_sniffed_osc` reads the `;cmdline=` argument the same way it already
read `;D;<exit>`. The `;C` mark may now be up to 16 KiB; every other mark keeps
the 32-byte limit, so a malformed or foreign mark still resyncs after 32 bytes.

A mark split across two PTY reads is held in a carry buffer. That buffer was a
32-byte inline array; it is now a `Vec<u8>` that allocates once and keeps its
capacity across `clear()`. The change let `PromptSniffer` derive `Default`
again and removed two stack copies from the carry path. The only hot-path
difference is the carry's upper bound: a read that ends in an `ESC` now copies
up to 16 KiB of the following read into the carry before parsing, instead of
32 bytes. That is a memcpy of about a microsecond on a read the engine spends
tens of microseconds parsing, and it happens only for reads that end in
`ESC`.

Why base64 rather than the raw line: the value lives inside an OSC string, so
it cannot contain `ESC`, `BEL` or, safely, any control byte, and multi-line
input contains newlines. base64 is already a dependency of the terminal
crate, costs one call on each side, and keeps the payload pure ASCII, which
also removes any question of how ConPTY re-encodes non-ASCII bytes inside an
OSC it passes through.

## Alternatives considered

**Teach `render_command_echo` about rows.** Tracking `CUD`, `CUU`, `LF` and
`CNL` so that an erase on another row does not touch the emulated line would
have made this one sequence render correctly, in perhaps fifteen lines. It
would have left the estimate as the gate for keeping output. The next
unforeseen redraw pattern would drop blocks the same way, and the ceiling the
function documents (wrapped multi-row input self-overwrites) would remain.
Fixing the specific rendering was rejected in favor of removing the estimate
from the decision.

**A dedicated `;N` mark with fragmented base64.** The first version of the fix
introduced a new mark, `ESC ] 133 ; N ; 1 ; <total> ; <offset> ; <part> BEL`,
sent in 1024-byte fragments, with a four-state assembler (`Missing`,
`Receiving`, `Complete`, `Invalid`), contiguity checks on offsets, a recovery
path that skipped a corrupt fragment to the next `ESC`, and a 1088-byte fixed
carry buffer that forced a hand-written `Default`. It fixed the bug because it
also removed the empty-text gate; its own regression test contained no `;N`
mark at all and passed on the gate removal alone. The protocol served only the
title, only PowerShell sent it, and it added roughly 150 lines of parser
surface. It was replaced by the single `cmdline=` parameter on the existing
`;C` mark: no new mark type, no fragments, no ordering state, and a cap
instead of an assembler.

**Send only an "empty" flag, never the text.** The smallest possible shell
change would emit `;C;cmdline=` for an empty line and a bare `;C` otherwise.
It fixes the bug and prevents empty blocks, but under `ListView` every block
would have no title, which is exactly the situation the user is in. Sending
the text costs one encoded call per Enter, so it was kept.

## Known ceilings

- A `;C` mark longer than 16 KiB is malformed and costs one trust cycle. The
  three integration scripts omit `cmdline` above 16000 encoded characters so
  this cannot happen with the shipped scripts.
- Third-party or older integration scripts emit a bare `;C` for an empty
  Enter. Those sessions now save a block for it: no title, no output rows,
  an exit-code accent. Previously the empty Enter was silently skipped.
- bash falls back to the echo estimate for lines `HISTCONTROL` did not save.
- The echo estimate keeps its original ceiling for legacy `;C` marks: wrapped
  input can self-overwrite. It now affects only the title.
- zsh was reviewed but not executed; no zsh was available on the development
  machine.

## Verification

- `prompt_sniffer_tests`: the erased-echo sequence above keeps its block at
  every split point of the input; a reported empty or whitespace-only line
  starts no execution; an unreadable `cmdline` falls back to the echo title;
  a unicode, multi-line reported line survives every one- and two-chunk split
  of the stream with both terminators, and the engine receives the stream
  unchanged.
- `ghostty_mirror_tests`: with the real engine, the PSReadLine sequence
  produces one block whose rows contain the output, and a `;C` mark that
  carries `cmdline=` is accepted by the engine's own OSC parser, so the output
  rows keep their semantic tag (block count 1).
- `session/psreadline_tests` (Windows only): a live PowerShell 7 with
  PSReadLine `ListView` prediction runs `ls` three times; after each prompt
  clear all blocks remain, each titled `ls` and containing the directory
  listing. An empty Enter and a Ctrl-C-abandoned line add no block. A 900+
  character multi-line unicode line is reported intact.
- bash: the interactive run tabulated above.
- The full `nmt_terminal` suite passes (195 tests).
