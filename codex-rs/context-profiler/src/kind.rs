//! The variant of a `ResponseItem`, named once for labels, traces, and call pairing.

use codex_protocol::models::ResponseItem;

/// Exhaustive so a new upstream `ResponseItem` variant fails the build.
pub fn item_kind(item: &ResponseItem) -> &'static str {
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

/// The name a tool call is known by, falling back to the variant name for everything else.
pub(crate) fn item_label(item: &ResponseItem) -> &str {
    match item {
        ResponseItem::FunctionCall { name, .. } | ResponseItem::CustomToolCall { name, .. } => name,
        ResponseItem::LocalShellCall { .. } => "shell",
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::Message { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => item_kind(item),
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
