//! What `/ctx` renders, and the profiler that accumulates it.

use std::ops::RangeInclusive;

use crate::event::InvalidationReason;
use crate::item::Category;
use crate::item::ItemGroup;
use crate::usage::UsageSnapshot;

/// What one turn added to the context window, estimated and measured.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnDelta {
    pub turn_id: String,
    pub index: u32,
    /// Global `ItemSummary::seq` range.
    pub item_seq_range: RangeInclusive<u64>,
    pub estimated_added: i64,
    /// The last anchor from BEFORE this turn started, captured on `TurnStarted`.
    /// `None` for the first turn of a session, where no prior anchor exists.
    ///
    /// This is the pre-turn anchor, not the first anchor *within* the turn: the
    /// first usage of a turn already reflects the user's prompt and the first model
    /// response, so `last_in_turn - first_in_turn` silently omits everything the
    /// prompt itself contributed - catastrophically so on turn one, where it would
    /// omit the entire initial context.
    pub measured_before: Option<i64>,
    /// The last anchor observed during this turn.
    pub measured_after: Option<i64>,
}

impl TurnDelta {
    /// The change in measured active context across the whole turn, when both
    /// anchors exist.
    pub fn measured_added(&self) -> Option<i64> {
        self.measured_before
            .zip(self.measured_after)
            .map(|(before, after)| after - before)
    }
}

/// What `/ctx` renders. Read back via [`ContextProfiler::state`].
///
/// The snapshot carries complete lists; truncation happens in the renderer.
/// Percentages are not the profiler's job - the TUI computes those from `window`,
/// `reported_context_tokens`, and `baseline_tokens` with its own helper.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextSnapshot {
    pub window: Option<i64>,
    pub reported_context_tokens: Option<i64>,
    pub initial_context: Option<InitialContextSummary>,
    pub by_category: Vec<(Category, i64)>,
    /// Reconciled - not measured - startup hidden baseline: tool schemas plus the
    /// base system prompt.
    ///
    /// Frozen to the initial hidden baseline. Tool schemas can change mid-session
    /// and those changes are invisible to us, so their cost lands in
    /// `drift_tokens`; the row means *startup* hidden baseline, not current
    /// system-and-tools cost.
    pub baseline_tokens: Option<i64>,
    /// Remainder of unknown cause.
    ///
    /// Deliberately not called estimator error. A growing remainder can mean
    /// estimator error, an unobserved token-bearing item, or new Codex behaviour;
    /// at least one such class is known to exist. Naming it after one hypothesis
    /// would misdirect whoever debugs it.
    pub drift_tokens: i64,
    /// Complete list; the view caps it.
    pub groups: Vec<ItemGroup>,
    pub turns: Vec<TurnDelta>,
}

impl ContextSnapshot {
    /// Items only. Derived, not stored - a stored copy can disagree with its inputs.
    pub fn attributed_tokens(&self) -> i64 {
        self.by_category.iter().map(|(_, tokens)| tokens).sum()
    }

    /// Everything the profiler can account for: attributed items plus the
    /// reconciled baseline plus drift.
    pub fn explained_tokens(&self) -> i64 {
        self.attributed_tokens() + self.baseline_tokens.unwrap_or(0) + self.drift_tokens
    }
}

/// Startup context, anchored to the authoritative first-request measurement rather
/// than assembled from independent estimates. Three stored fields; the rest derive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InitialContextSummary {
    /// Measured.
    pub first_request_input_tokens: i64,
    /// Estimated.
    pub estimated_user_input_tokens: i64,
    /// Estimated.
    pub estimated_instruction_tokens: i64,
}

impl InitialContextSummary {
    /// Everything present at startup other than the user's own first input.
    pub fn startup_context_tokens(&self) -> i64 {
        self.first_request_input_tokens - self.estimated_user_input_tokens
    }

    /// System prompt + tool schemas. Whatever the measured total leaves after the
    /// observed instruction items are estimated - so the parts sum by construction
    /// and the headline cannot exceed what was actually sent.
    ///
    /// This is the same quantity as [`ContextSnapshot::baseline_tokens`] by a
    /// second route: one from the first-request decomposition, one from the
    /// first-anchor residual. They must agree, and a material disagreement is a bug
    /// in one of the two.
    pub fn hidden_tokens(&self) -> i64 {
        self.startup_context_tokens() - self.estimated_instruction_tokens
    }
}

/// The profiler's full state: the renderable snapshot plus the invalidation state,
/// counters, and reconciliation input that sit beside it rather than inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfilerState {
    pub snapshot: ContextSnapshot,
    /// `None` while attribution is trustworthy.
    pub invalidated: Option<InvalidationReason>,
    /// Surfaced in `/ctx`: classification gaps we know about.
    pub unrecognized_fragment_count: u32,
    /// Anchors we could not record because usage was absent.
    pub missing_usage_count: u32,
    /// Reconciliation input, not diagnostics.
    pub anchors: Vec<UsageSnapshot>,
}

/// Accumulates [`ProfilerEvent`](crate::ProfilerEvent)s into a [`ProfilerState`].
#[derive(Debug, Default)]
pub struct ContextProfiler {
    state: ProfilerState,
}

impl ContextProfiler {
    /// A profiler with empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// The state accumulated so far.
    pub fn state(&self) -> &ProfilerState {
        &self.state
    }
}
