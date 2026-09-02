use super::*;
use codex_app_server_protocol::RawResponseItemCompletedNotification;
use codex_protocol::models::ContentItem;
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
    }
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
}

#[test]
fn the_log_appends_one_json_object_per_line() {
    let dir = TempDir::new().expect("tempdir");
    let log = ProfilerLog::open(dir.path()).expect("trace log opens");
    let adapter = ThreadProfilerAdapter::new();

    for thread_id in ["th_1", "th_2", "th_3"] {
        log.write(&adapter.attached(thread_id)).expect("write");
    }

    assert_eq!(
        records(dir.path())
            .iter()
            .map(|record| record["thread_id"].as_str().expect("thread_id"))
            .collect::<Vec<_>>(),
        vec!["th_1", "th_2", "th_3"]
    );
}
