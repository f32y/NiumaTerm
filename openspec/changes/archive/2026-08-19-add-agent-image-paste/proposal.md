## Why

Agent conversations are text-only today: a screenshot of a failing UI, a stack
trace captured as an image, or a design mock has to be described in words or
saved to disk and referenced by path. Both harnesses NiumaTerm drives already
accept images on a turn's input, and gpui already decodes clipboard images on
Windows, so the capability is one composer feature away.

## What Changes

- Pasting an image into the Agent composer attaches it to the pending message
  instead of pasting nothing, and inserts an `[Image #N]` placeholder at the
  cursor so the text records where the image belongs.
- A thumbnail strip above the composer shows every pending attachment, each
  with a remove control. Removing one deletes its placeholder from the composer
  text and renumbers the placeholders that follow.
- Deleting an `[Image #N]` placeholder by editing the composer text drops the
  attachment it named, so the two never disagree about what will be sent.
- Sending a message carries its attachments to the harness: Codex receives
  local file paths, Claude Code receives inline image content. Attachments are
  released once the message is sent.
- The transcript renders a sent message's images as thumbnails rather than
  leaving the reader with a bare `[Image #N]`.
- Attachments are bounded: at most 8 per message, and an image larger than 2048
  pixels on its long edge is downscaled before it is attached.

## Capabilities

### New Capabilities
- `agent-composer-attachments`: pasting, listing, removing, and sending images
  attached to an Agent composer message, and how a sent message's images appear
  in the transcript.

### Modified Capabilities
<!-- No existing capability's requirements change. Attachments are additive to
     the composer: text-only submission behaviour, input history, and slash
     command dispatch keep their current requirements. -->

## Impact

- `crates/app/src/agent_pane`: composer state gains an attachment list; the
  input view intercepts paste; the composer view gains a thumbnail strip; the
  send path forwards attachments; the transcript renders them.
- `crates/agent_utils/src/chat.rs`: the user-message send signature and the
  transcript item for a user message carry attachments.
- `crates/agent_utils/src/codex/app_server`: a turn's input gains `localImage`
  items, which requires writing each attachment to a temporary file.
- `crates/agent_utils/src/claude_code/stream_json`: a user message's content
  gains base64 image blocks.
- `crates/i18n/locales`: strings for the strip, the remove control, and the
  limits.
- No new dependency: gpui decodes clipboard images and renders encoded bytes.
  Downscaling needs an image codec, which gpui already depends on.
