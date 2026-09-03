//! Input events consumed by the profiler.

use codex_protocol::models::ResponseItem;

use crate::usage::UsageSnapshot;

/// One observation about a session's context, as seen by an adapter.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfilerEvent<'a> {
    TurnStarted {
        turn_id: &'a str,
    },
    Item {
        turn_id: &'a str,
        item: &'a ResponseItem,
    },
    Usage {
        turn_id: &'a str,
        usage: UsageSnapshot,
    },
    /// A response completed without usage: a boundary that prices nothing.
    UsageMissing {
        turn_id: &'a str,
    },
    /// The model context window reported by `thread/tokenUsage/updated`.
    WindowUpdated {
        turn_id: &'a str,
        window: i64,
    },
    TurnEnded {
        turn_id: &'a str,
        completed: bool,
    },
    /// Attribution can no longer be trusted from here on.
    Invalidated {
        reason: InvalidationReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InvalidationReason {
    /// `AppServerEvent::Lagged` is connection-level; adapters must broadcast it to every profiler.
    EventsDropped {
        skipped: usize,
    },
    Compacted,
    /// An anchor's item count disagreed with the profiler's, so the item stream is incomplete.
    SequenceMismatch {
        anchor_items_seen: u64,
        profiler_items_seen: u64,
    },
}
