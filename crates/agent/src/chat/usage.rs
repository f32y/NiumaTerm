//! What a conversation has spent: tokens by kind, and how much of the context
//! window is left.
//!
//! Cached reads are counted apart from fresh ones because they are what makes
//! a long conversation affordable, and a reader judging cost needs to see the
//! two separately.

/// Whole-log conversation counters, independent of how much history has been
/// paged in. Reported only by a backend that folds them from its complete log;
/// a count derived from the visible transcript would disagree with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub turns: u64,
    pub steps: u64,
    /// Summed model wall time over the steps that produced a message.
    pub model_ms: u64,
    /// Summed tool wall time over matched call/result pairs.
    pub tool_ms: u64,
}

/// Token accounting from one provider reporting scope. The total is
/// authoritative; optional categories describe parts of that total and stay
/// absent when a protocol does not expose them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsageBreakdown {
    pub total_tokens: u64,
    pub input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

impl TokenUsageBreakdown {
    /// A compaction boundary can report the replacement size without enough
    /// information to attribute tokens to categories.
    pub const fn total_only(total_tokens: u64) -> Self {
        Self {
            total_tokens,
            input_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextUsageScope {
    Thread,
    LastTurn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopedTokenUsage {
    pub scope: ContextUsageScope,
    pub breakdown: TokenUsageBreakdown,
}

/// Latest replacement snapshot of active context usage and any cumulative
/// accounting that the same provider update can identify precisely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextWindowUsage {
    pub current: TokenUsageBreakdown,
    pub cumulative: Option<ScopedTokenUsage>,
    pub max_tokens: Option<u64>,
}

impl ContextWindowUsage {
    pub const fn used_tokens(self) -> u64 {
        self.current.total_tokens
    }
}

/// One labelled part of what currently fills the context window, such as the
/// system prompt, the tool definitions, or the conversation itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSegment {
    pub label: String,
    pub tokens: u64,
    /// Colour the provider suggests for this segment, as it writes it. Kept as
    /// the provider's own string because a UI may prefer its theme instead.
    pub color: Option<String>,
    /// The segment is reserved rather than occupied: counted against the
    /// window, but holding no conversation content yet.
    pub deferred: bool,
}

/// How the context window is currently filled, as opposed to how tokens were
/// billed. A provider that only reports accounting never publishes this, so
/// its absence is a normal state rather than a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextComposition {
    pub segments: Vec<ContextSegment>,
    pub used_tokens: u64,
    /// Window the provider measures against. This can be smaller than the
    /// model's own window when the provider reserves room to compact.
    pub max_tokens: Option<u64>,
    /// The model's window before any such reserve, when the provider
    /// distinguishes the two.
    pub raw_max_tokens: Option<u64>,
    /// Where automatic compaction takes over, when the provider reports it.
    pub auto_compact_threshold: Option<u64>,
}
