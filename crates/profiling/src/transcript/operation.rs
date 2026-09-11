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

impl From<Operation> for &'static str {
    /// Stable operation name used by live reports and repeatable measurements.
    fn from(value: Operation) -> Self {
        match value {
            Operation::AppendEntry => "append-entry",
            Operation::Replay => "replay",
            Operation::MergeCompleted => "merge-completed",
            Operation::AppendDelta => "append-delta",
            Operation::RowsRebuild => "rows-rebuild",
            Operation::Typewriter => "typewriter",
            Operation::MirrorRebuild => "mirror-rebuild",
            Operation::BackgroundSnapshot => "background-snapshot",
            Operation::WorkflowSnapshot => "workflow-snapshot",
        }
    }
}
