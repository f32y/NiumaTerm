# Manual Validation Matrix

Run this matrix only after explicit build/launch authorization. Every launch
must use `target\debug\NiumaTerm.exe --testing`.

## Theme and window coverage

| Theme | Narrow | Standard | Wide |
|---|---:|---:|---:|
| Modern Light | Passed | Passed | Passed |
| Modern Dark | Passed | Passed | Passed |
| Modern Gray | Passed | Passed | Passed |
| Ubuntu | Passed | Passed | Passed |

At each size, verify the workspace and pane surfaces share their semantic
border, background, radius, and applicable gutters; the tab strip retains its
independent styling; the compact usage sequence remains in provider/window
order; and no control or pane introduces horizontal overflow.

## Agent transcript and composer

| State | Checks | Result |
|---|---|---|
| Long prose | Paragraphs, headings, lists, and quotes wrap near the preferred measure on a wide pane and shrink in a narrow pane. | Passed |
| Technical Markdown | Code and tables retain the available transcript width and keep their own overflow behavior. | Passed |
| Diff and command output | Expanded details use the shared rail and remain full-width within the transcript. | Passed |
| Many tool rows | Work, run, and completed-turn disclosures share chevron/type/content/status slots; expansion keys remain independent. | Passed |
| Transcript origin | First row has 16px top space and no fade. | Passed |
| Transcript scrolled | A 24px top fade appears without blocking selection, links, disclosure clicks, or scrolling. | Passed |
| Idle composer | Primary Send is 32×32, has an upward glyph, tooltip, accessible name, and dispatches normally. | Passed |
| Running composer | Danger Stop replaces Send in the same slot, has a square glyph, tooltip, accessible name, and interrupts normally. | Passed |
| Thread controls | Model is primary; execution policy and quality/cost controls remain grouped, keyboard reachable, and unchanged on the wire. | Passed |

Also verify text selection, context-menu Copy, tail follow, Jump to latest,
palette keyboard actions, and row-height remeasurement after resizing the pane.

## Pane layout

| Layout | Checks | Result |
|---|---|---|
| Single terminal | One pane-body frame beneath the independent tab strip; no redundant pane card. | Passed |
| Single Agent | One pane-body frame beneath the independent tab strip. | Passed |
| Horizontal split | Internal separator and focused-pane border remain visible and clipped by the pane surface. | Passed |
| Vertical/nested split | Resizing, focus changes, and saved ratios remain intact. | Passed |

## Workspace sidebar

| State | Checks | Result |
|---|---|---|
| Idle | Status slot stays aligned and paints no glyph. | Passed |
| Running | Theme warning progress indicator is visible. | Passed |
| Needs Input | Theme primary/information indicator is visible. | Passed |
| Unread | Count badge remains visible independently of runtime status. | Passed |
| Generated name | `New Workspace` displays the final cwd component; rename still edits and persists the source name. | Passed |
| Long path | Leading elision preserves complete trailing components; tooltip and accessibility expose the full path. | Passed |

## Provider usage

The visual order must always be `Codex icon`, Codex five-hour remaining,
Codex weekly remaining, two-space separator, `Claude icon`, Claude five-hour
remaining, and Claude weekly remaining. No progress bar, Fable value, reset
time, or visible window label is allowed.

| Codex | Claude | Refresh | Expected result | Result |
|---|---|---|---|---|
| Full | Full | Idle | All four percentages remain visible. | Passed |
| Partial | Full | Idle | Missing Codex window is `—`; Claude is unchanged. | Passed |
| Full | Partial | Idle | Missing Claude window is `—`; Codex is unchanged. | Passed |
| Unavailable | Unavailable | Idle | Four stable `—` slots remain visible. | Passed |
| Previous success | Previous success | Refreshing | Values stay in place with non-displacing loading feedback. | Passed |
| Failure | Success | Complete | Last Codex success is retained; new Claude success is applied. | Passed |
| Success | Failure | Complete | New Codex success is applied; last Claude success is retained. | Passed |

For Claude, validate the supplied `/usage` fixture as 3% session remaining and
83% weekly remaining, then separately validate missing CLI, timeout/cancel,
malformed output, and oversized output failure paths.

## Automated verification log

- 2026-08-06: `cargo check` — passed with no errors or warnings.
- 2026-08-06: focused automated checks and the full `--testing` visual matrix
  were confirmed as passed by the user. The externally run test command was
  not captured by the agent.
- Accepted visual tuning: retain the independent tab-strip styling above the
  pane card. The short-transcript Jump to latest regression was corrected by
  requiring a real scroll range and hidden content below the viewport.
