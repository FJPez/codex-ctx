//! Bridges app-server v2 notifications into context-profiler events.
//!
//! One `ThreadProfilerAdapter` per thread turns the notification stream into
//! `ProfilerEvent`s and owned `RecordedEvent`s that a trace sink can serialize.

#![allow(dead_code, unused_imports)]

mod adapter;

pub(crate) use adapter::RecordedEvent;
pub(crate) use adapter::RecordedKind;
pub(crate) use adapter::ThreadProfilerAdapter;
