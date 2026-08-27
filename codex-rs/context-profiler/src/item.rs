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
    pub estimated_tokens: i64,
    pub label: String,
    pub group: GroupKey,
    pub item_id: Option<String>,
}

/// The display unit for "largest contributors": typically a tool call with its output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemGroup {
    pub key: GroupKey,
    pub category: Category,
    pub estimated_tokens: i64,
    pub label: String,
    /// `ItemSummary::seq` values of the members.
    pub members: Vec<u64>,
}
