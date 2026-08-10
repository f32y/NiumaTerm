## Context

See `proposal.md` for motivation. The shared chat model currently carries only a used-token count and an optional maximum. Codex app-server sends current and thread-total breakdowns in each token-usage notification. Claude stream-json sends current usage during assistant streaming and last-turn usage in the result message. The existing titlebar already owns account rate-limit presentation.

## Goals / Non-Goals

**Goals:**

- Preserve one coherent context snapshot through the provider, session, and composer layers.
- Normalize enough token categories for a shared panel while retaining provider-specific availability.
- Keep cache and reasoning values visually subordinate to their inclusive input or output totals.
- Reuse the existing GPUI component hover-card behavior so the pointer can move from the trigger into the panel.

**Non-Goals:**

- Display account rate limits, repository state, session identifiers, or provider diagnostics in the context panel.
- Add cost estimates or billing behavior.
- Persist transient usage snapshots across application restarts.

## Decisions

### Carry structured current and cumulative usage

Replace the two-field shared snapshot with a current token breakdown, an optional scoped cumulative breakdown, and the optional context limit. Category values remain optional because providers and compaction boundaries do not always report the same detail.

The normalized input value represents total input occupying the context. Cache-read and cache-write values are descriptive parts of that input. Output is the total provider output, with reasoning output represented as an optional descriptive part. The provider-reported total remains authoritative instead of being recomputed from detail rows.

An alternative was to keep provider-specific payloads in the UI. That would couple the composer to protocol field names and duplicate formatting behavior.

### Use explicit cumulative scopes

Codex cumulative values are labeled `Thread total`. Claude result usage is labeled `Last turn`. Encoding the scope in the shared snapshot prevents a generic `Total` label from implying equal accounting periods.

An alternative was to omit cumulative values. That would discard a substantial portion of the provider data requested for this surface.

### Treat every context event as a replacement snapshot

Adapters construct the complete shared value before emitting an update. A compaction boundary that only supplies a replacement total clears unavailable category detail instead of retaining values from the pre-compaction context.

An alternative was to merge sparse updates in the pane. That would require provider-specific merge rules and could combine values from different model calls.

### Use a hover card rather than a plain tooltip

The panel uses the GPUI component hover card with a short opening delay and a close delay that permits moving the pointer into the content. The existing compact ChartPie indicator remains the trigger and keeps its current one-line label.

A plain text tooltip cannot express the hierarchy between totals and detail categories and cannot support the requested small panel layout.

## Risks / Trade-offs

- [Older provider versions omit newer token categories] → Parse detail fields independently and hide missing rows.
- [Claude exposes its context limit only after a completed result] → Show current usage without a maximum until the provider supplies one.
- [Large values make the panel noisy] → Use compact token formatting while retaining unrounded values in accessible labels where practical.
- [A hover-only surface is unavailable before the first usage event] → Keep the indicator conditional on the same first valid snapshot as today.

## Migration Plan

No persisted data migration is required. Update the shared model and both adapters together, then update the composer consumer in the same code commit. Rollback consists of reverting those coordinated code changes.
