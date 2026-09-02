//! Bridges app-server v2 notifications into context-profiler events.
//!
//! One `ThreadProfilerAdapter` per thread turns the notification stream into
//! `ProfilerEvent`s and owned `RecordedEvent`s that the registry serializes to a
//! JSONL trace. Tracing is best effort: any write failure drops the trace rather
//! than degrading the session.

mod adapter;
mod log;

use std::collections::HashMap;

use codex_app_server_protocol::ServerNotification;
use codex_context_profiler::InvalidationReason;
use codex_protocol::ThreadId;

use crate::legacy_core::config::Config;
use adapter::RecordedEvent;
use adapter::ThreadProfilerAdapter;
use log::ProfilerLog;

/// Owns one adapter per observed thread plus the trace file they share.
pub(crate) struct ProfilerRegistry {
    threads: HashMap<ThreadId, ThreadProfilerAdapter>,
    writer: Option<ProfilerLog>,
}

impl ProfilerRegistry {
    pub(crate) fn disabled() -> Self {
        Self {
            threads: HashMap::new(),
            writer: None,
        }
    }

    pub(crate) fn enabled(config: &Config) -> Self {
        match ProfilerLog::open(&config.log_dir) {
            Ok(writer) => Self {
                threads: HashMap::new(),
                writer: Some(writer),
            },
            Err(error) => {
                tracing::warn!(%error, "context profiler disabled: failed to open trace log");
                Self::disabled()
            }
        }
    }

    /// `allow_create` gates attaching to a thread the user is not looking at.
    pub(crate) fn observe(
        &mut self,
        thread_id: &ThreadId,
        notification: &ServerNotification,
        allow_create: bool,
    ) {
        if self.writer.is_none() {
            return;
        }
        if !self.threads.contains_key(thread_id) {
            if !allow_create {
                return;
            }
            let adapter = ThreadProfilerAdapter::new();
            let attached = adapter.attached(&thread_id.to_string());
            self.threads.insert(*thread_id, adapter);
            self.write(&attached);
        }
        let record = match self.threads.get_mut(thread_id) {
            Some(adapter) => adapter.observe(notification),
            None => return,
        };
        if let Some(record) = record {
            self.write(&record);
        }
    }

    /// Lag is connection-level, so every live adapter loses its anchor.
    pub(crate) fn broadcast_lagged(&mut self, skipped: usize) {
        if self.writer.is_none() {
            return;
        }
        let records: Vec<RecordedEvent> = self
            .threads
            .iter_mut()
            .map(|(thread_id, adapter)| {
                adapter.invalidate(
                    &thread_id.to_string(),
                    InvalidationReason::EventsDropped { skipped },
                )
            })
            .collect();
        for record in &records {
            self.write(record);
        }
    }

    pub(crate) fn remove(&mut self, thread_id: &ThreadId) {
        self.threads.remove(thread_id);
    }

    fn write(&mut self, record: &RecordedEvent) {
        let Some(writer) = self.writer.as_ref() else {
            return;
        };
        if let Err(error) = writer.write(record) {
            tracing::warn!(%error, "context profiler disabled: trace log write failed");
            self.writer = None;
        }
    }
}

#[cfg(test)]
#[path = "context_profiler/registry_tests.rs"]
mod tests;
