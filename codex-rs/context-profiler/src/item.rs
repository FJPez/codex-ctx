//! History items, their classification, and the groups they are displayed in.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Category {
    UserMessage,
    AgentMessage,
    Reasoning,
    ToolCall,
    ToolOutput,
    Instructions,
    Compaction,
    Other,
}

impl Category {
    /// Declaration order, so category aggregates are deterministic.
    pub(crate) fn ordinal(self) -> u8 {
        match self {
            Self::UserMessage => 0,
            Self::AgentMessage => 1,
            Self::Reasoning => 2,
            Self::ToolCall => 3,
            Self::ToolOutput => 4,
            Self::Instructions => 5,
            Self::Compaction => 6,
            Self::Other => 7,
        }
    }
}

/// Which measured total may price an item. Never derived from `Category`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PricingKind {
    /// Serialised into the next request, so an anchor delta prices it.
    Input,
    /// One response's own output, so that response's `output_tokens` prices it.
    Output,
    /// If an attribution span contains an `Ambiguous` item, leave the entire span on its initial
    /// estimates. Still record the usage anchor and advance the span boundary.
    Ambiguous,
}

/// One entry of a message's content array, classified on its own.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentPart {
    /// The harness-owned `ContentItemKind`, or empty when the entry carried none.
    pub kind: String,
    pub bytes: usize,
    pub category: Category,
}

/// A whole measured total on one item is `Exact`; a proportional share of a measured total is
/// `Estimated`, as is a byte proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TokenCost {
    Exact(i64),
    Estimated(i64),
}

impl TokenCost {
    pub fn tokens(self) -> i64 {
        match self {
            Self::Exact(tokens) | Self::Estimated(tokens) => tokens,
        }
    }

    /// A total is exact only when every part of it is.
    pub(crate) fn combine(self, other: Self) -> Self {
        let tokens = self.tokens() + other.tokens();
        match (self, other) {
            (Self::Exact(_), Self::Exact(_)) => Self::Exact(tokens),
            _ => Self::Estimated(tokens),
        }
    }
}

/// Every item belongs to exactly one group.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum GroupKey {
    /// Keyed on `call_id` alone; core pairs calls with outputs globally, not per turn.
    ToolCall(String),
    Ungrouped(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemSummary {
    pub seq: u64,
    pub turn_index: u32,
    pub category: Category,
    pub pricing: PricingKind,
    pub bytes: usize,
    pub cost: TokenCost,
    pub label: String,
    pub group: GroupKey,
    pub item_id: Option<String>,
    /// Per-content-entry breakdown, in bytes; empty for items without a content array.
    pub parts: Vec<ContentPart>,
}

/// The display unit for "largest contributors": typically a tool call with its output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemGroup {
    pub key: GroupKey,
    pub category: Category,
    /// `Exact` only when every member is.
    pub cost: TokenCost,
    pub label: String,
    /// `ItemSummary::seq` values of the members.
    pub members: Vec<u64>,
}
