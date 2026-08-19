## 1. Attachment model

- [x] 1.1 Add an `Attachment` type in `crates/app/src/agent_pane/composer/attachments.rs`: encoded bytes, image format, pixel dimensions, and the placeholder text it is anchored by
- [x] 1.2 Add a `PendingAttachments` collection with attach, remove-at-index, and "reconcile against composer text" operations, where reconcile drops attachments whose placeholder is absent and renumbers the survivors consecutively from 1
- [x] 1.3 Unit-test reconcile: removing the first of three, removing the middle, deleting a placeholder by hand, clearing all text, and a hand-typed placeholder naming no attachment
- [x] 1.4 Unit-test the caps: a ninth attach is refused, and an oversized image is reported as needing downscaling to 2048 on its long edge with its aspect ratio kept

## 2. Clipboard capture

- [x] 2.1 Read `ClipboardEntry::Image` from `cx.read_from_clipboard()` and convert it to an `Attachment`, downscaling past 2048 pixels on the long edge
- [x] 2.2 Confirm on the running application which clipboard sources produce an image entry on Windows (screenshot tool, browser copy, Explorer file copy) and record any that do not
- [x] 2.3 Refuse a paste on a harness with no image input, with a composer feedback message

## 3. Composer wiring

- [x] 3.1 Hold `PendingAttachments` on `AgentPane` beside the composer state
- [x] 3.2 Intercept paste in `agent_pane/view/input.rs`: attach and insert `[Image #N]` at the caret when the clipboard holds an image, otherwise fall through to text paste
- [x] 3.3 Reconcile after composer edits, only when the set of placeholders in the text changed
- [x] 3.4 Add i18n strings for the strip, the remove control, the attachment-limit refusal, and the unsupported-harness refusal

## 4. Thumbnail strip

- [x] 4.1 Render the strip above the composer from the pending attachments, using `img()` over `gpui::Image::from_bytes`, hidden while nothing is attached
- [x] 4.2 Add a remove control per thumbnail that deletes the attachment's placeholder from the composer text and then reconciles
- [x] 4.3 Verify in the running application that the composer does not jump as the strip appears and disappears

## 5. Send path

- [x] 5.1 Carry attachments through `Backend::send_user_message` and the `SendOutcome` path, clearing the pending list only on an accepted send
- [x] 5.2 Codex: write each attachment to a per-tab temporary directory at send time and add `{"type":"localImage","path":...}` items to the turn input after the text item, in placeholder order
- [x] 5.3 Codex: remove the per-tab temporary directory when the tab closes
- [x] 5.4 Claude Code: add `{"type":"image","source":{"type":"base64",...}}` blocks to the user message content after the text block, in placeholder order
- [x] 5.5 Unit-test both adapters' request shapes for a message with two attachments, and confirm a text-only message's request is byte-identical to today's

## 6. Transcript

- [x] 6.1 Carry a sent message's images on its transcript item
- [x] 6.2 Render them as thumbnails with the message text
- [x] 6.3 Confirm a replayed conversation containing an image message renders without error

## 7. Verification

- [x] 7.1 Exercise the whole path in the running application on Claude Code and on Codex: paste, remove from the strip, delete a placeholder by hand, send, and read the transcript
- [x] 7.2 Confirm a refused send keeps its attachments pending
- [x] 7.3 Run `cargo clippy -p app -p nmt_agent_utils --all-targets` and the test suites
