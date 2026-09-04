use super::*;
use crate::context_profiler::log::attached_record;
use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::RawResponseItemCompletedNotification;
use codex_app_server_protocol::TokenUsageBreakdown;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;

fn raw_item(thread_id: ThreadId, text: &str) -> ServerNotification {
    ServerNotification::RawResponseItemCompleted(RawResponseItemCompletedNotification {
        thread_id: thread_id.to_string(),
        turn_id: "tu_1".to_string(),
        item: ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    })
}

fn registry_in(dir: &Path) -> ProfilerRegistry {
    ProfilerRegistry {
        threads: HashMap::new(),
        writer: Some(ProfilerLog::open(dir).expect("trace log opens")),
        enabled: true,
    }
}

/// A first request: one turn, one user item, and the anchor that prices it.
/// The anchor carries no output tokens, so the whole unattributed residual is the baseline.
fn first_request(thread_id: ThreadId) -> Vec<ServerNotification> {
    vec![
        turn_started(thread_id),
        user_item(thread_id, "hello"),
        raw_usage(
            thread_id, /*input_tokens*/ 1_200, /*output_tokens*/ 0,
        ),
    ]
}

fn user_item(thread_id: ThreadId, text: &str) -> ServerNotification {
    ServerNotification::RawResponseItemCompleted(RawResponseItemCompletedNotification {
        thread_id: thread_id.to_string(),
        turn_id: "tu_1".to_string(),
        item: ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: Some(
                InternalChatMessageMetadataPassthrough {
                    content_item_kinds: Some(vec![ContentItemKind("user.text".to_string())]),
                    ..Default::default()
                },
            ),
        },
    })
}

fn turn_started(thread_id: ThreadId) -> ServerNotification {
    ServerNotification::TurnStarted(TurnStartedNotification {
        thread_id: thread_id.to_string(),
        turn: Turn {
            id: "tu_1".to_string(),
            items: Vec::new(),
            items_view: TurnItemsView::NotLoaded,
            status: TurnStatus::InProgress,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        },
    })
}

fn raw_usage(thread_id: ThreadId, input_tokens: i64, output_tokens: i64) -> ServerNotification {
    ServerNotification::RawResponseCompleted(RawResponseCompletedNotification {
        thread_id: thread_id.to_string(),
        turn_id: "tu_1".to_string(),
        response_id: "resp_a".to_string(),
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

fn folded_items(registry: &ProfilerRegistry, thread_id: &ThreadId) -> usize {
    registry
        .state(thread_id)
        .expect("thread is profiled")
        .snapshot
        .items
        .len()
}

fn trace_path(dir: &Path) -> PathBuf {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("trace dir readable")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    assert_eq!(paths.len(), 1, "exactly one trace file per registry");
    paths.pop().expect("trace file")
}

fn records(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(trace_path(dir))
        .expect("trace readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect()
}

fn kinds(dir: &Path) -> Vec<String> {
    records(dir)
        .iter()
        .map(|record| {
            record["kind"]
                .as_str()
                .expect("kind is a string")
                .to_string()
        })
        .collect()
}

#[test]
fn attaching_writes_attached_before_observed_records() {
    let dir = TempDir::new().expect("tempdir");
    let thread_id = ThreadId::new();
    let mut registry = registry_in(dir.path());

    registry.observe(
        &thread_id,
        &raw_item(thread_id, "one"),
        /*allow_create*/ true,
    );
    registry.observe(
        &thread_id,
        &raw_item(thread_id, "two"),
        /*allow_create*/ true,
    );

    let records = records(dir.path());
    assert_eq!(
        records
            .iter()
            .map(|record| record["kind"].as_str().expect("kind"))
            .collect::<Vec<_>>(),
        vec!["attached", "item", "item"]
    );
    assert_eq!(records[0]["thread_id"], thread_id.to_string());
    assert_eq!(records[1]["items_seq"], 1);
    assert_eq!(records[2]["items_seq"], 2);
}

#[test]
fn background_threads_are_not_attached_until_displayed() {
    let dir = TempDir::new().expect("tempdir");
    let thread_id = ThreadId::new();
    let mut registry = registry_in(dir.path());

    registry.observe(
        &thread_id,
        &raw_item(thread_id, "hidden"),
        /*allow_create*/ false,
    );
    assert_eq!(kinds(dir.path()), Vec::<String>::new());

    registry.observe(
        &thread_id,
        &raw_item(thread_id, "visible"),
        /*allow_create*/ true,
    );
    assert_eq!(kinds(dir.path()), vec!["attached", "item"]);
}

#[test]
fn lag_invalidates_every_live_adapter() {
    let dir = TempDir::new().expect("tempdir");
    let first = ThreadId::new();
    let second = ThreadId::new();
    let mut registry = registry_in(dir.path());
    registry.observe(&first, &raw_item(first, "a"), /*allow_create*/ true);
    registry.observe(&second, &raw_item(second, "b"), /*allow_create*/ true);

    registry.broadcast_lagged(7);

    let invalidated: Vec<(String, String)> = records(dir.path())
        .iter()
        .filter(|record| record["kind"] == "invalidated")
        .map(|record| {
            (
                record["thread_id"].as_str().expect("thread_id").to_string(),
                record["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect();
    let mut thread_ids: Vec<String> = invalidated
        .iter()
        .map(|(thread_id, _)| thread_id.clone())
        .collect();
    thread_ids.sort();
    let mut expected = vec![first.to_string(), second.to_string()];
    expected.sort();

    assert_eq!(thread_ids, expected);
    assert!(
        invalidated
            .iter()
            .all(|(_, reason)| reason == "events_dropped(skipped=7)")
    );
}

#[test]
fn disabled_registry_records_nothing() {
    let thread_id = ThreadId::new();
    let mut registry = ProfilerRegistry::disabled();

    registry.observe(
        &thread_id,
        &raw_item(thread_id, "one"),
        /*allow_create*/ true,
    );
    registry.broadcast_lagged(3);

    assert!(registry.threads.is_empty());
}

#[test]
fn reattaching_restarts_the_item_sequence() {
    let dir = TempDir::new().expect("tempdir");
    let thread_id = ThreadId::new();
    let mut registry = registry_in(dir.path());
    registry.observe(
        &thread_id,
        &raw_item(thread_id, "before"),
        /*allow_create*/ true,
    );

    registry.remove(&thread_id);
    registry.observe(
        &thread_id,
        &raw_item(thread_id, "after"),
        /*allow_create*/ true,
    );

    let records = records(dir.path());
    assert_eq!(
        kinds(dir.path()),
        vec!["attached", "item", "attached", "item"]
    );
    assert_eq!(records[1]["items_seq"], 1);
    assert_eq!(records[3]["items_seq"], 1);
}

#[test]
fn a_failed_write_drops_the_writer() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("read-only.jsonl");
    std::fs::write(&path, "").expect("seed trace file");
    let read_only = OpenOptions::new()
        .read(true)
        .open(&path)
        .expect("trace file opens read-only");
    let thread_id = ThreadId::new();
    let mut registry = ProfilerRegistry {
        threads: HashMap::new(),
        writer: Some(ProfilerLog::with_file(read_only)),
        enabled: true,
    };

    registry.observe(
        &thread_id,
        &raw_item(thread_id, "one"),
        /*allow_create*/ true,
    );
    assert!(registry.writer.is_none());

    registry.observe(
        &thread_id,
        &raw_item(thread_id, "two"),
        /*allow_create*/ true,
    );
    registry.broadcast_lagged(1);

    assert_eq!(std::fs::read_to_string(&path).expect("trace readable"), "");
    assert_eq!(folded_items(&registry, &thread_id), 2);
}

#[tokio::test]
async fn a_failed_open_keeps_profiling() {
    let dir = TempDir::new().expect("tempdir");
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "").expect("seed blocking file");
    let mut config = Config::load_default_with_cli_overrides_for_codex_home(
        dir.path().to_path_buf(),
        Vec::new(),
    )
    .await
    .expect("config");
    // A directory cannot be created under a regular file, so the trace log cannot open.
    config.log_dir = blocker.join("traces");
    let thread_id = ThreadId::new();
    let mut registry = ProfilerRegistry::enabled(&config);
    assert!(registry.writer.is_none());

    for notification in first_request(thread_id) {
        registry.observe(&thread_id, &notification, /*allow_create*/ true);
    }

    assert!(registry.state(&thread_id).is_some());
    assert_eq!(folded_items(&registry, &thread_id), 1);
}

#[test]
fn thread_started_creates_a_session_start_profiler() {
    let dir = TempDir::new().expect("tempdir");
    let thread_id = ThreadId::new();
    let mut registry = registry_in(dir.path());

    registry.thread_started(&thread_id);
    for notification in first_request(thread_id) {
        registry.observe(&thread_id, &notification, /*allow_create*/ true);
    }

    assert!(
        registry
            .state(&thread_id)
            .expect("thread is profiled")
            .snapshot
            .baseline_tokens
            .is_some()
    );
    assert_eq!(kinds(dir.path())[0], "attached");
}

#[test]
fn lazy_attachment_is_mid_stream() {
    let dir = TempDir::new().expect("tempdir");
    let thread_id = ThreadId::new();
    let mut registry = registry_in(dir.path());

    for notification in first_request(thread_id) {
        registry.observe(&thread_id, &notification, /*allow_create*/ true);
    }

    assert!(
        registry
            .state(&thread_id)
            .expect("thread is profiled")
            .snapshot
            .baseline_tokens
            .is_none()
    );
}

#[test]
fn the_log_appends_one_json_object_per_line() {
    let dir = TempDir::new().expect("tempdir");
    let log = ProfilerLog::open(dir.path()).expect("trace log opens");

    for thread_id in ["th_1", "th_2", "th_3"] {
        log.write(&attached_record(thread_id)).expect("write");
    }

    assert_eq!(
        records(dir.path())
            .iter()
            .map(|record| record["thread_id"].as_str().expect("thread_id"))
            .collect::<Vec<_>>(),
        vec!["th_1", "th_2", "th_3"]
    );
}
