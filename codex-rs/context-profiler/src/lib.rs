//! Accumulates model-context profiling data for the Codex TUI's `/ctx` view.
//!
//! An adapter feeds this crate `ProfilerEvent`s as a session progresses, and the
//! profiler produces `ContextSnapshot`s describing what currently occupies the context window.

mod classify;
mod estimate;
mod event;
mod item;
mod kind;
mod profiler;
mod snapshot;
mod usage;

pub use estimate::serialized_size;
pub use event::InvalidationReason;
pub use event::ProfilerEvent;
pub use item::Category;
pub use item::ClassificationWarning;
pub use item::ContentPart;
pub use item::GroupKey;
pub use item::ItemGroup;
pub use item::ItemSummary;
pub use item::PartMedia;
pub use item::PricingKind;
pub use item::TokenCost;
pub use kind::call_id;
pub use kind::item_kind;
pub use profiler::ContextProfiler;
pub use snapshot::ContextSnapshot;
pub use snapshot::InitialContextSummary;
pub use snapshot::ProfilerState;
pub use snapshot::TurnDelta;
pub use usage::UsageSnapshot;
