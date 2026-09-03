//! Per-thread state machine converting v2 notifications into profiler records.

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnStatus;
use codex_context_profiler::InvalidationReason;
use codex_context_profiler::ProfilerEvent;
use codex_context_profiler::UsageSnapshot;
use codex_context_profiler::call_id;
use codex_context_profiler::item_kind;
use codex_context_profiler::serialized_size;
use serde::Serialize;

/// One observation, flattened for a trace log. Never carries item text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RecordedEvent {
    pub thread_id: String,
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub kind: RecordedKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RecordedKind {
    Attached,
    TurnStarted,
    Item {
        item_kind: String,
        bytes: usize,
        items_seq: u64,
        stamped_turn_id: Option<String>,
        call_id: Option<String>,
    },
    Usage {
        response_id: String,
        reported_context_tokens: i64,
        input_tokens: i64,
        cached_input_tokens: i64,
        cache_write_input_tokens: i64,
        output_tokens: i64,
        reasoning_output_tokens: i64,
        items_seq: u64,
    },
    MissingUsage {
        response_id: String,
    },
    WindowUpdated {
        window: i64,
        matches_anchor: Option<bool>,
    },
    TurnEnded {
        completed: bool,
        status: String,
    },
    Invalidated {
        reason: String,
    },
}

/// Extra payload fields that `ProfilerEvent` does not carry.
#[derive(Default)]
struct RecordFields {
    turn_id: Option<String>,
    response_id: Option<String>,
    status: Option<String>,
    matches_anchor: Option<bool>,
}

/// Converts notifications for one thread into profiler events and trace records.
pub(crate) struct ThreadProfilerAdapter {
    items_seq: u64,
    last_anchor_total: Option<i64>,
}

impl ThreadProfilerAdapter {
    pub(crate) fn new() -> Self {
        Self {
            items_seq: 0,
            last_anchor_total: None,
        }
    }

    /// Trace-lifecycle record written when the registry starts observing a thread.
    pub(crate) fn attached(&self, thread_id: &str) -> RecordedEvent {
        RecordedEvent {
            thread_id: thread_id.to_string(),
            turn_id: None,
            kind: RecordedKind::Attached,
        }
    }

    /// Drops the anchor so later window updates are not attributed to stale usage.
    pub(crate) fn invalidate(
        &mut self,
        thread_id: &str,
        reason: InvalidationReason,
    ) -> RecordedEvent {
        self.last_anchor_total = None;
        to_record(
            thread_id,
            ProfilerEvent::Invalidated { reason },
            self.items_seq,
            RecordFields::default(),
        )
    }

    pub(crate) fn observe(&mut self, notification: &ServerNotification) -> Option<RecordedEvent> {
        match notification {
            ServerNotification::TurnStarted(params) => Some(to_record(
                &params.thread_id,
                ProfilerEvent::TurnStarted {
                    turn_id: &params.turn.id,
                },
                self.items_seq,
                RecordFields::default(),
            )),
            ServerNotification::RawResponseItemCompleted(params) => {
                self.items_seq += 1;
                Some(to_record(
                    &params.thread_id,
                    ProfilerEvent::Item {
                        turn_id: &params.turn_id,
                        item: &params.item,
                    },
                    self.items_seq,
                    RecordFields::default(),
                ))
            }
            ServerNotification::RawResponseCompleted(params) => match &params.usage {
                Some(usage) => {
                    let anchor_total = usage.total_tokens;
                    self.last_anchor_total = Some(anchor_total);
                    Some(to_record(
                        &params.thread_id,
                        ProfilerEvent::Usage {
                            turn_id: &params.turn_id,
                            usage: UsageSnapshot {
                                reported_context_tokens: anchor_total,
                                input_tokens: usage.input_tokens,
                                cached_input_tokens: usage.cached_input_tokens,
                                cache_write_input_tokens: usage.cache_write_input_tokens,
                                output_tokens: usage.output_tokens,
                                reasoning_output_tokens: usage.reasoning_output_tokens,
                                items_seq: self.items_seq,
                            },
                        },
                        self.items_seq,
                        RecordFields {
                            response_id: Some(params.response_id.clone()),
                            ..RecordFields::default()
                        },
                    ))
                }
                None => {
                    self.last_anchor_total = None;
                    Some(to_record(
                        &params.thread_id,
                        ProfilerEvent::UsageMissing {
                            turn_id: &params.turn_id,
                        },
                        self.items_seq,
                        RecordFields {
                            response_id: Some(params.response_id.clone()),
                            ..RecordFields::default()
                        },
                    ))
                }
            },
            ServerNotification::ThreadTokenUsageUpdated(params) => {
                let window = params.token_usage.model_context_window?;
                let matches_anchor = self
                    .last_anchor_total
                    .take()
                    .map(|anchor| anchor == params.token_usage.last.total_tokens);
                Some(to_record(
                    &params.thread_id,
                    ProfilerEvent::WindowUpdated {
                        turn_id: &params.turn_id,
                        window,
                    },
                    self.items_seq,
                    RecordFields {
                        matches_anchor,
                        ..RecordFields::default()
                    },
                ))
            }
            ServerNotification::TurnCompleted(params) => {
                let completed = params.turn.status == TurnStatus::Completed;
                Some(to_record(
                    &params.thread_id,
                    ProfilerEvent::TurnEnded {
                        turn_id: &params.turn.id,
                        completed,
                    },
                    self.items_seq,
                    RecordFields {
                        status: Some(turn_status_name(&params.turn.status).to_string()),
                        ..RecordFields::default()
                    },
                ))
            }
            ServerNotification::ItemCompleted(params) => {
                if !matches!(params.item, ThreadItem::ContextCompaction { .. }) {
                    return None;
                }
                self.last_anchor_total = None;
                Some(to_record(
                    &params.thread_id,
                    ProfilerEvent::Invalidated {
                        reason: InvalidationReason::Compacted,
                    },
                    self.items_seq,
                    RecordFields {
                        turn_id: Some(params.turn_id.clone()),
                        ..RecordFields::default()
                    },
                ))
            }
            _ => None,
        }
    }
}

fn to_record(
    thread_id: &str,
    event: ProfilerEvent<'_>,
    items_seq: u64,
    fields: RecordFields,
) -> RecordedEvent {
    let (turn_id, kind) = match event {
        ProfilerEvent::TurnStarted { turn_id } => {
            (Some(turn_id.to_string()), RecordedKind::TurnStarted)
        }
        ProfilerEvent::Item { turn_id, item } => (
            Some(turn_id.to_string()),
            RecordedKind::Item {
                item_kind: item_kind(item).as_str().to_string(),
                bytes: serialized_size(item).unwrap_or(0),
                items_seq,
                stamped_turn_id: item.turn_id().map(str::to_string),
                call_id: call_id(item),
            },
        ),
        ProfilerEvent::Usage { turn_id, usage } => (
            Some(turn_id.to_string()),
            RecordedKind::Usage {
                response_id: fields.response_id.unwrap_or_default(),
                reported_context_tokens: usage.reported_context_tokens,
                input_tokens: usage.input_tokens,
                cached_input_tokens: usage.cached_input_tokens,
                cache_write_input_tokens: usage.cache_write_input_tokens,
                output_tokens: usage.output_tokens,
                reasoning_output_tokens: usage.reasoning_output_tokens,
                items_seq: usage.items_seq,
            },
        ),
        ProfilerEvent::WindowUpdated { turn_id, window } => (
            Some(turn_id.to_string()),
            RecordedKind::WindowUpdated {
                window,
                matches_anchor: fields.matches_anchor,
            },
        ),
        ProfilerEvent::TurnEnded { turn_id, completed } => (
            Some(turn_id.to_string()),
            RecordedKind::TurnEnded {
                completed,
                status: fields.status.unwrap_or_default(),
            },
        ),
        ProfilerEvent::UsageMissing { turn_id } => (
            Some(turn_id.to_string()),
            RecordedKind::MissingUsage {
                response_id: fields.response_id.unwrap_or_default(),
            },
        ),
        ProfilerEvent::Invalidated { reason } => (
            fields.turn_id,
            RecordedKind::Invalidated {
                reason: invalidation_reason(&reason),
            },
        ),
    };
    RecordedEvent {
        thread_id: thread_id.to_string(),
        turn_id,
        kind,
    }
}

fn invalidation_reason(reason: &InvalidationReason) -> String {
    match reason {
        InvalidationReason::EventsDropped { skipped } => {
            format!("events_dropped(skipped={skipped})")
        }
        InvalidationReason::Compacted => "compacted".to_string(),
        InvalidationReason::SequenceMismatch {
            anchor_items_seen,
            profiler_items_seen,
        } => {
            format!("sequence_mismatch(anchor={anchor_items_seen}, profiler={profiler_items_seen})")
        }
    }
}

fn turn_status_name(status: &TurnStatus) -> &'static str {
    match status {
        TurnStatus::Completed => "completed",
        TurnStatus::Interrupted => "interrupted",
        TurnStatus::Failed => "failed",
        TurnStatus::InProgress => "inProgress",
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
