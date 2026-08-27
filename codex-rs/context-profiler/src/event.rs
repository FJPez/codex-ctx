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
}
