//! Per-thread state machine converting v2 notifications into profiler events.

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnStatus;
use codex_context_profiler::InvalidationReason;
use codex_context_profiler::ProfilerEvent;
use codex_context_profiler::UsageSnapshot;

/// One profiler event plus the trace-only payload the event itself does not carry.
pub(crate) struct Observation<'a> {
    pub event: ProfilerEvent<'a>,
    pub items_seq: u64,
    pub fields: RecordFields,
}

/// Extra payload fields that `ProfilerEvent` does not carry.
#[derive(Default)]
pub(crate) struct RecordFields {
    pub turn_id: Option<String>,
    pub response_id: Option<String>,
    pub status: Option<String>,
    pub matches_anchor: Option<bool>,
}

/// Converts notifications for one thread into profiler events.
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

    /// Drops the anchor so later window updates are not attributed to stale usage.
    pub(crate) fn invalidate(&mut self, reason: InvalidationReason) -> Observation<'static> {
        self.last_anchor_total = None;
        Observation {
            event: ProfilerEvent::Invalidated { reason },
            items_seq: self.items_seq,
            fields: RecordFields::default(),
        }
    }

    pub(crate) fn observe<'a>(
        &mut self,
        notification: &'a ServerNotification,
    ) -> Option<Observation<'a>> {
        match notification {
            ServerNotification::TurnStarted(params) => Some(Observation {
                event: ProfilerEvent::TurnStarted {
                    turn_id: &params.turn.id,
                },
                items_seq: self.items_seq,
                fields: RecordFields::default(),
            }),
            ServerNotification::RawResponseItemCompleted(params) => {
                self.items_seq += 1;
                Some(Observation {
                    event: ProfilerEvent::Item {
                        turn_id: &params.turn_id,
                        item: &params.item,
                    },
                    items_seq: self.items_seq,
                    fields: RecordFields::default(),
                })
            }
            ServerNotification::RawResponseCompleted(params) => match &params.usage {
                Some(usage) => {
                    let anchor_total = usage.total_tokens;
                    self.last_anchor_total = Some(anchor_total);
                    Some(Observation {
                        event: ProfilerEvent::Usage {
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
                        items_seq: self.items_seq,
                        fields: RecordFields {
                            response_id: Some(params.response_id.clone()),
                            ..RecordFields::default()
                        },
                    })
                }
                None => {
                    self.last_anchor_total = None;
                    Some(Observation {
                        event: ProfilerEvent::UsageMissing {
                            turn_id: &params.turn_id,
                        },
                        items_seq: self.items_seq,
                        fields: RecordFields {
                            response_id: Some(params.response_id.clone()),
                            ..RecordFields::default()
                        },
                    })
                }
            },
            ServerNotification::ThreadTokenUsageUpdated(params) => {
                let window = params.token_usage.model_context_window?;
                let matches_anchor = self
                    .last_anchor_total
                    .take()
                    .map(|anchor| anchor == params.token_usage.last.total_tokens);
                Some(Observation {
                    event: ProfilerEvent::WindowUpdated {
                        turn_id: &params.turn_id,
                        window,
                    },
                    items_seq: self.items_seq,
                    fields: RecordFields {
                        matches_anchor,
                        ..RecordFields::default()
                    },
                })
            }
            ServerNotification::TurnCompleted(params) => {
                let completed = params.turn.status == TurnStatus::Completed;
                Some(Observation {
                    event: ProfilerEvent::TurnEnded {
                        turn_id: &params.turn.id,
                        completed,
                    },
                    items_seq: self.items_seq,
                    fields: RecordFields {
                        status: Some(turn_status_name(&params.turn.status).to_string()),
                        ..RecordFields::default()
                    },
                })
            }
            ServerNotification::ItemCompleted(params) => {
                if !matches!(params.item, ThreadItem::ContextCompaction { .. }) {
                    return None;
                }
                self.last_anchor_total = None;
                Some(Observation {
                    event: ProfilerEvent::Invalidated {
                        reason: InvalidationReason::Compacted,
                    },
                    items_seq: self.items_seq,
                    fields: RecordFields {
                        turn_id: Some(params.turn_id.clone()),
                        ..RecordFields::default()
                    },
                })
            }
            _ => None,
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
