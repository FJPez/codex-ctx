use super::*;
use crate::classify::classify;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

/// Deliberately loose: the estimator only has to keep an unmeasured item in the right order of
/// magnitude, since every item that matters is repriced from an anchor. Do not tighten this.
const TEXT_TOLERANCE: i64 = 2;
const REASONING_TOLERANCE: i64 = 10;

fn assert_within(factor: i64, measured: i64, estimated: i64) {
    assert!(
        estimated * factor >= measured && measured * factor >= estimated,
        "{estimated} is more than {factor}x from the measured {measured}"
    );
}

/// The measured calibration points from the module table.
#[test]
fn the_estimator_stays_within_a_factor_of_two_of_every_measured_point() {
    for (bytes, measured) in [
        (4_792_usize, 1_040_i64),
        (15_876, 3_373),
        (14_152, 2_219),
        (24_567, 5_043),
        (41_448, 8_942),
    ] {
        assert_within(TEXT_TOLERANCE, measured, text_tokens(bytes));
    }
    assert_within(REASONING_TOLERANCE, 14, reasoning_tokens(1_593));
}

#[test]
fn an_empty_item_costs_nothing_and_a_huge_one_does_not_overflow() {
    assert_eq!(0, text_tokens(0));
    assert_eq!(0, reasoning_tokens(0));
    assert_eq!(2_259_862, text_tokens(10 * 1_024 * 1_024));
    // Widened to i128 before the multiply, so even an impossible size neither wraps nor panics.
    assert!(text_tokens(usize::MAX) > 0);
}

/// An image is a data URL tens of kilobytes long that costs about a paragraph, so it is priced flat
/// and only the sibling text entry is priced per byte.
#[test]
fn an_image_entry_takes_the_flat_estimate_and_its_siblings_do_not() {
    let image = ContentItem::InputImage {
        image_url: format!("data:image/png;base64,{}", "A".repeat(40_000)),
        detail: None,
    };
    let text = ContentItem::InputText {
        text: "what is this?".to_string(),
    };
    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![image, text],
        phase: None,
        internal_chat_message_metadata_passthrough: Some(InternalChatMessageMetadataPassthrough {
            content_item_kinds: Some(vec![
                ContentItemKind("user.image".to_string()),
                ContentItemKind("user.text".to_string()),
            ]),
            ..Default::default()
        }),
    };

    let classification = classify(&item);
    let bytes = serde_json::to_vec(&item).expect("serializable item").len();
    let parts = &classification.parts;
    assert_eq!(
        vec![PartMedia::Image, PartMedia::Text],
        parts.iter().map(|part| part.media).collect::<Vec<_>>()
    );
    assert_eq!(
        IMAGE_TOKENS + text_tokens(parts[1].bytes),
        item_tokens(classification.category, parts, bytes)
    );
    // The base64 payload dominates the item, so pricing it as text would be several times over.
    assert!(text_tokens(bytes) > 4 * item_tokens(classification.category, parts, bytes));
}
