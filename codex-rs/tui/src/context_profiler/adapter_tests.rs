use super::*;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::RawResponseItemCompletedNotification;
use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::TokenUsageBreakdown;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartedNotification;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

const THREAD: &str = "th_1";
const TURN: &str = "tu_1";

fn turn(status: TurnStatus) -> Turn {
    Turn {
        id: TURN.to_string(),
        items: Vec::new(),
        items_view: TurnItemsView::NotLoaded,
        status,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

fn turn_started() -> ServerNotification {
    ServerNotification::TurnStarted(TurnStartedNotification {
        thread_id: THREAD.to_string(),
        turn: turn(TurnStatus::InProgress),
    })
}

fn turn_completed(status: TurnStatus) -> ServerNotification {
    ServerNotification::TurnCompleted(TurnCompletedNotification {
        thread_id: THREAD.to_string(),
        turn: turn(status),
    })
}

fn message_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn custom_tool_call(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: call_id.to_string(),
        name: "shell".to_string(),
        namespace: None,
        input: "ls".to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn custom_tool_call_output(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("ok".to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning_item() -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: None,
        encrypted_content: Some("opaque".to_string()),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn raw_item(item: ResponseItem) -> ServerNotification {
    ServerNotification::RawResponseItemCompleted(RawResponseItemCompletedNotification {
        thread_id: THREAD.to_string(),
        turn_id: TURN.to_string(),
        item,
    })
}

fn raw_usage(response_id: &str, input_tokens: i64, output_tokens: i64) -> ServerNotification {
    ServerNotification::RawResponseCompleted(RawResponseCompletedNotification {
        thread_id: THREAD.to_string(),
        turn_id: TURN.to_string(),
        response_id: response_id.to_string(),
        usage: Some(TokenUsageBreakdown {
            total_tokens: input_tokens + output_tokens,
            input_tokens,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens,
            reasoning_output_tokens: 0,
        }),
        usage_metadata: None,
    })
}

fn raw_usage_missing(response_id: &str) -> ServerNotification {
    ServerNotification::RawResponseCompleted(RawResponseCompletedNotification {
        thread_id: THREAD.to_string(),
        turn_id: TURN.to_string(),
        response_id: response_id.to_string(),
        usage: None,
        usage_metadata: None,
    })
}

fn token_usage_with(last_total: i64, window: Option<i64>) -> ServerNotification {
    let breakdown = |total: i64| TokenUsageBreakdown {
        total_tokens: total,
        input_tokens: total,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
    };
    ServerNotification::ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification {
        thread_id: THREAD.to_string(),
        turn_id: TURN.to_string(),
        token_usage: ThreadTokenUsage {
            total: breakdown(last_total),
            last: breakdown(last_total),
            model_context_window: window,
        },
    })
}

fn token_usage(last_total: i64, window: i64) -> ServerNotification {
    token_usage_with(last_total, Some(window))
}

fn item_completed(item: ThreadItem) -> ServerNotification {
    ServerNotification::ItemCompleted(ItemCompletedNotification {
        item,
        thread_id: THREAD.to_string(),
        turn_id: TURN.to_string(),
        completed_at_ms: 0,
    })
}

fn record(kind: RecordedKind) -> RecordedEvent {
    RecordedEvent {
        thread_id: THREAD.to_string(),
        turn_id: Some(TURN.to_string()),
        kind,
    }
}

fn observe_all(notifications: &[ServerNotification]) -> Vec<RecordedEvent> {
    let mut adapter = ThreadProfilerAdapter::new();
    notifications
        .iter()
        .filter_map(|notification| adapter.observe(notification))
        .collect()
}

fn item_kinds(records: &[RecordedEvent]) -> Vec<(&str, u64)> {
    records
        .iter()
        .filter_map(|record| match &record.kind {
            RecordedKind::Item {
                item_kind,
                items_seq,
                ..
            } => Some((item_kind.as_str(), *items_seq)),
            _ => None,
        })
        .collect()
}

/// Findings 6.2: response outputs, then raw usage, then the tool output, then token usage.
#[test]
fn replays_measured_capture() {
    let mut notifications = vec![turn_started()];
    notifications.extend((1..6).map(|index| raw_item(message_item(&format!("input {index}")))));
    notifications.push(raw_item(reasoning_item()));
    notifications.push(raw_item(message_item("commentary")));
    notifications.push(raw_item(custom_tool_call("call_1")));
    notifications.push(raw_usage("resp_a", 25_230, 192));
    notifications.push(raw_item(custom_tool_call_output("call_1")));
    notifications.push(token_usage(25_422, 258_400));
    notifications.push(raw_item(reasoning_item()));
    notifications.push(raw_usage("resp_b", 29_137, 93));
    notifications.push(token_usage(29_230, 258_400));
    notifications.push(turn_completed(TurnStatus::Completed));

    let records = observe_all(&notifications);
    let non_item: Vec<RecordedKind> = records
        .iter()
        .filter(|record| !matches!(record.kind, RecordedKind::Item { .. }))
        .map(|record| record.kind.clone())
        .collect();

    assert_eq!(
        item_kinds(&records),
        vec![
            ("Message", 1),
            ("Message", 2),
            ("Message", 3),
            ("Message", 4),
            ("Message", 5),
            ("Reasoning", 6),
            ("Message", 7),
            ("CustomToolCall", 8),
            ("CustomToolCallOutput", 9),
            ("Reasoning", 10),
        ]
    );
    assert_eq!(
        non_item,
        vec![
            RecordedKind::TurnStarted,
            RecordedKind::Usage {
                response_id: "resp_a".to_string(),
                reported_context_tokens: 25_422,
                input_tokens: 25_230,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 192,
                reasoning_output_tokens: 0,
                items_seq: 8,
            },
            RecordedKind::WindowUpdated {
                window: 258_400,
                matches_anchor: Some(true),
            },
            RecordedKind::Usage {
                response_id: "resp_b".to_string(),
                reported_context_tokens: 29_230,
                input_tokens: 29_137,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 93,
                reasoning_output_tokens: 0,
                items_seq: 10,
            },
            RecordedKind::WindowUpdated {
                window: 258_400,
                matches_anchor: Some(true),
            },
            RecordedKind::TurnEnded {
                completed: true,
                status: "completed".to_string(),
            },
        ]
    );
    assert_eq!(records[1].thread_id, THREAD);
    assert_eq!(records[1].turn_id, Some(TURN.to_string()));
}

/// A token usage update without a window must not consume the pending anchor.
#[test]
fn a_windowless_update_keeps_the_anchor() {
    let records = observe_all(&[
        raw_usage("resp_a", 100, 10),
        token_usage_with(110, None),
        token_usage(110, 258_400),
    ]);

    assert_eq!(
        records,
        vec![
            record(RecordedKind::Usage {
                response_id: "resp_a".to_string(),
                reported_context_tokens: 110,
                input_tokens: 100,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 10,
                reasoning_output_tokens: 0,
                items_seq: 0,
            }),
            record(RecordedKind::WindowUpdated {
                window: 258_400,
                matches_anchor: Some(true),
            }),
        ]
    );
}

/// The adapter owns no accounting formula: the protocol's total is recorded as is.
#[test]
fn the_protocol_total_is_authoritative() {
    let inconsistent = ServerNotification::RawResponseCompleted(RawResponseCompletedNotification {
        thread_id: THREAD.to_string(),
        turn_id: TURN.to_string(),
        response_id: "resp_a".to_string(),
        usage: Some(TokenUsageBreakdown {
            total_tokens: 999,
            input_tokens: 100,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 10,
            reasoning_output_tokens: 0,
        }),
        usage_metadata: None,
    });
    let records = observe_all(&[inconsistent, token_usage(999, 258_400)]);

    assert_eq!(
        records,
        vec![
            record(RecordedKind::Usage {
                response_id: "resp_a".to_string(),
                reported_context_tokens: 999,
                input_tokens: 100,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 10,
                reasoning_output_tokens: 0,
                items_seq: 0,
            }),
            record(RecordedKind::WindowUpdated {
                window: 258_400,
                matches_anchor: Some(true),
            }),
        ]
    );
}

/// Findings 10.1: an interrupted turn strands items past the last anchor and sends no usage.
#[test]
fn interrupted_turn_reports_no_missing_usage() {
    let records = observe_all(&[
        raw_item(custom_tool_call("call_1")),
        raw_usage("resp_a", 42_648, 130),
        raw_item(message_item("output")),
        token_usage(42_778, 258_400),
        raw_item(message_item("stranded")),
        turn_completed(TurnStatus::Interrupted),
    ]);

    assert_eq!(
        records.last().map(|record| record.kind.clone()),
        Some(RecordedKind::TurnEnded {
            completed: false,
            status: "interrupted".to_string(),
        })
    );
    assert!(
        !records
            .iter()
            .any(|record| matches!(record.kind, RecordedKind::MissingUsage { .. }))
    );
}

#[test]
fn absent_usage_clears_the_anchor() {
    let records = observe_all(&[raw_usage_missing("resp_a"), token_usage(1_000, 258_400)]);

    assert_eq!(
        records,
        vec![
            record(RecordedKind::MissingUsage {
                response_id: "resp_a".to_string(),
            }),
            record(RecordedKind::WindowUpdated {
                window: 258_400,
                matches_anchor: None,
            }),
        ]
    );
}

#[test]
fn compaction_invalidates_and_other_items_are_ignored() {
    let records = observe_all(&[
        raw_usage("resp_a", 10, 5),
        item_completed(ThreadItem::ContextCompaction {
            id: "cmp_1".to_string(),
        }),
        item_completed(ThreadItem::ExitedReviewMode {
            id: "rev_1".to_string(),
            review: "done".to_string(),
        }),
        token_usage(15, 258_400),
    ]);

    assert_eq!(
        records,
        vec![
            record(RecordedKind::Usage {
                response_id: "resp_a".to_string(),
                reported_context_tokens: 15,
                input_tokens: 10,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 5,
                reasoning_output_tokens: 0,
                items_seq: 0,
            }),
            record(RecordedKind::Invalidated {
                reason: "compacted".to_string(),
            }),
            record(RecordedKind::WindowUpdated {
                window: 258_400,
                matches_anchor: None,
            }),
        ]
    );
}

#[test]
fn only_the_latest_anchor_is_compared() {
    let matching_second = observe_all(&[
        raw_usage("resp_a", 100, 10),
        raw_usage("resp_b", 200, 20),
        token_usage(220, 258_400),
    ]);
    let matching_first = observe_all(&[
        raw_usage("resp_a", 100, 10),
        raw_usage("resp_b", 200, 20),
        token_usage(110, 258_400),
    ]);

    assert_eq!(
        matching_second.last().map(|record| record.kind.clone()),
        Some(RecordedKind::WindowUpdated {
            window: 258_400,
            matches_anchor: Some(true),
        })
    );
    assert_eq!(
        matching_first.last().map(|record| record.kind.clone()),
        Some(RecordedKind::WindowUpdated {
            window: 258_400,
            matches_anchor: Some(false),
        })
    );
}

#[test]
fn serializes_one_line_per_variant() {
    let records = observe_all(&[
        turn_started(),
        raw_item(custom_tool_call("call_1")),
        raw_usage("resp_a", 25_230, 192),
        token_usage(25_422, 258_400),
        raw_usage_missing("resp_b"),
        item_completed(ThreadItem::ContextCompaction {
            id: "cmp_1".to_string(),
        }),
        turn_completed(TurnStatus::Completed),
    ]);
    let lines: Vec<String> = std::iter::once(ThreadProfilerAdapter::new().attached(THREAD))
        .chain(records)
        .map(|record| serde_json::to_string(&record).expect("record serializes"))
        .collect();

    assert_eq!(
        lines,
        vec![
            r#"{"thread_id":"th_1","turn_id":null,"kind":"attached"}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"turn_started"}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"item","item_kind":"CustomToolCall","bytes":74,"items_seq":1,"stamped_turn_id":null,"call_id":"call_1"}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"usage","response_id":"resp_a","reported_context_tokens":25422,"input_tokens":25230,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":192,"reasoning_output_tokens":0,"items_seq":1}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"window_updated","window":258400,"matches_anchor":true}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"missing_usage","response_id":"resp_b"}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"invalidated","reason":"compacted"}"#,
            r#"{"thread_id":"th_1","turn_id":"tu_1","kind":"turn_ended","completed":true,"status":"completed"}"#,
        ]
    );
}

#[test]
fn invalidate_clears_the_anchor() {
    let mut adapter = ThreadProfilerAdapter::new();
    adapter.observe(&raw_usage("resp_a", 100, 10));
    let invalidated = adapter.invalidate(THREAD, InvalidationReason::EventsDropped { skipped: 3 });
    let window = adapter.observe(&token_usage(110, 258_400));

    assert_eq!(
        invalidated,
        RecordedEvent {
            thread_id: THREAD.to_string(),
            turn_id: None,
            kind: RecordedKind::Invalidated {
                reason: "events_dropped(skipped=3)".to_string(),
            },
        }
    );
    assert_eq!(
        window,
        Some(record(RecordedKind::WindowUpdated {
            window: 258_400,
            matches_anchor: None,
        }))
    );
}

#[test]
fn attached_names_the_thread() {
    let adapter = ThreadProfilerAdapter::new();

    assert_eq!(
        adapter.attached(THREAD),
        RecordedEvent {
            thread_id: THREAD.to_string(),
            turn_id: None,
            kind: RecordedKind::Attached,
        }
    );
}

#[test]
fn burst_of_notifications_keeps_every_item_in_order() {
    let mut notifications = Vec::new();
    for index in 0..10_000 {
        if index % 10 == 9 {
            notifications.push(raw_usage(&format!("resp_{index}"), index, 1));
        } else {
            notifications.push(raw_item(message_item(&format!("item {index}"))));
        }
    }

    let records = observe_all(&notifications);
    let items = item_kinds(&records);

    assert_eq!(records.len(), 10_000);
    assert_eq!(items.len(), 9_000);
    assert_eq!(items.last(), Some(&("Message", 9_000)));
}
