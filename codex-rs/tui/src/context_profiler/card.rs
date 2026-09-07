//! The plain-data model behind the `/ctx` card.
//!
//! Everything the view needs is decided here once, so rendering only depends on width.

use codex_context_profiler::Category;
use codex_context_profiler::InvalidationReason;
use codex_context_profiler::ProfilerState;
use codex_context_profiler::TokenCost;

use crate::token_usage::percent_of_window_remaining;

/// How many turns the card lists.
const MAX_TURN_ROWS: usize = 5;
/// How many item groups the card lists.
const MAX_CONTRIBUTORS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) reported: i64,
    pub(crate) window: Option<i64>,
    pub(crate) percent_remaining: Option<i64>,
}

/// Whether the reported total already accounts for every observed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Usage {
    Current,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Startup {
    Available {
        total: i64,
        instructions: i64,
        baseline: i64,
        first_request_input: i64,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) name: &'static str,
    pub(crate) tokens: TokenCost,
    pub(crate) share_percent: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Contributor {
    pub(crate) rank: u8,
    pub(crate) category: &'static str,
    pub(crate) label: String,
    pub(crate) tokens: TokenCost,
    pub(crate) share_percent: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnRow {
    pub(crate) index: u32,
    pub(crate) added: Option<i64>,
    pub(crate) context_after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextCard {
    pub(crate) invalidated: Option<InvalidationReason>,
    pub(crate) header: Option<Header>,
    pub(crate) usage: Usage,
    pub(crate) startup: Startup,
    pub(crate) categories: Vec<Row>,
    pub(crate) attributed_total: TokenCost,
    pub(crate) not_attributable: Vec<Row>,
    pub(crate) footer: Option<i64>,
    pub(crate) contributors: Vec<Contributor>,
    pub(crate) turns: Vec<TurnRow>,
    pub(crate) warnings: u32,
    pub(crate) unsizable: u32,
    pub(crate) has_items: bool,
}

pub(crate) fn build(state: &ProfilerState) -> ContextCard {
    let snapshot = &state.snapshot;
    let usage = if state.usage_pending {
        Usage::Pending
    } else {
        Usage::Current
    };
    let header = snapshot.reported_context_tokens.map(|reported| Header {
        reported,
        window: snapshot.window,
        percent_remaining: snapshot
            .window
            .map(|window| percent_of_window_remaining(reported, window)),
    });
    // Shares are only honest against a total that covers every observed item.
    let denominator = match (header.as_ref(), usage) {
        (Some(header), Usage::Current) if header.reported > 0 => Some(header.reported),
        _ => None,
    };

    let mut categories: Vec<Row> = snapshot
        .by_category
        .iter()
        .filter(|(_, cost)| cost.tokens() != 0)
        .map(|(category, cost)| Row {
            name: category_name(*category),
            tokens: *cost,
            share_percent: share(cost.tokens(), denominator),
        })
        .collect();
    categories.sort_by(|left, right| {
        right
            .tokens
            .tokens()
            .cmp(&left.tokens.tokens())
            .then_with(|| left.name.cmp(right.name))
    });

    let attributed_total = snapshot
        .by_category
        .iter()
        .map(|(_, cost)| *cost)
        .reduce(combine)
        .unwrap_or(TokenCost::Estimated(0));

    let not_attributable = match denominator {
        None => Vec::new(),
        Some(reported) => match snapshot.baseline_tokens {
            // The baseline is a reconciled residual, never a measured item cost.
            Some(baseline) => vec![
                Row {
                    name: "System + tools baseline",
                    tokens: TokenCost::Estimated(baseline),
                    share_percent: share(baseline, denominator),
                },
                Row {
                    name: "Reconciliation drift",
                    tokens: TokenCost::Estimated(snapshot.drift_tokens),
                    share_percent: share(snapshot.drift_tokens, denominator),
                },
            ],
            None => {
                let remainder = reported - snapshot.attributed_tokens();
                vec![Row {
                    name: "Not attributed",
                    tokens: TokenCost::Estimated(remainder),
                    share_percent: share(remainder, denominator),
                }]
            }
        },
    };
    let footer = denominator.and(header.as_ref().map(|header| header.reported));

    let mut groups: Vec<_> = snapshot.groups.iter().collect();
    groups.sort_by(|left, right| {
        right
            .cost
            .tokens()
            .cmp(&left.cost.tokens())
            .then_with(|| left.members.first().cmp(&right.members.first()))
    });
    let contributors = groups
        .into_iter()
        .take(MAX_CONTRIBUTORS)
        .enumerate()
        .map(|(position, group)| Contributor {
            rank: position as u8 + 1,
            category: category_name(group.category),
            label: group.label.clone(),
            tokens: group.cost,
            share_percent: share(group.cost.tokens(), denominator),
        })
        .collect();

    let turns = snapshot
        .turns
        .iter()
        .rev()
        .take(MAX_TURN_ROWS)
        .map(|turn| TurnRow {
            index: turn.index,
            added: turn.measured_added(),
            context_after: turn.measured_after,
        })
        .collect();

    let startup = match snapshot.initial_context.as_ref() {
        Some(initial) => Startup::Available {
            total: initial.startup_context_tokens(),
            instructions: initial.estimated_instruction_tokens,
            baseline: initial.hidden_tokens(),
            first_request_input: initial.first_request_input_tokens,
        },
        None => Startup::Unavailable,
    };

    ContextCard {
        invalidated: state.invalidated.clone(),
        header,
        usage,
        startup,
        categories,
        attributed_total,
        not_attributable,
        footer,
        contributors,
        turns,
        warnings: state.classification_warning_count,
        unsizable: state.unsizable_item_count,
        has_items: !snapshot.items.is_empty(),
    }
}

/// A total is exact only when every part of it is.
fn combine(left: TokenCost, right: TokenCost) -> TokenCost {
    let tokens = left.tokens() + right.tokens();
    match (left, right) {
        (TokenCost::Exact(_), TokenCost::Exact(_)) => TokenCost::Exact(tokens),
        _ => TokenCost::Estimated(tokens),
    }
}

/// A negative remainder has no honest share of the reported total, so it carries none.
fn share(tokens: i64, denominator: Option<i64>) -> Option<u32> {
    let reported = denominator?;
    if tokens < 0 {
        return None;
    }
    // A frozen baseline can outgrow a shrunken total, so shares above 100% are kept.
    let percent = (tokens as f64 * 100.0 / reported as f64).round();
    Some(percent as u32)
}

fn category_name(category: Category) -> &'static str {
    match category {
        Category::UserMessage => "User messages",
        Category::AgentMessage => "Agent messages",
        Category::Reasoning => "Reasoning",
        Category::ToolCall => "Tool calls",
        Category::ToolOutput => "Tool outputs",
        Category::Instructions => "Instructions",
        Category::Compaction => "Compaction",
        Category::Other => "Other",
    }
}
