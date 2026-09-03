//! Folds `ProfilerEvent`s into `ProfilerState`.
//!
//! `items_seen`, and every `ItemSummary::seq` derived from it, counts the items this profiler was
//! shown; it is never the number of items the server holds, nor the size of its context.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use crate::classify::classify;
use crate::estimate::item_tokens;
use crate::estimate::serialized_size;
use crate::event::InvalidationReason;
use crate::event::ProfilerEvent;
use crate::item::Category;
use crate::item::GroupKey;
use crate::item::ItemGroup;
use crate::item::ItemSummary;
use crate::item::PricingKind;
use crate::item::TokenCost;
use crate::kind::call_id;
use crate::kind::item_kind;
use crate::snapshot::ProfilerState;
use crate::snapshot::TurnDelta;
use crate::usage::UsageSnapshot;

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
    /// `items_seen` when this turn's last anchor arrived; the start of the next attribution span.
    last_anchor_seq: Option<u64>,
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
                let size = serialized_size(item);
                if size.is_none() {
                    self.state.unsizable_item_count += 1;
                }
                let group = match call_id(item) {
                    Some(call_id) => GroupKey::ToolCall(call_id),
                    None => GroupKey::Ungrouped(seq),
                };
                let classification = classify(item);
                if classification.warned {
                    self.state.classification_warning_count += 1;
                }
                let estimate = size.map_or(0, |bytes| {
                    item_tokens(classification.category, &classification.parts, bytes)
                });
                self.state.snapshot.items.push(ItemSummary {
                    seq,
                    turn_index,
                    category: classification.category,
                    pricing: classification.pricing,
                    bytes: size.unwrap_or(0),
                    cost: TokenCost::Estimated(estimate),
                    label: item_kind(item).as_str().to_string(),
                    group,
                    item_id: item.id().map(ToString::to_string),
                    parts: classification.parts,
                });
                self.rebuild_aggregates();
            }
            ProfilerEvent::Usage { turn_id, usage } => {
                self.turn_index(turn_id);
                if usage.items_seq != self.items_seen {
                    self.state.invalidated = Some(InvalidationReason::SequenceMismatch {
                        anchor_items_seen: usage.items_seq,
                        profiler_items_seen: self.items_seen,
                    });
                    return;
                }
                let total = usage.reported_context_tokens;
                self.attribute_span(&usage);
                self.state.snapshot.reported_context_tokens = Some(total);
                self.last_anchor_total = Some(total);
                let items_seen = self.items_seen;
                if let Some(turn) = self.open_turn.as_mut() {
                    turn.last_anchor = Some(total);
                    turn.last_anchor_seq = Some(items_seen);
                }
                self.state.anchors.push(usage);
            }
            ProfilerEvent::UsageMissing { turn_id } => {
                self.turn_index(turn_id);
                self.last_anchor_total = None;
                let items_seen = self.items_seen;
                if let Some(turn) = self.open_turn.as_mut() {
                    turn.last_anchor = None;
                    turn.last_anchor_seq = Some(items_seen);
                }
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
            last_anchor_seq: None,
        });
        index
    }

    /// Prices the items observed since this turn's previous anchor from the measured usage.
    ///
    /// Output-kind items are priced at every anchor, since `output_tokens` is an absolute. Input-kind
    /// items need a pair of same-turn anchors, because only a delta reveals what the next request
    /// serialised. Items left over when a turn closes keep their estimates forever.
    ///
    /// `reasoning_output_tokens` is a documented subset of `output_tokens` (the API's
    /// `output_tokens_details.reasoning_tokens`), so the output pass splits in two: reasoning items
    /// share the subset and the rest share what is left. Reasoning is priced per byte nothing like
    /// prose, so mixing the two into one apportionment is what the split exists to avoid.
    ///
    /// One `Ambiguous` item poisons its whole span: neither measured total can be divided when part
    /// of the span may have landed in the other one, so the span keeps its estimates. The anchor is
    /// still recorded and still closes the span, so the next span prices normally.
    ///
    /// Reasoning tokens split off from the output pool only when the anchor reports them; a zero
    /// is what an unreported `output_tokens_details` reads as, never evidence of free reasoning.
    fn attribute_span(&mut self, usage: &UsageSnapshot) {
        let Some(turn) = self.open_turn.as_ref() else {
            return;
        };
        let span = turn.last_anchor_seq.map_or(turn.first_seq, |seq| seq + 1)..=self.items_seen;
        let previous_total = turn.last_anchor;
        if self.span_is_ambiguous(&span) {
            return;
        }
        let output = self.positions(&span, PricingKind::Output);
        if usage.reasoning_output_tokens > 0 {
            let (reasoning, generated): (Vec<usize>, Vec<usize>) =
                output.into_iter().partition(|&position| {
                    self.state.snapshot.items[position].category == Category::Reasoning
                });
            self.reprice(&reasoning, usage.reasoning_output_tokens);
            self.reprice(
                &generated,
                (usage.output_tokens - usage.reasoning_output_tokens).max(0),
            );
        } else {
            self.reprice(&output, usage.output_tokens);
        }
        if let Some(previous_total) = previous_total {
            let delta = usage.input_tokens - previous_total;
            if delta >= 0 {
                let input = self.positions(&span, PricingKind::Input);
                self.reprice(&input, delta);
            }
        }
        self.rebuild_aggregates();
    }

    fn span_is_ambiguous(&self, span: &RangeInclusive<u64>) -> bool {
        self.state
            .snapshot
            .items
            .iter()
            .any(|item| span.contains(&item.seq) && item.pricing == PricingKind::Ambiguous)
    }

    fn positions(&self, span: &RangeInclusive<u64>, kind: PricingKind) -> Vec<usize> {
        self.state
            .snapshot
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| span.contains(&item.seq) && item.pricing == kind)
            .map(|(position, _)| position)
            .collect()
    }

    /// A whole total on a lone item is exact; a share of one is not.
    ///
    /// Shares are weighted by each item's current cost, which is still its initial estimate: an item
    /// is repriced exactly once, when its span closes, and the passes above weight disjoint sets.
    /// Bytes would be the wrong weight, since an image contributes tens of kilobytes of base64 for
    /// roughly the tokens of a paragraph.
    fn reprice(&mut self, positions: &[usize], total: i64) {
        match positions {
            [] => {}
            [only] => self.state.snapshot.items[*only].cost = TokenCost::Exact(total),
            _ => {
                let weights: Vec<i64> = positions
                    .iter()
                    .map(|&position| self.state.snapshot.items[position].cost.tokens())
                    .collect();
                for (&position, share) in positions.iter().zip(apportion(total, &weights)) {
                    self.state.snapshot.items[position].cost = TokenCost::Estimated(share);
                }
            }
        }
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
            TurnOutcome::Incomplete => {
                self.last_anchor_total = None;
                None
            }
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
        by_category.sort_by_key(|(category, _)| *category);
        self.state.snapshot.groups = groups;
        self.state.snapshot.by_category = by_category;
    }
}

/// Splits `total` across `weights` so the shares sum to exactly `total`.
///
/// Each share is the running floor of the cumulative weight fraction minus what is already handed
/// out, so the last cumulative floor is `total` itself and the rounding remainder lands on later
/// entries rather than being lost. Weightless items split evenly, remainder to the earliest.
fn apportion(total: i64, weights: &[i64]) -> Vec<i64> {
    if weights.is_empty() {
        return Vec::new();
    }
    if total <= 0 {
        return vec![0; weights.len()];
    }
    let count = weights.len() as i128;
    let total = i128::from(total);
    let total_weight: i128 = weights.iter().copied().map(i128::from).sum();
    if total_weight == 0 {
        let base = total / count;
        let remainder = total % count;
        return (0..count)
            .map(|index| (base + i128::from(index < remainder)) as i64)
            .collect();
    }
    let mut shares = Vec::with_capacity(weights.len());
    let mut cumulative_weight: i128 = 0;
    let mut assigned: i128 = 0;
    for &weight in weights {
        cumulative_weight += i128::from(weight);
        let cumulative = total * cumulative_weight / total_weight;
        shares.push((cumulative - assigned) as i64);
        assigned = cumulative;
    }
    shares
}

#[cfg(test)]
#[path = "profiler_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "fixture_tests.rs"]
mod fixture_tests;
