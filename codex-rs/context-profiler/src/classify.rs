//! Classifies an item into a display `Category` and an independent `PricingKind`.
//!
//! Display classification reads the harness-owned `content_item_kinds` that core stamps on
//! contextual messages, one kind per entry of the message's `content` array. Kind strings churn
//! across upstream versions, so nothing here matches a list of known kinds: `user.*` and
//! `compaction.summary` are the only families with special display meaning, and every other
//! non-empty kind on a user-role message is an injected instruction fragment.

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::APPS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::COLLABORATION_MODE_OPEN_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_GUIDANCE_OPEN_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_OPEN_TAG;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use codex_protocol::protocol::ENVIRONMENTS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::PLUGINS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::REALTIME_CONVERSATION_OPEN_TAG;
use codex_protocol::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::TOOLS_OPEN_TAG;
use codex_protocol::protocol::USER_INSTRUCTIONS_OPEN_TAG;

use crate::item::Category;
use crate::item::ContentPart;
use crate::item::PricingKind;

/// The kind core stamps when it has no classification for an entry.
const UNKNOWN_KIND: &str = "unknown";

/// Kinds whose display category is not the default instruction one.
const USER_KIND_PREFIX: &str = "user.";
const COMPACTION_SUMMARY_KIND: &str = "compaction.summary";

/// Fallback only, for messages that reach us without `content_item_kinds` at all.
const INSTRUCTION_OPEN_TAGS: [&str; 12] = [
    USER_INSTRUCTIONS_OPEN_TAG,
    ENVIRONMENT_CONTEXT_OPEN_TAG,
    ENVIRONMENTS_INSTRUCTIONS_OPEN_TAG,
    APPS_INSTRUCTIONS_OPEN_TAG,
    SKILLS_INSTRUCTIONS_OPEN_TAG,
    PLUGINS_INSTRUCTIONS_OPEN_TAG,
    TOOLS_OPEN_TAG,
    COLLABORATION_MODE_OPEN_TAG,
    MULTI_AGENT_MODE_OPEN_TAG,
    REALTIME_CONVERSATION_OPEN_TAG,
    CONTEXT_WINDOW_OPEN_TAG,
    CONTEXT_WINDOW_GUIDANCE_OPEN_TAG,
];

pub(crate) struct Classification {
    pub category: Category,
    pub pricing: PricingKind,
    pub parts: Vec<ContentPart>,
    /// One flag per item however many uncertainties it held, so warnings count items.
    pub warned: bool,
}

pub(crate) fn classify(item: &ResponseItem) -> Classification {
    let pricing = pricing_kind(item);
    match item {
        ResponseItem::Message { role, content, .. } => {
            let mut warned = false;
            let parts = message_parts(item, role, content, &mut warned);
            let category = merge_categories(&parts, &mut warned);
            Classification {
                category,
                pricing,
                parts,
                warned,
            }
        }
        ResponseItem::ConfigurationUpdate { .. } => Classification {
            category: Category::Other,
            pricing,
            parts: vec![ContentPart {
                kind: "configuration_update".to_string(),
                bytes: serde_json::to_vec(item).map(|json| json.len()).unwrap_or(0),
                category: Category::Other,
            }],
            warned: false,
        },
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => Classification {
            category: structural_category(item),
            pricing,
            parts: Vec::new(),
            warned: false,
        },
    }
}

/// Which measured total may price an item; exhaustive so a new upstream variant fails the build.
pub(crate) fn pricing_kind(item: &ResponseItem) -> PricingKind {
    match item {
        ResponseItem::Message { role, .. } => match role.as_str() {
            "user" | "developer" => PricingKind::Input,
            "assistant" => PricingKind::Output,
            // `system`, and any role we have never seen.
            _ => PricingKind::Ambiguous,
        },
        ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. } => PricingKind::Input,
        ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. } => PricingKind::Output,
        ResponseItem::CompactionTrigger { .. }
        | ResponseItem::AdditionalTools { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other
        | ResponseItem::ConfigurationUpdate { .. } => PricingKind::Ambiguous,
    }
}

/// Non-message items have no content array, so their category is the variant itself.
fn structural_category(item: &ResponseItem) -> Category {
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
        ResponseItem::AgentMessage { .. } => Category::AgentMessage,
        ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. } => Category::Compaction,
        ResponseItem::Message { .. }
        | ResponseItem::AdditionalTools { .. }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::Other => Category::Other,
    }
}

/// One part per content entry, with its kind looked up by index rather than zipped, so a short or
/// long kind array cannot silently shift every later entry's classification.
fn message_parts(
    item: &ResponseItem,
    role: &str,
    content: &[ContentItem],
    warned: &mut bool,
) -> Vec<ContentPart> {
    let kinds = content_item_kinds(item);
    if role_consults_kinds(role) && kinds.len() != content.len() {
        *warned = true;
    }
    content
        .iter()
        .enumerate()
        .map(|(index, entry)| ContentPart {
            kind: kinds.get(index).cloned().unwrap_or_default(),
            bytes: serde_json::to_vec(entry)
                .map(|json| json.len())
                .unwrap_or(0),
            category: entry_category(role, kinds.get(index).map(String::as_str), entry, warned),
        })
        .collect()
}

/// Only user-role messages carry meaningful kinds; core stamps `unknown` on everything else.
fn role_consults_kinds(role: &str) -> bool {
    matches!(role, "user" | "developer")
}

fn content_item_kinds(item: &ResponseItem) -> Vec<String> {
    let ResponseItem::Message {
        internal_chat_message_metadata_passthrough: Some(metadata),
        ..
    } = item
    else {
        return Vec::new();
    };
    metadata
        .content_item_kinds
        .iter()
        .flatten()
        .map(|kind| kind.0.clone())
        .collect()
}

fn entry_category(
    role: &str,
    kind: Option<&str>,
    entry: &ContentItem,
    warned: &mut bool,
) -> Category {
    match role {
        "assistant" => Category::AgentMessage,
        "system" => Category::Instructions,
        "user" | "developer" => match kind {
            Some(kind) if kind.starts_with(USER_KIND_PREFIX) => Category::UserMessage,
            Some(COMPACTION_SUMMARY_KIND) => Category::Compaction,
            Some(kind) if !kind.is_empty() && kind != UNKNOWN_KIND => Category::Instructions,
            _ => {
                *warned = true;
                tagged_fallback(entry)
            }
        },
        _ => {
            *warned = true;
            Category::Other
        }
    }
}

/// Pre-`content_item_kinds` shape: instruction fragments announce themselves with an open tag.
fn tagged_fallback(entry: &ContentItem) -> Category {
    let text = match entry {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => text.trim_start(),
        ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => {
            return Category::UserMessage;
        }
    };
    if INSTRUCTION_OPEN_TAGS
        .iter()
        .any(|tag| text.starts_with(tag))
    {
        Category::Instructions
    } else {
        Category::UserMessage
    }
}

/// A merged message keeps its category only while its entries agree; a genuinely mixed one is not
/// representable in a single row, so it lands in `Other` and says so.
fn merge_categories(parts: &[ContentPart], warned: &mut bool) -> Category {
    let Some(first) = parts.first() else {
        return Category::Other;
    };
    if parts.iter().all(|part| part.category == first.category) {
        return first.category;
    }
    *warned = true;
    Category::Other
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
