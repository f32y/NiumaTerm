## Context

See proposal.md - Why. The constraints that shape the approach:

- **The composer is plain text.** The Agent composer is a `gpui-component`
  `InputState`; it stores a string with no span or element model. An
  attachment can therefore only be tied to the text by a literal placeholder,
  which is also what both upstream harnesses do.
- **The two harnesses accept images differently.** Codex's app server takes
  `{"type":"localImage","path":...}` in a turn's input array and reads the file
  itself. Claude Code's stream-json takes an Anthropic content block with
  base64 bytes inline. Neither accepts the other's shape.
- **gpui already covers the platform work.** `gpui_windows`'s clipboard reads
  `CF_DIB`, `CF_HDROP`, and the PNG/JPEG/GIF clipboard formats, returning
  `ClipboardEntry::Image` with encoded bytes; `img(Arc<gpui::Image>)` renders
  encoded bytes with no file on disk.
- **Both send paths already carry arrays.** Codex's input array already holds a
  `skill` item beside `text`; Claude's content is already a block array. Adding
  an item to each needs no protocol reshaping.

## Goals / Non-Goals

**Goals:**

- One attachment model in the composer, translated per harness at the last
  possible layer.
- The composer text and the thumbnail strip can never disagree about what will
  be sent.
- No temporary file is written for anything but the harness that requires one.

**Non-Goals:**

- Attaching by file picker or drag-and-drop. Paste is the entry point; other
  entry points can reuse the same attachment model later.
- Non-image attachments. Claude Code also pastes long text as a
  `[Pasted text #N]` placeholder; that is a separate capability sharing this
  mechanism.
- Editing an attachment (crop, annotate, reorder).
- DeepSeek. Its harness has no image input, so a paste there is refused.

## Decisions

### Each harness gets images in its own native shape

Codex receives file paths, Claude receives base64. The alternative - one shape
converted at the boundary - fails in both directions: Codex's `localImage` has
no field for bytes, and Claude's stream-json has no path input. An attachment
therefore holds encoded bytes as its truth and can produce a path on demand,
which only Codex's path asks for.

### The placeholder is the anchor, and numbering is normalized after every edit

`[Image #N]` is inserted at the caret on paste, and after any change to either
side - removing from the strip, deleting the text by hand - the placeholders are
renumbered to stay consecutive from 1. Gaps are the alternative and are worse:
`[Image #1]` and `[Image #3]` reads as a lost image, and the number is what
tells a reader which thumbnail a sentence is about.

Codex does exactly this (`relabel_local_images`), which is a useful signal that
the churn is tolerable in practice.

### Synchronization runs from the text, not from the strip

After every composer edit, the placeholders present in the text are the
authority: an attachment whose placeholder is gone is dropped. Removing from
the strip is implemented as "delete the placeholder, then reconcile", so both
directions share one code path and there is one rule to reason about.

The alternative - treating the strip as authoritative and rewriting the text to
match - would have to guess where a re-added placeholder belongs in a sentence
the user has since edited.

### Attachments are held by the pane, cleared on accepted send

The pending list lives beside the composer state in `AgentPane`, so it follows
the tab's lifetime and is dropped with it. A submission the harness refuses
keeps its attachments, matching how refused text stays in the composer.

### Temporary files are written at send time and cleaned with the tab

Only the Codex path needs a file. Writing at send rather than at paste means an
attachment that is removed before sending never touches the disk. Files are
written under a per-tab directory removed when the tab closes.

Codex itself writes at paste time and never deletes, because its app server may
re-read a rollout later. NiumaTerm holds the transcript itself, so it does not
need the file to outlive the turn - but it must outlive the *request*, which is
why the file is not deleted immediately after the send call returns.

### Thumbnails render from bytes, not from files

`gpui::Image::from_bytes` plus `img()` renders the attachment the user pasted
with no disk round-trip, in both the strip and the transcript. This is why the
Codex temp file is a send-time concern only.

### Bounds are enforced at paste

8 attachments per message, 2048 pixels on the long edge. Downscaling at paste
keeps the bound on one path and means the thumbnail, the transcript image, and
the bytes sent are the same data. A 4K screenshot is roughly 8 MB as PNG, and
Claude's path puts that on one stdin line as base64; the cap keeps a message
that a harness would choke on from being composable at all.

## Risks / Trade-offs

- **Renumbering churns the composer text while the user types** → Reconcile
  only when the set of placeholders actually changes, not on every keystroke.
- **A placeholder can be typed by hand** (`[Image #1]` with nothing attached)
  → The text is the authority for *removal* only; a placeholder naming no
  attachment is left as literal text and sends as text.
- **Claude's base64 makes one very large stdin line** → The 2048-pixel cap
  bounds a single image; the 8-image cap bounds the message. Neither is a
  guarantee against a pathological PNG, which is accepted.
- **A harness with no image input** (DeepSeek) → Paste is refused there with a
  message, rather than silently attaching something that cannot be sent.
- **The transcript holds decoded images for the life of the tab** → Thumbnails
  are bounded by the same caps, and a conversation's images are already bounded
  by what the harness accepted.

## Open Questions

- Whether a resumed conversation's replay can recover the images a message
  carried, or whether such a message renders text-only. The spec requires only
  that it render without error, so this can be answered when replay is
  extended.
