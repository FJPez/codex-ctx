//! Baseline and drift reconciliation: what the first anchor explains, and what later ones do not.

use super::*;
use crate::snapshot::InitialContextSummary;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

const TURN: &str = "turn-1";
const OTHER_TURN: &str = "turn-2";

fn kinds(kind: &str) -> Option<InternalChatMessageMetadataPassthrough> {
    Some(InternalChatMessageMetadataPassthrough {
        content_item_kinds: Some(vec![ContentItemKind(kind.to_string())]),
        ..Default::default()
    })
}

/// A user-role message tagged with the rollout kind that decides its category.
fn tagged_message(kind: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: kinds(kind),
    }
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: kinds("unknown"),
    }
}

/// A system-role message: core drops these, so their pricing direction is unknown.
fn system_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning_item() -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: None,
        encrypted_content: Some("opaque".to_string()),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn tool_output(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn anchor(total: i64, input: i64, output: i64, items_seq: u64) -> UsageSnapshot {
    UsageSnapshot {
        reported_context_tokens: total,
        input_tokens: input,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: 0,
        items_seq,
    }
}

fn observe_items(profiler: &mut ContextProfiler, turn_id: &str, items: &[&ResponseItem]) {
    for item in items {
        profiler.observe(ProfilerEvent::Item { turn_id, item });
    }
}

fn category_tokens(profiler: &ContextProfiler, category: Category) -> i64 {
    profiler
        .state()
        .snapshot
        .items
        .iter()
        .filter(|item| item.category == category)
        .map(|item| item.cost.tokens())
        .sum()
}

/// The eligible prefix every baseline test starts from: instructions, the user's turn, one response.
fn eligible_prefix() -> Vec<ResponseItem> {
    vec![
        tagged_message("agents_md.instructions", "project rules"),
        tagged_message("environments.environment_context", "cwd and shell"),
        tagged_message("user.text", "hello"),
        reasoning_item(),
        assistant_message("on it"),
    ]
}

fn fold_eligible_prefix(start: ObservationStart) -> ContextProfiler {
    let items = eligible_prefix();
    let mut profiler = ContextProfiler::new(start);
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &items.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_100, 2_000, 100, 5),
    });
    profiler
}

#[test]
fn an_eligible_first_request_establishes_the_baseline() {
    let profiler = fold_eligible_prefix(ObservationStart::SessionStart);

    let snapshot = &profiler.state().snapshot;
    assert_eq!(
        Some(2_100 - snapshot.attributed_tokens()),
        snapshot.baseline_tokens
    );
    assert_eq!(
        Some(InitialContextSummary {
            first_request_input_tokens: 2_000,
            estimated_user_input_tokens: category_tokens(&profiler, Category::UserMessage),
            estimated_instruction_tokens: category_tokens(&profiler, Category::Instructions),
        }),
        snapshot.initial_context
    );
    assert_eq!(0, snapshot.drift_tokens);
}

/// A profiler attached mid-session never saw the first request, so the residual is not a baseline.
/// Attribution itself is unaffected.
#[test]
fn a_mid_stream_start_never_claims_a_baseline() {
    let mut profiler = fold_eligible_prefix(ObservationStart::MidStream);
    let output = tool_output("call_1", "listing");
    observe_items(&mut profiler, TURN, &[&output]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_400, 2_400, 0, 6),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(None, snapshot.baseline_tokens);
    assert_eq!(None, snapshot.initial_context);
    assert_eq!(0, snapshot.drift_tokens);
    assert_eq!(TokenCost::Exact(300), snapshot.items[5].cost);
}

/// A response without usage breaks the chain from the session's start to the first anchor.
#[test]
fn usage_missing_before_the_first_anchor_disqualifies_the_baseline() {
    let items = eligible_prefix();
    let mut profiler = ContextProfiler::new(ObservationStart::SessionStart);
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &items.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::UsageMissing { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_100, 2_000, 100, 5),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(None, snapshot.baseline_tokens);
    assert_eq!(None, snapshot.initial_context);
}

#[test]
fn an_interrupted_first_turn_disqualifies_the_baseline() {
    let items = eligible_prefix();
    let mut profiler = ContextProfiler::new(ObservationStart::SessionStart);
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &items.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: false,
    });
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: OTHER_TURN,
        usage: anchor(2_100, 2_000, 100, 5),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(None, snapshot.baseline_tokens);
    assert_eq!(None, snapshot.initial_context);
}

#[test]
fn an_ambiguous_prefix_item_disqualifies_the_baseline() {
    let odd = system_message("who put this here");
    let mut profiler = ContextProfiler::new(ObservationStart::SessionStart);
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    let mut items = eligible_prefix();
    items.insert(0, odd);
    observe_items(&mut profiler, TURN, &items.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_100, 2_000, 100, 6),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(None, snapshot.baseline_tokens);
    assert_eq!(None, snapshot.initial_context);
}

/// A compaction summary is neither user input nor an instruction, so the residual route and the
/// summary route disagree and neither is trusted.
#[test]
fn a_compaction_item_in_the_prefix_disqualifies_the_baseline() {
    let mut profiler = ContextProfiler::new(ObservationStart::SessionStart);
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    let mut items = eligible_prefix();
    items.insert(0, tagged_message("compaction.summary", "what came before"));
    observe_items(&mut profiler, TURN, &items.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_100, 2_000, 100, 6),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(None, snapshot.baseline_tokens);
    assert_eq!(None, snapshot.initial_context);
}

/// Estimates larger than the measured request leave a negative residual, which is never a baseline.
#[test]
fn a_negative_residual_leaves_the_baseline_unclaimed() {
    let items = eligible_prefix();
    let mut profiler = ContextProfiler::new(ObservationStart::SessionStart);
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &items.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(30, 20, 10, 5),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(None, snapshot.baseline_tokens);
    assert_eq!(None, snapshot.initial_context);
}

#[test]
fn drift_tracks_the_residual_after_the_baseline() {
    let mut profiler = fold_eligible_prefix(ObservationStart::SessionStart);
    let priced = tool_output("call_1", "listing");
    observe_items(&mut profiler, TURN, &[&priced]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_400, 2_400, 0, 6),
    });
    assert_eq!(0, profiler.state().snapshot.drift_tokens);

    let odd = system_message("out of band");
    let stranded = tool_output("call_2", "more listing");
    observe_items(&mut profiler, TURN, &[&odd, &stranded]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(2_900, 2_900, 0, 8),
    });

    let snapshot = &profiler.state().snapshot;
    assert_eq!(
        vec![TokenCost::Estimated(9), TokenCost::Estimated(16)],
        snapshot.items[6..]
            .iter()
            .map(|item| item.cost)
            .collect::<Vec<_>>()
    );
    // 500 more reported tokens against the 25 the ambiguous span could only estimate.
    assert_eq!(475, snapshot.drift_tokens);
}

/// A turn that opens smaller than it closed shows up as negative drift, not as negative item costs.
#[test]
fn a_negative_cross_turn_delta_yields_negative_drift() {
    let mut profiler = fold_eligible_prefix(ObservationStart::SessionStart);
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    let follow_up = tagged_message("user.text", "and again");
    observe_items(&mut profiler, OTHER_TURN, &[&follow_up]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: OTHER_TURN,
        usage: anchor(1_900, 1_900, 0, 6),
    });

    assert_eq!(-208, profiler.state().snapshot.drift_tokens);
}
