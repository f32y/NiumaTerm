## Context

NiumaTerm already sends Claude Code a `generate_session_title` control request with persistence enabled and translates a successful response into a Tab title update. The pane does not publish its prepared provisional title for Claude, and its history reader recognizes only user `custom-title` metadata. Claude Code now stores model titles as `ai-title` records and resolves user titles ahead of model titles.

The provider operation is preferable to Claude Desktop's private HTTP route because it uses the CLI process and credentials NiumaTerm already owns, writes metadata in Claude Code's native format, and remains usable without a Claude Desktop organization session.

## Goals / Non-Goals

**Goals:**

- Match Claude Desktop's responsive two-stage title presentation.
- Keep automatic naming to the opening prompt of a fresh conversation.
- Make live, history, and resumed-session title choices consistent.
- Preserve user-authored names.

**Non-Goals:**

- Call Claude Desktop's private title endpoint.
- Reimplement Claude Code's model prompt or model selection.
- Change Codex or DeepSeek title generation.
- Introduce a new title metadata format.

## Decisions

### Keep title generation in the Claude Code control channel

The existing `generate_session_title` request remains the generation mechanism. It already runs independently from the primary response, disables unrelated agent features internally, and writes an `ai-title` record when persistence is enabled. Calling the Desktop-only HTTP route would add organization authentication and version-sensitive behavior without improving the stored result.

### Publish the provisional title only after an accepted send

Claude derives its provisional title from the first six normalized words and limits it to 60 characters, using an ellipsis when truncation is required. The pane publishes it only after the backend reports that it accepted the message. The same acceptance marks the conversation as having consumed its one automatic naming attempt, so a null response cannot retitle the conversation from a later prompt.

### Bound title input at the provider adapter

The stream-json session trims the description and keeps at most 2,000 characters before writing the control request. This keeps every caller within the same bound and avoids putting UI-specific conditions into the control parser.

### Follow Claude Code's stored-title precedence

The history reader scans its existing bounded tail window for both title record types. It keeps the newest value of each type, then selects `custom-title`, followed by `ai-title`. If neither is present, the opening-prompt fallback uses the same six-word and 60-character projection as the live provisional title.

### Treat every resume target as already named

Starting a pane with a recovery identity marks its conversation as named. Even an old transcript without title metadata has an opening prompt, so a follow-up is not eligible to become the conversation's title.

## Risks / Trade-offs

- [An `ai-title` record falls outside the bounded tail scan] -> Keep the opening-prompt fallback and rely on Claude Code's repeated session metadata for long transcripts.
- [A title response arrives after a user rename] -> The Tab manager keeps the user title authoritative, while Claude Code resolves `custom-title` ahead of `ai-title` when restoring.
- [A title request receives a very long Unicode prompt] -> Truncate by Rust characters so the request never splits an encoded character.
- [An older Claude Code build does not support title generation] -> Retain the provisional title and do not surface a primary transcript error.

## Migration Plan

No data migration is needed. Existing `custom-title` and `ai-title` records remain unchanged. Rolling back restores the earlier display behavior without altering stored Claude transcripts.
