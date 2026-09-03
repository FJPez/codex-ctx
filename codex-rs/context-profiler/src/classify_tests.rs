use super::*;
use codex_protocol::models::ConfigurationReasoning;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

fn text(text: &str) -> ContentItem {
    ContentItem::InputText {
        text: text.to_string(),
    }
}

/// A message with one content entry per kind, or no metadata at all when `kinds` is `None`.
fn message(role: &str, content: Vec<ContentItem>, kinds: Option<&[&str]>) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: kinds.map(|kinds| {
            InternalChatMessageMetadataPassthrough {
                content_item_kinds: Some(
                    kinds
                        .iter()
                        .map(|kind| ContentItemKind((*kind).to_string()))
                        .collect(),
                ),
                ..Default::default()
            }
        }),
    }
}

fn user(kind: &str, body: &str) -> ResponseItem {
    message("user", vec![text(body)], Some(&[kind]))
}

fn tool_output() -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "call_1".to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("ok".to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn every_variant() -> Vec<(ResponseItem, PricingKind)> {
    vec![
        (
            message("user", vec![text("hi")], Some(&["user.text"])),
            PricingKind::Input,
        ),
        (
            message("developer", vec![text("hi")], Some(&["user.text"])),
            PricingKind::Input,
        ),
        (
            message("assistant", vec![text("hi")], Some(&["unknown"])),
            PricingKind::Output,
        ),
        (
            message("system", vec![text("hi")], None),
            PricingKind::Ambiguous,
        ),
        (
            message("tool", vec![text("hi")], None),
            PricingKind::Ambiguous,
        ),
        (
            ResponseItem::FunctionCallOutput {
                id: None,
                call_id: Some("call_1".to_string()),
                name: None,
                namespace: None,
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("ok".to_string()),
                    success: Some(true),
                },
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Input,
        ),
        (tool_output(), PricingKind::Input),
        (
            ResponseItem::ToolSearchOutput {
                id: None,
                call_id: None,
                status: "completed".to_string(),
                execution: "local".to_string(),
                tools: Vec::new(),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Input,
        ),
        (
            ResponseItem::AgentMessage {
                id: None,
                author: "codex".to_string(),
                recipient: "user".to_string(),
                content: Vec::new(),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some("opaque".to_string()),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".to_string(),
                namespace: None,
                arguments: "{}".to_string(),
                encrypted_function_args: None,
                call_id: "call_1".to_string(),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: "call_1".to_string(),
                name: "shell".to_string(),
                namespace: None,
                input: "ls".to_string(),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("call_1".to_string()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["ls".to_string()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::ToolSearchCall {
                id: None,
                call_id: None,
                status: None,
                execution: "local".to_string(),
                arguments: serde_json::Value::Null,
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::WebSearchCall {
                id: None,
                status: None,
                action: None,
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (
            ResponseItem::ImageGenerationCall {
                id: None,
                status: "completed".to_string(),
                revised_prompt: None,
                result: String::new(),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Output,
        ),
        (ResponseItem::CompactionTrigger {}, PricingKind::Ambiguous),
        (
            ResponseItem::AdditionalTools {
                id: None,
                role: "system".to_string(),
                tools: Vec::new(),
            },
            PricingKind::Ambiguous,
        ),
        (
            ResponseItem::Compaction {
                id: None,
                encrypted_content: "opaque".to_string(),
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Ambiguous,
        ),
        (
            ResponseItem::ContextCompaction {
                id: None,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
            },
            PricingKind::Ambiguous,
        ),
        (ResponseItem::Other, PricingKind::Ambiguous),
        (configuration_update(), PricingKind::Ambiguous),
    ]
}

fn configuration_update() -> ResponseItem {
    ResponseItem::ConfigurationUpdate {
        reasoning: ConfigurationReasoning {
            effort: ReasoningEffort::Medium,
        },
    }
}

#[test]
fn every_variant_and_role_has_a_pricing_kind() {
    let expected: Vec<PricingKind> = every_variant()
        .iter()
        .map(|(_, pricing)| *pricing)
        .collect();
    let actual: Vec<PricingKind> = every_variant()
        .iter()
        .map(|(item, _)| pricing_kind(item))
        .collect();
    assert_eq!(expected, actual);
}

#[test]
fn the_kind_table_decides_the_category() {
    let rows = [
        (user("user.text", "hello"), Category::UserMessage, false),
        (
            message(
                "user",
                vec![ContentItem::InputImage {
                    image_url: "data:,".to_string(),
                    detail: None,
                }],
                Some(&["user.image"]),
            ),
            Category::UserMessage,
            false,
        ),
        (
            user("compaction.summary", "summary"),
            Category::Compaction,
            false,
        ),
        (
            user("compaction.auto_fallback_prompt", "keep going"),
            Category::Instructions,
            false,
        ),
        (
            user("never_seen_upstream.family", "novel"),
            Category::Instructions,
            false,
        ),
        (
            message("assistant", vec![text("done")], Some(&["unknown"])),
            Category::AgentMessage,
            false,
        ),
        (
            message("system", vec![text("be brief")], None),
            Category::Instructions,
            false,
        ),
        (
            message("tool", vec![text("mystery")], None),
            Category::Other,
            true,
        ),
    ];

    let expected: Vec<(Category, bool)> = rows
        .iter()
        .map(|(_, category, warned)| (*category, *warned))
        .collect();
    let actual: Vec<(Category, bool)> = rows
        .iter()
        .map(|(item, _, _)| {
            let classification = classify(item);
            (classification.category, classification.warned)
        })
        .collect();
    assert_eq!(expected, actual);
}

#[test]
fn kinds_are_looked_up_by_index_never_zipped() {
    let short = message(
        "user",
        vec![text("<user_instructions>do this"), text("and this")],
        Some(&["agents_md.instructions"]),
    );
    let classification = classify(&short);
    assert_eq!(
        vec![Category::Instructions, Category::UserMessage],
        classification
            .parts
            .iter()
            .map(|part| part.category)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        vec!["agents_md.instructions".to_string(), String::new()],
        classification
            .parts
            .iter()
            .map(|part| part.kind.clone())
            .collect::<Vec<_>>()
    );
    assert!(classification.warned);

    let long = message(
        "user",
        vec![text("hello")],
        Some(&["user.text", "agents_md.instructions"]),
    );
    let classification = classify(&long);
    assert_eq!(Category::UserMessage, classification.category);
    assert_eq!(1, classification.parts.len());
    assert!(classification.warned);
}

#[test]
fn an_untagged_message_falls_back_to_the_open_tag_marker() {
    let tagged = message(
        "user",
        vec![text("\n<user_instructions>\nbe careful\n")],
        None,
    );
    let plain = message("user", vec![text("just a question")], None);

    let tagged = classify(&tagged);
    let plain = classify(&plain);
    assert_eq!(
        (Category::Instructions, true),
        (tagged.category, tagged.warned)
    );
    assert_eq!(
        (Category::UserMessage, true),
        (plain.category, plain.warned)
    );
}

#[test]
fn a_merged_fragment_message_keeps_one_category_but_a_mixed_one_does_not() {
    let entries = vec![text("AGENTS.md says"), text("<environment_context>cwd")];
    let merged = message(
        "user",
        entries.clone(),
        Some(&["agents_md.instructions", "environments.environment_context"]),
    );
    let classification = classify(&merged);
    assert_eq!(Category::Instructions, classification.category);
    assert!(!classification.warned);
    assert_eq!(
        entries
            .iter()
            .map(|entry| serde_json::to_vec(entry).expect("serializable entry").len())
            .collect::<Vec<_>>(),
        classification
            .parts
            .iter()
            .map(|part| part.bytes)
            .collect::<Vec<_>>()
    );

    let mixed = message(
        "user",
        vec![text("hello"), text("AGENTS.md says")],
        Some(&["user.text", "agents_md.instructions"]),
    );
    let classification = classify(&mixed);
    assert_eq!(Category::Other, classification.category);
    assert!(classification.warned);
}

#[test]
fn a_configuration_update_is_one_ambiguous_part() {
    let item = configuration_update();
    let classification = classify(&item);

    assert_eq!(Category::Other, classification.category);
    assert_eq!(PricingKind::Ambiguous, classification.pricing);
    assert_eq!(
        vec![ContentPart {
            kind: "configuration_update".to_string(),
            bytes: serde_json::to_vec(&item).expect("serializable item").len(),
            category: Category::Other,
            is_image: false,
        }],
        classification.parts
    );
}
