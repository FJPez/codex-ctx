//! The single input vocabulary of the profiler.
//!
//! An adapter - live app-server or rollout replay - translates its own stream into
//! these events and folds them through one `observe` entry point, so adding a
//! variant produces a compile error at the adapter rather than a silently
//! unhandled case.

use codex_protocol::models::ResponseItem;

use crate::usage::UsageSnapshot;

/// One observation about a session's context, as seen by an adapter.
///
/// `turn_id` is present on every turn-scoped variant so an item arriving outside a
/// turn is attributable rather than silently misfiled.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfilerEvent<'a> {
    /// A turn began. The profiler captures the last anchor from before this point
    /// as the turn's `measured_before`.
    TurnStarted { turn_id: &'a str },
    /// A history item entered the context window.
    Item {
        turn_id: &'a str,
        item: &'a ResponseItem,
    },
    /// A usage anchor arrived for this turn.
    Usage {
        turn_id: &'a str,
        usage: UsageSnapshot,
    },
    /// A turn ended. `completed` is false for interrupted or failed turns, whose
    /// measured delta is not trustworthy and renders as `-` rather than a number.
    TurnEnded { turn_id: &'a str, completed: bool },
    /// Attribution can no longer be trusted from here on.
    Invalidated { reason: InvalidationReason },
}

/// Why attribution stopped being trustworthy.
///
/// Both variants collapse to the same product behaviour: we can no longer account
/// for what is in the window, so say so instead of showing a plausible number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InvalidationReason {
    /// The app-server event consumer lagged; `AppServerEvent::Lagged { skipped }`.
    ///
    /// That event is connection-level and carries no `thread_id`, so the adapter
    /// must broadcast it to every live profiler. Do not attempt to derive a
    /// `thread_id` for it - there isn't one.
    EventsDropped { skipped: usize },
    /// `thread/compacted`. History was rewritten out from under us.
    ///
    /// Sealing the pre-compaction state so it can be inspected and compared is
    /// later work; until then a compaction ends attribution for the session.
    Compacted,
}
