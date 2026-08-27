//! What `/ctx` renders, and the profiler that accumulates it.

use std::ops::RangeInclusive;

use crate::event::InvalidationReason;
use crate::item::Category;
use crate::item::ItemGroup;
use crate::usage::UsageSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnDelta {
    pub turn_id: String,
    pub index: u32,
    pub item_seq_range: RangeInclusive<u64>,
    pub estimated_added: i64,
    /// The last anchor before this turn started, not the first anchor within it.
    pub measured_before: Option<i64>,
    pub measured_after: Option<i64>,
}

impl TurnDelta {
    pub fn measured_added(&self) -> Option<i64> {
        self.measured_before
            .zip(self.measured_after)
            .map(|(before, after)| after - before)
    }
}

/// Complete lists; the view truncates. Percentages are the TUI's job.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextSnapshot {
    pub window: Option<i64>,
    pub reported_context_tokens: Option<i64>,
    pub initial_context: Option<InitialContextSummary>,
    pub by_category: Vec<(Category, i64)>,
    /// Reconciled startup baseline (tool schemas + system prompt); frozen, not current cost.
    pub baseline_tokens: Option<i64>,
    /// Remainder of unknown cause, deliberately not attributed to the estimator.
    pub drift_tokens: i64,
    pub groups: Vec<ItemGroup>,
    pub turns: Vec<TurnDelta>,
}

impl ContextSnapshot {
    pub fn attributed_tokens(&self) -> i64 {
        self.by_category.iter().map(|(_, tokens)| tokens).sum()
    }

    pub fn explained_tokens(&self) -> i64 {
        self.attributed_tokens() + self.baseline_tokens.unwrap_or(0) + self.drift_tokens
    }
}

/// Startup context anchored to the measured first request, so the parts cannot exceed it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InitialContextSummary {
    pub first_request_input_tokens: i64,
    pub estimated_user_input_tokens: i64,
    pub estimated_instruction_tokens: i64,
}

impl InitialContextSummary {
    pub fn startup_context_tokens(&self) -> i64 {
        self.first_request_input_tokens - self.estimated_user_input_tokens
    }

    /// Must agree with `ContextSnapshot::baseline_tokens`, which reaches the same figure by anchor residual.
    pub fn hidden_tokens(&self) -> i64 {
        self.startup_context_tokens() - self.estimated_instruction_tokens
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfilerState {
    pub snapshot: ContextSnapshot,
    pub invalidated: Option<InvalidationReason>,
    pub unrecognized_fragment_count: u32,
    pub missing_usage_count: u32,
    pub anchors: Vec<UsageSnapshot>,
}

#[derive(Debug, Default)]
pub struct ContextProfiler {
    state: ProfilerState,
}

impl ContextProfiler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &ProfilerState {
        &self.state
    }
}
