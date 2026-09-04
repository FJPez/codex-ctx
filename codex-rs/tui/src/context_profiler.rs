//! Bridges app-server v2 notifications into context-profiler events.
//!
//! One `ThreadProfilerAdapter` per thread turns the notification stream into
//! `ProfilerEvent`s, which are folded into that thread's `ContextProfiler` and
//! mirrored into a JSONL trace. The trace is best effort: any failure to open or
//! write it drops the trace only, never the profiling.

mod adapter;
mod log;

use std::collections::HashMap;

use codex_app_server_protocol::ServerNotification;
use codex_context_profiler::ContextProfiler;
use codex_context_profiler::InvalidationReason;
use codex_context_profiler::ObservationStart;
use codex_context_profiler::ProfilerState;
use codex_protocol::ThreadId;

use crate::legacy_core::config::Config;
use adapter::ThreadProfilerAdapter;
use log::ProfilerLog;
use log::RecordedEvent;
use log::attached_record;
use log::to_record;

/// The adapter and profiler for one observed thread.
struct ObservedThread {
    adapter: ThreadProfilerAdapter,
    profiler: ContextProfiler,
}

/// Owns one profiler per observed thread plus the trace file they share.
pub(crate) struct ProfilerRegistry {
    threads: HashMap<ThreadId, ObservedThread>,
    writer: Option<ProfilerLog>,
    enabled: bool,
}

impl ProfilerRegistry {
    pub(crate) fn disabled() -> Self {
        Self {
            threads: HashMap::new(),
            writer: None,
            enabled: false,
        }
    }

    pub(crate) fn enabled(config: &Config) -> Self {
        let writer = match ProfilerLog::open(&config.log_dir) {
            Ok(writer) => Some(writer),
            Err(error) => {
                tracing::warn!(%error, "context profiler trace disabled: failed to open trace log");
                None
            }
        };
        Self {
            threads: HashMap::new(),
            writer,
            enabled: true,
        }
    }

    /// `allow_create` gates attaching to a thread the user is not looking at.
    pub(crate) fn observe(
        &mut self,
        thread_id: &ThreadId,
        notification: &ServerNotification,
        allow_create: bool,
    ) {
        if !self.enabled {
            return;
        }
        if !self.threads.contains_key(thread_id) {
            if !allow_create {
                return;
            }
            self.attach(thread_id, ObservationStart::MidStream);
        }
        let Some(observed) = self.threads.get_mut(thread_id) else {
            return;
        };
        let Some(observation) = observed.adapter.observe(notification) else {
            return;
        };
        let record = self
            .writer
            .as_ref()
            .map(|_| to_record(&thread_id.to_string(), &observation));
        observed.profiler.observe(observation.event);
        if let Some(record) = record {
            self.write(&record);
        }
    }

    /// Eager attachment for a thread this app started fresh, so a baseline is claimable.
    pub(crate) fn thread_started(&mut self, thread_id: &ThreadId) {
        if !self.enabled || self.threads.contains_key(thread_id) {
            return;
        }
        self.attach(thread_id, ObservationStart::SessionStart);
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by the upcoming /ctx view")
    )]
    pub(crate) fn state(&self, thread_id: &ThreadId) -> Option<&ProfilerState> {
        self.threads
            .get(thread_id)
            .map(|observed| observed.profiler.state())
    }

    /// Lag is connection-level, so every live profiler loses its anchor.
    pub(crate) fn broadcast_lagged(&mut self, skipped: usize) {
        if !self.enabled {
            return;
        }
        let mut records = Vec::new();
        for (thread_id, observed) in self.threads.iter_mut() {
            let observation = observed
                .adapter
                .invalidate(InvalidationReason::EventsDropped { skipped });
            if self.writer.is_some() {
                records.push(to_record(&thread_id.to_string(), &observation));
            }
            observed.profiler.observe(observation.event);
        }
        for record in &records {
            self.write(record);
        }
    }

    pub(crate) fn remove(&mut self, thread_id: &ThreadId) {
        self.threads.remove(thread_id);
    }

    fn attach(&mut self, thread_id: &ThreadId, start: ObservationStart) {
        self.threads.insert(
            *thread_id,
            ObservedThread {
                adapter: ThreadProfilerAdapter::new(),
                profiler: ContextProfiler::new(start),
            },
        );
        self.write(&attached_record(&thread_id.to_string()));
    }

    fn write(&mut self, record: &RecordedEvent) {
        let Some(writer) = self.writer.as_ref() else {
            return;
        };
        if let Err(error) = writer.write(record) {
            tracing::warn!(%error, "context profiler trace disabled: trace log write failed");
            self.writer = None;
        }
    }
}

#[cfg(test)]
#[path = "context_profiler/registry_tests.rs"]
mod tests;
