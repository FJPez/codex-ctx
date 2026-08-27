//! Usage anchors: the measured points the profiler reconciles its estimates against.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageSnapshot {
    /// Window occupancy, from `ThreadTokenUsage::last`, never `total` (which accumulates).
    pub reported_context_tokens: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub model_context_window: Option<i64>,
    /// Number of items observed when this anchor arrived.
    pub items_seq: u64,
}
