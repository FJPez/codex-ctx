//! Classifies an item into a display `Category` and an independent `PricingKind`.
//!
//! Display classification reads the harness-owned `content_item_kinds` that core stamps on
//! contextual messages, one kind per entry of the message's `content` array. Kind strings churn
//! across upstream versions, so nothing here matches a list of known kinds: `user.*` and
//! `compaction.summary` are the only families with special display meaning, and every other
//! non-empty kind on a user-role message is an injected instruction fragment.

use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
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

use crate::estimate::serialized_size;
use crate::item::Category;
use crate::item::ClassificationWarning;
use crate::item::ContentPart;
use crate::item::PartMedia;
use crate::item::PricingKind;

/// The kind core stamps when it has no classification for an entry.
const UNKNOWN_KIND: &str = "unknown";

/// Kinds whose display category is not the default instruction one.
const USER_KIND_PREFIX: &str = "user.";
const COMPACTION_SUMMARY_KIND: &str = "compaction.summary";
/// Core substitutes these for user input it could not deliver, so they stay the user's.
const USER_REPLACEMENT_KINDS: [&str; 3] = [
    "images.preparation_error",
    "images.unsupported",
    "audio.unsupported",
];

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
    /// Each reason at most once, so the count of warned items is `!warnings.is_empty()`.
    pub warnings: Vec<ClassificationWarning>,
}

impl Classification {
    pub(crate) fn warned(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Records a reason once however many entries raised it.
fn note(warnings: &mut Vec<ClassificationWarning>, warning: ClassificationWarning) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

/// The message roles this crate distinguishes; the wire type is an open string, normalized
/// centrally so every match on it is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Developer,
    Assistant,
    System,
    Unknown,
}

impl Role {
    fn parse(role: &str) -> Self {
        match role {
            "user" => Self::User,
            "developer" => Self::Developer,
            "assistant" => Self::Assistant,
            "system" => Self::System,
            _ => Self::Unknown,
        }
    }
}

pub(crate) fn classify(item: &ResponseItem) -> Classification {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let role = Role::parse(role);
            let mut warnings = Vec::new();
            let fixed = role_category(role, &mut warnings);
            let parts = message_parts(item, fixed, content, &mut warnings);
            let category = fixed.unwrap_or_else(|| merge_categories(&parts, &mut warnings));
            Classification {
                category,
                pricing: pricing_for_role(role),
                parts,
                warnings,
            }
        }
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => Classification {
            category: Category::ToolOutput,
            pricing: pricing_kind(item),
            parts: output_parts(&output.body),
            warnings: Vec::new(),
        },
        ResponseItem::ConfigurationUpdate { .. } => Classification {
            category: Category::Other,
            pricing: pricing_kind(item),
            parts: vec![ContentPart {
                kind: "configuration_update".to_string(),
                bytes: serialized_size(item).unwrap_or(0),
                category: Category::Other,
                media: PartMedia::Text,
            }],
            warnings: Vec::new(),
        },
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::ContextCompaction { .. } => Classification {
            category: structural_category(item),
            pricing: pricing_kind(item),
            parts: Vec::new(),
            warnings: Vec::new(),
        },
        // The serde fallback: an item type this build does not know, so its arrival is the signal.
        ResponseItem::Other => Classification {
            category: Category::Other,
            pricing: pricing_kind(item),
            parts: Vec::new(),
            warnings: vec![ClassificationWarning::UnknownItemType],
        },
    }
}

/// Structured tool outputs can carry images and audio, which must not be priced as prose.
fn output_parts(body: &FunctionCallOutputBody) -> Vec<ContentPart> {
    let FunctionCallOutputBody::ContentItems(entries) = body else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| ContentPart {
            kind: String::new(),
            bytes: serialized_size(entry).unwrap_or(0),
            category: Category::ToolOutput,
            media: match entry {
                FunctionCallOutputContentItem::InputText { .. }
                | FunctionCallOutputContentItem::EncryptedContent { .. } => PartMedia::Text,
                FunctionCallOutputContentItem::InputImage { .. } => PartMedia::Image,
                FunctionCallOutputContentItem::InputAudio { .. } => PartMedia::Audio,
            },
        })
        .collect()
}

/// Which measured total may price an item; exhaustive so a new upstream variant fails the build.
pub(crate) fn pricing_kind(item: &ResponseItem) -> PricingKind {
    match item {
        ResponseItem::Message { role, .. } => pricing_for_role(Role::parse(role)),
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

fn pricing_for_role(role: Role) -> PricingKind {
    match role {
        Role::User | Role::Developer => PricingKind::Input,
        Role::Assistant => PricingKind::Output,
        // Core drops raw system messages after the raw-stream clone; unknown roles could go
        // either way.
        Role::System | Role::Unknown => PricingKind::Ambiguous,
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
    fixed: Option<Category>,
    content: &[ContentItem],
    warnings: &mut Vec<ClassificationWarning>,
) -> Vec<ContentPart> {
    let kinds = content_item_kinds(item);
    if fixed.is_none() && !kinds.is_empty() && kinds.len() != content.len() {
        note(warnings, ClassificationWarning::KindLengthMismatch);
    }
    content
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let kind = kinds.get(index).map(|kind| kind.0.as_str());
            ContentPart {
                kind: kind.unwrap_or_default().to_string(),
                bytes: serialized_size(entry).unwrap_or(0),
                category: fixed.unwrap_or_else(|| entry_category(kind, entry, warnings)),
                media: part_media(entry),
            }
        })
        .collect()
}

fn part_media(entry: &ContentItem) -> PartMedia {
    match entry {
        ContentItem::InputText { .. } | ContentItem::OutputText { .. } => PartMedia::Text,
        ContentItem::InputImage { .. } => PartMedia::Image,
        ContentItem::InputAudio { .. } => PartMedia::Audio,
    }
}

/// Roles whose category is independent of their entries, so an empty message is still classified
/// and an unknown role is warned about regardless of content.
fn role_category(role: Role, warnings: &mut Vec<ClassificationWarning>) -> Option<Category> {
    match role {
        Role::Assistant => Some(Category::AgentMessage),
        Role::System => Some(Category::Instructions),
        Role::User | Role::Developer => None,
        Role::Unknown => {
            note(warnings, ClassificationWarning::UnknownRole);
            Some(Category::Other)
        }
    }
}

fn content_item_kinds(item: &ResponseItem) -> &[ContentItemKind] {
    match item {
        ResponseItem::Message {
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } => metadata.content_item_kinds.as_deref().unwrap_or_default(),
        _ => &[],
    }
}

/// User- and developer-role entries only; every other role is decided by `role_category`.
fn entry_category(
    kind: Option<&str>,
    entry: &ContentItem,
    warnings: &mut Vec<ClassificationWarning>,
) -> Category {
    match kind {
        Some(kind)
            if kind.starts_with(USER_KIND_PREFIX) || USER_REPLACEMENT_KINDS.contains(&kind) =>
        {
            Category::UserMessage
        }
        Some(COMPACTION_SUMMARY_KIND) => Category::Compaction,
        Some(kind) if !kind.is_empty() && kind != UNKNOWN_KIND => Category::Instructions,
        _ => {
            note(warnings, ClassificationWarning::MarkerFallback);
            tagged_fallback(entry)
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
fn merge_categories(parts: &[ContentPart], warnings: &mut Vec<ClassificationWarning>) -> Category {
    let Some(first) = parts.first() else {
        return Category::Other;
    };
    if parts.iter().all(|part| part.category == first.category) {
        return first.category;
    }
    note(warnings, ClassificationWarning::MixedCategories);
    Category::Other
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
