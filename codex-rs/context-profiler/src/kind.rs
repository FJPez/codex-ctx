//! The variant of a `ResponseItem`, named once for labels, traces, and call pairing.

use codex_protocol::models::ResponseItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ItemKind {
    AdditionalTools,
    Message,
    AgentMessage,
    Reasoning,
    LocalShellCall,
    FunctionCall,
    ToolSearchCall,
    FunctionCallOutput,
    CustomToolCall,
    CustomToolCallOutput,
    ToolSearchOutput,
    WebSearchCall,
    ImageGenerationCall,
    Compaction,
    CompactionTrigger,
    ConfigurationUpdate,
    ContextCompaction,
    Other,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdditionalTools => "AdditionalTools",
            Self::Message => "Message",
            Self::AgentMessage => "AgentMessage",
            Self::Reasoning => "Reasoning",
            Self::LocalShellCall => "LocalShellCall",
            Self::FunctionCall => "FunctionCall",
            Self::ToolSearchCall => "ToolSearchCall",
            Self::FunctionCallOutput => "FunctionCallOutput",
            Self::CustomToolCall => "CustomToolCall",
            Self::CustomToolCallOutput => "CustomToolCallOutput",
            Self::ToolSearchOutput => "ToolSearchOutput",
            Self::WebSearchCall => "WebSearchCall",
            Self::ImageGenerationCall => "ImageGenerationCall",
            Self::Compaction => "Compaction",
            Self::CompactionTrigger => "CompactionTrigger",
            Self::ConfigurationUpdate => "ConfigurationUpdate",
            Self::ContextCompaction => "ContextCompaction",
            Self::Other => "Other",
        }
    }
}

/// Exhaustive so a new upstream `ResponseItem` variant fails the build.
pub fn item_kind(item: &ResponseItem) -> ItemKind {
    match item {
        ResponseItem::AdditionalTools { .. } => ItemKind::AdditionalTools,
        ResponseItem::Message { .. } => ItemKind::Message,
        ResponseItem::AgentMessage { .. } => ItemKind::AgentMessage,
        ResponseItem::Reasoning { .. } => ItemKind::Reasoning,
        ResponseItem::LocalShellCall { .. } => ItemKind::LocalShellCall,
        ResponseItem::FunctionCall { .. } => ItemKind::FunctionCall,
        ResponseItem::ToolSearchCall { .. } => ItemKind::ToolSearchCall,
        ResponseItem::FunctionCallOutput { .. } => ItemKind::FunctionCallOutput,
        ResponseItem::CustomToolCall { .. } => ItemKind::CustomToolCall,
        ResponseItem::CustomToolCallOutput { .. } => ItemKind::CustomToolCallOutput,
        ResponseItem::ToolSearchOutput { .. } => ItemKind::ToolSearchOutput,
        ResponseItem::WebSearchCall { .. } => ItemKind::WebSearchCall,
        ResponseItem::ImageGenerationCall { .. } => ItemKind::ImageGenerationCall,
        ResponseItem::Compaction { .. } => ItemKind::Compaction,
        ResponseItem::CompactionTrigger { .. } => ItemKind::CompactionTrigger,
        ResponseItem::ConfigurationUpdate { .. } => ItemKind::ConfigurationUpdate,
        ResponseItem::ContextCompaction { .. } => ItemKind::ContextCompaction,
        ResponseItem::Other => ItemKind::Other,
    }
}

/// The id that pairs a tool call with its output; core pairs them globally, not per turn.
pub fn call_id(item: &ResponseItem) -> Option<String> {
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
