//! Record shape and the append-only JSONL sink for profiler observations.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use codex_context_profiler::InvalidationReason;
use codex_context_profiler::ProfilerEvent;
use codex_context_profiler::call_id;
use codex_context_profiler::item_kind;
use codex_context_profiler::serialized_size;
use serde::Serialize;

use super::adapter::Observation;

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

/// Trace-lifecycle record written when the registry starts observing a thread.
pub(crate) fn attached_record(thread_id: &str) -> RecordedEvent {
    RecordedEvent {
        thread_id: thread_id.to_string(),
        turn_id: None,
        kind: RecordedKind::Attached,
    }
}

pub(crate) fn to_record(thread_id: &str, observation: &Observation<'_>) -> RecordedEvent {
    let fields = &observation.fields;
    let (turn_id, kind) = match &observation.event {
        ProfilerEvent::TurnStarted { turn_id } => {
            (Some(turn_id.to_string()), RecordedKind::TurnStarted)
        }
        ProfilerEvent::Item { turn_id, item } => (
            Some(turn_id.to_string()),
            RecordedKind::Item {
                item_kind: item_kind(item).to_string(),
                bytes: serialized_size(item).unwrap_or(0),
                items_seq: observation.items_seq,
                stamped_turn_id: item.turn_id().map(str::to_string),
                call_id: call_id(item),
            },
        ),
        ProfilerEvent::Usage { turn_id, usage } => (
            Some(turn_id.to_string()),
            RecordedKind::Usage {
                response_id: fields.response_id.clone().unwrap_or_default(),
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
                window: *window,
                matches_anchor: fields.matches_anchor,
            },
        ),
        ProfilerEvent::TurnEnded { turn_id, completed } => (
            Some(turn_id.to_string()),
            RecordedKind::TurnEnded {
                completed: *completed,
                status: fields.status.clone().unwrap_or_default(),
            },
        ),
        ProfilerEvent::UsageMissing { turn_id } => (
            Some(turn_id.to_string()),
            RecordedKind::MissingUsage {
                response_id: fields.response_id.clone().unwrap_or_default(),
            },
        ),
        ProfilerEvent::Invalidated { reason } => (
            fields.turn_id.clone(),
            RecordedKind::Invalidated {
                reason: invalidation_reason(reason),
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

pub(crate) struct ProfilerLog {
    file: Mutex<File>,
}

impl ProfilerLog {
    /// One file per process; the pid keeps same-second starts from sharing a trace.
    pub(crate) fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let filename = format!(
            "context-profiler-{}-{}.jsonl",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        );

        let mut opts = OpenOptions::new();
        opts.create(true).append(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        Ok(Self::with_file(opts.open(dir.join(filename))?))
    }

    pub(crate) fn with_file(file: File) -> Self {
        Self {
            file: Mutex::new(file),
        }
    }

    pub(crate) fn write(&self, record: &RecordedEvent) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.write_all(line.as_bytes())?;
        guard.flush()
    }
}
