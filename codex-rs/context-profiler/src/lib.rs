//! Accumulates model-context profiling data for the Codex TUI's `/ctx` view.
//!
//! An adapter feeds this crate `ProfilerEvent`s as a session progresses, and the
//! profiler produces `ContextSnapshot`s describing what currently occupies the context window.

mod event;
mod item;
mod snapshot;
mod usage;
