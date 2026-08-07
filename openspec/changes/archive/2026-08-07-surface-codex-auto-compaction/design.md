## Context

Agent Tab consumes Codex through app-server. Current versions report both manual and automatic history rewrites as a `contextCompaction` thread item with only an `id`; the item follows the ordinary `item/started` and `item/completed` lifecycle, while active-context usage is reported separately through `thread/tokenUsage/updated`. The legacy `thread/compacted` notification is deprecated. Persisted `compacted.payload.message` is not the replacement summary and can be empty; the replacement history carries an opaque encrypted compaction item rather than displayable summary text.

The shared chat model and Agent Tab already support `CompactionStarted`, `CompactionFinished`, a structural `Item::Compaction`, optional accounting, and replay rendering. Claude Code already drives that model. Codex currently falls through to `Item::Other`, so the work is confined primarily to the Codex adapter.

## Goals / Non-Goals

**Goals:**

- Present automatic Codex compaction as an in-turn progress state and a durable structural transcript boundary.
- Classify live compaction as manual when this session issued `thread/compact/start`, and automatic otherwise.
- Populate before/after token counts only when surrounding usage snapshots demonstrate a reduction.
- Preserve a compaction boundary during Codex history replay even though persisted app-server items expose no metadata beyond their id.
- Align manual `/compact` feedback with the asynchronous item lifecycle.

**Non-Goals:**

- Implement compaction, summaries, thresholds, or model-context policy in NiumaTerm.
- Expose `model_auto_compact_token_limit` in Agent Tab settings.
- Read private Codex rollout files or encrypted compaction state to recover metadata omitted by app-server.
- Recover summary text, message counts, accounting, or trigger classification for historical Codex boundaries.
- Add compatibility rendering for the deprecated `thread/compacted` notification.

## Decisions

### Reuse the provider-neutral compaction model

The Codex adapter will emit the same progress and item events already used by Claude. `item/started` for `contextCompaction` emits `CompactionStarted` without inserting a transcript item. `item/completed` emits `CompactionFinished` and one completed `Item::Compaction`, letting the existing UI stop its spinner and insert the durable divider.

Keeping the in-progress marker separate from the completed item avoids rendering a boundary before the history rewrite succeeds and avoids relying on item-update behavior for metadata that becomes known only at completion.

### Track compaction correlation inside the Codex session

The session will retain the latest `ContextWindowUsage`, whether a manual compact request is pending, and the active compaction item's id, trigger, and pre-compaction token count. A successful manual RPC acknowledgement remains acceptance only; its marker is consumed when the corresponding item starts. A completed manual item produces the completion feedback.

This client-side marker is reliable for live sessions because a NiumaTerm Codex session owns one app-server child and manual compaction is accepted only while the thread is idle. The wire item has no trigger field, so historical items deliberately keep an unknown trigger.

### Treat token accounting as guarded best effort

The usage snapshot at `item/started` is the candidate `pre_tokens`. The latest snapshot at `item/completed` is accepted as `post_tokens` only when it is strictly smaller. This matches observed app-server ordering while preventing a stale or reordered usage event from creating false accounting. Missing or ambiguous values remain `None`, which the existing renderer already omits.

### Use `contextCompaction` as the single source of truth

Live notifications and replayed turn items both map `contextCompaction` to the dedicated item type. The deprecated `thread/compacted` notification remains ignored, preventing duplicate boundaries on versions that publish both forms.

### Keep unavailable Codex summaries unknown

The resume response remains authoritative for transcript structure and boundary ids. A persisted Codex `contextCompaction` item therefore replays with an unknown summary. The adapter does not treat `compacted.payload.message` as the summary and does not attempt to display the encrypted replacement context.

Codex boundaries are non-expandable because all available information already fits in the row label and accounting preview; an empty detail panel adds no value. Claude retains its disclosure because resumed transcripts can supply a real plaintext summary. No provider renders placeholder prose when a summary is absent.

## Risks / Trade-offs

- [App-server reorders usage after item completion] → The boundary still appears, but optional post-compaction accounting is omitted.
- [A compaction turn fails before the completed item] → Turn completion clears adapter state and the existing turn error remains visible; no successful boundary is fabricated.
- [Historical items omit trigger and accounting] → Leave those fields unknown instead of inferring them from unrelated persisted data.
- [Codex later exposes a readable summary] → Prefer that authoritative app-server field and re-enable its disclosure without changing the shared model.
- [Manual acknowledgement races with lifecycle notifications] → Manual intent is recorded before sending the request and completion is driven by the item rather than the acknowledgement.

## Migration Plan

No persisted NiumaTerm state changes. Deploy the adapter mapping and tests together; rollback restores generic tool rendering without affecting Codex threads or their stored histories.

## Open Questions

None for the initial implementation. If app-server later adds summary, trigger, or accounting fields to `contextCompaction`, the adapter can prefer those authoritative values without changing the shared model.
