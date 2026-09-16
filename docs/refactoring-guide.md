# Refactoring Guide

Reduce the code, state, and navigation needed to understand a real execution
path. Preserve required behavior and remove structure that no longer earns its
cost.

## Principles

1. **Keep related code in one source file.** Keep a responsibility's types,
   helpers, and behavior together by default. A helper used by only one source
   file usually belongs in that file. Extract a shared file when logic in
   multiple source files needs it. File length or one-type-per-file convention
   alone does not justify a split. Split genuinely different responsibilities;
   merge fragments of the same responsibility.

2. **Follow actual producers and consumers.** Check construction, reads, and
   callers before keeping a field, enum variant, wrapper, or public API. Remove
   unused representations and speculative extension points. Keep visibility as
   narrow as real callers allow.

3. **Keep one authoritative owner for each state.** Derive secondary values
   from existing state instead of maintaining another ledger. Remove the
   synchronization code and consistency checks made unnecessary by that change.
   Retain caches only when their benefit and invalidation rules are clear.

4. **Return complete results from the layer doing the work.** Assemble related
   information where it is available instead of making callers join partial
   events. Commands return their outcomes; the view decides notifications,
   focus, scrolling, and other presentation reactions.

5. **Make types express meaningful distinctions.** Merge identical enums,
   one-to-one result wrappers, and redundant aliases. Remove result fields no
   caller reads. Preserve separate variants when callers need different
   behavior, recovery, or diagnostics.

6. **Share equivalent behavior, not merely similar shapes.** Reuse common
   parsing and transformations when their semantics agree. Keep provider
   differences explicit. Prefer existing engine capabilities and suitable
   dependencies over duplicate implementations or hand-written utilities.

7. **Match mechanisms to the workload.** Use a complete snapshot when small
   data does not need a journal and replay chain. Preserve essential guarantees
   such as validation, atomic persistence, locking, and persistence before
   dispatch. Decide format compatibility explicitly and respect requested
   feature boundaries.

8. **Complete and verify the simplification.** Update callers and remove the
   obsolete path, files, and tests that only describe the removed machinery.
   Check behavior that could regress, especially recovery, duplicate dispatch,
   budgets, and notifications. Avoid tests that merely repeat a trivial helper's
   implementation.

## Reference

These principles summarize the refactoring series from `bbea6169` through
`5742635c`, inclusive. Across the full commit range, 69 Rust source files were
deleted and 5 added, for a net reduction of 64; renames were counted separately.
The aim is less unnecessary structure, not a file-count or line-count target.
