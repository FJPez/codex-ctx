//! History items, their classification, and the groups they are displayed in.

/// What kind of context an item contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Category {
    /// Deliberately not `UserPrompt`: we classify by failing to match known tags,
    /// which is negative inference, not proof of human provenance.
    UserMessage,
    AgentMessage,
    Reasoning,
    /// `FunctionCall` | `LocalShellCall` | `CustomToolCall` | `ToolSearchCall`
    /// | `WebSearchCall` | `ImageGenerationCall`.
    ToolCall,
    /// `FunctionCallOutput` | `CustomToolCallOutput` | `ToolSearchOutput`.
    ToolOutput,
    /// Injected contextual fragments.
    ///
    /// A tagged but unrecognised user message lands here and increments
    /// `ProfilerState::unrecognized_fragment_count`, turning a silent accuracy bug
    /// into a visible signal.
    Instructions,
    /// `ResponseItem::Compaction` | `ContextCompaction`.
    Compaction,
    Other,
}

/// What an item is grouped under for display.
///
/// Grouping is total - every item belongs to exactly one group - so the view never
/// handles an `Option` and cannot render a half-pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum GroupKey {
    /// A `call_id`, pairing a tool call with its output.
    ///
    /// Do NOT scope this by turn. Core pairs globally by `call_id`, and an
    /// interrupted turn whose output lands in the next turn would fail to pair
    /// under turn scoping - the exact orphan this grouping prevents. For collision
    /// safety add a diagnostics counter, not a composite key.
    ToolCall(String),
    /// The item's `seq`: the item is its own group.
    Ungrouped(u64),
}

/// One observed history item, classified and estimated.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemSummary {
    pub seq: u64,
    pub turn_index: u32,
    pub category: Category,
    pub estimated_tokens: i64,
    /// Human-readable label, capped in length.
    pub label: String,
    pub group: GroupKey,
    pub item_id: Option<String>,
}

/// One or more items that must be presented as a unit - typically a tool call
/// with its output. This is the display unit for "largest contributors",
/// never a bare `ItemSummary`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemGroup {
    pub key: GroupKey,
    /// The group's dominant category.
    pub category: Category,
    /// Sum across members.
    pub estimated_tokens: i64,
    pub label: String,
    /// `ItemSummary::seq` values of the members.
    pub members: Vec<u64>,
}
