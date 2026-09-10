/// Independently reported content, presentation, and source-copy operations.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    AppendEntry,
    Replay,
    MergeCompleted,
    AppendDelta,
    RowsRebuild,
    Typewriter,
    MirrorRebuild,
    BackgroundSnapshot,
    WorkflowSnapshot,
}
