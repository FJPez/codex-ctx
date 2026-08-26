//! Usage anchors: the measured points the profiler reconciles its estimates against.

/// One usage anchor. Numbers only - no `response_id`, which nothing in the MVP
/// renders or joins against, and which the rollout path cannot reproduce.
///
/// The six token fields duplicate `codex_protocol::protocol::TokenUsage`. That is
/// deliberate: the rename of `total_tokens` to `reported_context_tokens` is the
/// point, because `ThreadTokenUsage` carries both a cumulative `total` and an
/// occupancy `last`, and confusing them produces plausible nonsense.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageSnapshot {
    /// Current occupancy of the context window, taken from `last`, NEVER `total`.
    ///
    /// `total` accumulates across the session and exceeds the window on any long
    /// thread; only `last` tracks occupancy.
    pub reported_context_tokens: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    /// Only the `thread/tokenUsage/updated` stream carries the window, so this is
    /// `None` when an anchor could not be merged with one.
    pub model_context_window: Option<i64>,
    /// Number of items observed when this anchor arrived.
    pub items_seq: u64,
}
