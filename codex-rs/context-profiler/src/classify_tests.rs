use super::*;
use codex_protocol::models::ConfigurationReasoning;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
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

fn configuration_update() -> ResponseItem {
    ResponseItem::ConfigurationUpdate {
        reasoning: ConfigurationReasoning {
            effort: ReasoningEffort::Medium,
        },
    }
}

#[test]
fn content_kind_families_decide_message_categories() {
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
            (classification.category, classification.warned())
        })
        .collect();
    assert_eq!(expected, actual);
}

#[test]
fn mismatched_kind_arrays_do_not_shift_entry_classification() {
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
    assert_eq!(
        vec![
            ClassificationWarning::KindLengthMismatch,
            ClassificationWarning::MarkerFallback,
            ClassificationWarning::MixedCategories,
        ],
        classification.warnings
    );

    let long = message(
        "user",
        vec![text("hello")],
        Some(&["user.text", "agents_md.instructions"]),
    );
    let classification = classify(&long);
    assert_eq!(Category::UserMessage, classification.category);
    assert_eq!(1, classification.parts.len());
    assert_eq!(
        vec![ClassificationWarning::KindLengthMismatch],
        classification.warnings
    );
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
        (tagged.category, tagged.warned())
    );
    assert_eq!(
        (Category::UserMessage, true),
        (plain.category, plain.warned())
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
    assert!(!classification.warned());
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
    assert_eq!(
        vec![ClassificationWarning::MixedCategories],
        classification.warnings
    );
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
            media: PartMedia::Text,
        }],
        classification.parts
    );
}

/// An item type this build cannot name is worth a warning; known control items are not.
#[test]
fn an_unknown_item_type_warns_but_known_controls_do_not() {
    let unknown = classify(&ResponseItem::Other);
    assert_eq!(Category::Other, unknown.category);
    assert_eq!(PricingKind::Ambiguous, unknown.pricing);
    assert_eq!(
        vec![ClassificationWarning::UnknownItemType],
        unknown.warnings
    );

    assert!(!classify(&configuration_update()).warned());
    assert!(!classify(&ResponseItem::CompactionTrigger {}).warned());
}

#[test]
fn audio_entries_are_marked_as_audio() {
    let item = message(
        "user",
        vec![ContentItem::InputAudio {
            audio_url: "data:audio/wav;base64,AAAA".to_string(),
        }],
        Some(&["user.audio"]),
    );
    let classification = classify(&item);
    assert_eq!(PartMedia::Audio, classification.parts[0].media);
    assert_eq!(Category::UserMessage, classification.category);
}

/// A role's category is independent of the message content, so an empty message is still
/// classified and an unknown role still warns - once, however many entries it has.
#[test]
fn empty_messages_classify_by_role() {
    let cases = [
        ("assistant", Category::AgentMessage, vec![]),
        ("system", Category::Instructions, vec![]),
        ("user", Category::Other, vec![]),
        (
            "future_role",
            Category::Other,
            vec![ClassificationWarning::UnknownRole],
        ),
    ];
    for (role, category, warnings) in cases {
        let classification = classify(&message(role, Vec::new(), None));
        assert_eq!(category, classification.category, "{role}");
        assert_eq!(warnings, classification.warnings, "{role}");
        assert!(classification.parts.is_empty(), "{role}");
    }

    let two_entries = classify(&message("future_role", vec![text("a"), text("b")], None));
    assert_eq!(
        vec![ClassificationWarning::UnknownRole],
        two_entries.warnings
    );
}
