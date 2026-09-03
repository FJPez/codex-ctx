//! Folds `ProfilerEvent`s into `ProfilerState`.
//!
//! `items_seen`, and every `ItemSummary::seq` derived from it, counts the items this profiler was
//! shown; it is never the number of items the server holds, nor the size of its context.

use std::collections::HashMap;

use codex_protocol::models::ResponseItem;

use crate::event::ProfilerEvent;
use crate::item::Category;
use crate::item::GroupKey;
use crate::item::ItemGroup;
use crate::item::ItemSummary;
use crate::item::TokenCost;
use crate::snapshot::ProfilerState;
use crate::snapshot::TurnDelta;

/// How a turn stopped, since only a completed turn has a trustworthy closing anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOutcome {
    Completed,
    Incomplete,
}

/// The turn currently being folded; becomes a `TurnDelta` when it closes.
#[derive(Debug)]
struct OpenTurn {
    turn_id: String,
    index: u32,
    first_seq: u64,
    measured_before: Option<i64>,
    last_anchor: Option<i64>,
}

#[derive(Debug, Default)]
pub struct ContextProfiler {
    state: ProfilerState,
    /// Items observed so far; incremented before it is stamped, matching the TUI adapter.
    items_seen: u64,
    /// Survives turn ends: the next turn's `measured_before`.
    last_anchor_total: Option<i64>,
    open_turn: Option<OpenTurn>,
}

impl ContextProfiler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &ProfilerState {
        &self.state
    }

    pub fn observe(&mut self, event: ProfilerEvent<'_>) {
        if self.state.invalidated.is_some() {
            if let ProfilerEvent::WindowUpdated { window, .. } = event {
                self.state.snapshot.window = Some(window);
            }
            return;
        }
        match event {
            ProfilerEvent::TurnStarted { turn_id } => {
                self.close_turn(TurnOutcome::Incomplete);
                self.open_turn(turn_id);
            }
            ProfilerEvent::Item { turn_id, item } => {
                let turn_index = self.turn_index(turn_id);
                self.items_seen += 1;
                let seq = self.items_seen;
                let bytes = serde_json::to_vec(item).map(|json| json.len()).unwrap_or(0);
                let group = match call_id(item) {
                    Some(call_id) => GroupKey::ToolCall(call_id),
                    None => GroupKey::Ungrouped(seq),
                };
                self.state.snapshot.items.push(ItemSummary {
                    seq,
                    turn_index,
                    category: category(item),
                    bytes,
                    cost: TokenCost::Estimated(byte_proxy(bytes)),
                    label: item_kind(item).to_string(),
                    group,
                    item_id: item.id().map(ToString::to_string),
                });
                self.rebuild_aggregates();
            }
            ProfilerEvent::Usage { turn_id, usage } => {
                self.turn_index(turn_id);
                if usage.items_seq != self.items_seen {
                    self.state.seq_mismatch_count += 1;
                }
                let total = usage.reported_context_tokens;
                self.state.snapshot.reported_context_tokens = Some(total);
                self.last_anchor_total = Some(total);
                if let Some(turn) = self.open_turn.as_mut() {
                    turn.last_anchor = Some(total);
                }
                self.state.anchors.push(usage);
            }
            ProfilerEvent::WindowUpdated { window, .. } => {
                self.state.snapshot.window = Some(window);
            }
            ProfilerEvent::TurnEnded { completed, .. } => {
                let outcome = if completed {
                    TurnOutcome::Completed
                } else {
                    TurnOutcome::Incomplete
                };
                self.close_turn(outcome);
            }
            ProfilerEvent::Invalidated { reason } => {
                self.state.invalidated = Some(reason);
            }
        }
    }

    /// Index of the open turn, opening an implicit one for events that arrive outside a turn.
    fn turn_index(&mut self, turn_id: &str) -> u32 {
        match self.open_turn.as_ref() {
            Some(turn) => turn.index,
            None => self.open_turn(turn_id),
        }
    }

    fn open_turn(&mut self, turn_id: &str) -> u32 {
        let index = self.state.snapshot.turns.len() as u32;
        self.open_turn = Some(OpenTurn {
            turn_id: turn_id.to_string(),
            index,
            first_seq: self.items_seen + 1,
            measured_before: self.last_anchor_total,
            last_anchor: None,
        });
        index
    }

    fn close_turn(&mut self, outcome: TurnOutcome) {
        let Some(turn) = self.open_turn.take() else {
            return;
        };
        let item_seq_range = turn.first_seq..=self.items_seen;
        let estimated_added = self
            .state
            .snapshot
            .items
            .iter()
            .filter(|item| item_seq_range.contains(&item.seq))
            .map(|item| item.cost.tokens())
            .sum();
        let measured_after = match outcome {
            TurnOutcome::Completed => turn.last_anchor,
            TurnOutcome::Incomplete => None,
        };
        self.state.snapshot.turns.push(TurnDelta {
            turn_id: turn.turn_id,
            index: turn.index,
            item_seq_range,
            estimated_added,
            measured_before: turn.measured_before,
            measured_after,
        });
    }

    /// Groups and category totals are derived views of `snapshot.items`, so they are rebuilt whole.
    fn rebuild_aggregates(&mut self) {
        let items = &self.state.snapshot.items;
        let mut groups: Vec<ItemGroup> = Vec::new();
        let mut positions: HashMap<&GroupKey, usize> = HashMap::new();
        let mut totals: HashMap<Category, TokenCost> = HashMap::new();
        for item in items {
            match positions.get(&item.group) {
                Some(&position) => {
                    let group = &mut groups[position];
                    group.cost = group.cost.combine(item.cost);
                    group.members.push(item.seq);
                }
                None => {
                    positions.insert(&item.group, groups.len());
                    groups.push(ItemGroup {
                        key: item.group.clone(),
                        category: item.category,
                        cost: item.cost,
                        label: item.label.clone(),
                        members: vec![item.seq],
                    });
                }
            }
            totals
                .entry(item.category)
                .and_modify(|total| *total = total.combine(item.cost))
                .or_insert(item.cost);
        }
        let mut by_category: Vec<(Category, TokenCost)> = totals.into_iter().collect();
        by_category.sort_by_key(|(category, _)| category.ordinal());
        self.state.snapshot.groups = groups;
        self.state.snapshot.by_category = by_category;
    }
}

/// Crude placeholder cost, replaced by the M2d estimator.
fn byte_proxy(bytes: usize) -> i64 {
    (bytes / 4) as i64
}

/// Exhaustive so a new upstream `ResponseItem` variant fails the build.
fn category(item: &ResponseItem) -> Category {
    match item {
        ResponseItem::Reasoning { .. } => Category::Reasoning,
        ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. } => Category::ToolCall,
        ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. } => Category::ToolOutput,
        ResponseItem::Message { role, .. } if role == "user" => Category::UserMessage,
        ResponseItem::Message { .. } | ResponseItem::AgentMessage { .. } => Category::AgentMessage,
        ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. } => Category::Compaction,
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::Other => Category::Other,
    }
}

fn item_kind(item: &ResponseItem) -> &'static str {
    match item {
        ResponseItem::AdditionalTools { .. } => "AdditionalTools",
        ResponseItem::Message { .. } => "Message",
        ResponseItem::AgentMessage { .. } => "AgentMessage",
        ResponseItem::Reasoning { .. } => "Reasoning",
        ResponseItem::LocalShellCall { .. } => "LocalShellCall",
        ResponseItem::FunctionCall { .. } => "FunctionCall",
        ResponseItem::ToolSearchCall { .. } => "ToolSearchCall",
        ResponseItem::FunctionCallOutput { .. } => "FunctionCallOutput",
        ResponseItem::CustomToolCall { .. } => "CustomToolCall",
        ResponseItem::CustomToolCallOutput { .. } => "CustomToolCallOutput",
        ResponseItem::ToolSearchOutput { .. } => "ToolSearchOutput",
        ResponseItem::WebSearchCall { .. } => "WebSearchCall",
        ResponseItem::ImageGenerationCall { .. } => "ImageGenerationCall",
        ResponseItem::Compaction { .. } => "Compaction",
        ResponseItem::CompactionTrigger { .. } => "CompactionTrigger",
        ResponseItem::ConfigurationUpdate { .. } => "ConfigurationUpdate",
        ResponseItem::ContextCompaction { .. } => "ContextCompaction",
        ResponseItem::Other => "Other",
    }
}

/// Mirrors the TUI adapter so the two agree on which items pair into one group.
fn call_id(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::LocalShellCall { call_id, .. }
        | ResponseItem::ToolSearchCall { call_id, .. }
        | ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::ToolSearchOutput { call_id, .. } => call_id.clone(),
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.clone()),
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::Message { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => None,
    }
}

#[cfg(test)]
#[path = "profiler_tests.rs"]
mod tests;
