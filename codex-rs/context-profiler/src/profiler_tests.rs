use super::*;
use crate::classify::classify;
use crate::event::InvalidationReason;
use crate::snapshot::ContextSnapshot;
use crate::usage::UsageSnapshot;
use codex_protocol::models::ConfigurationReasoning;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

const TURN: &str = "tu_1";
const OTHER_TURN: &str = "tu_2";

/// Core stamps one `ContentItemKind` per content entry; every message here has exactly one entry.
fn kinds(kind: &str) -> Option<InternalChatMessageMetadataPassthrough> {
    Some(InternalChatMessageMetadataPassthrough {
        content_item_kinds: Some(vec![ContentItemKind(kind.to_string())]),
        ..Default::default()
    })
}

fn message_item(text: &str) -> ResponseItem {
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

fn custom_tool_call(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: call_id.to_string(),
        name: "shell".to_string(),
        namespace: None,
        input: "ls".to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn custom_tool_call_output(call_id: &str) -> ResponseItem {
    sized_tool_output(call_id, 2)
}

/// `text_len` bytes of payload on top of a fixed envelope, so byte weights can be dialled exactly.
fn sized_tool_output(call_id: &str, text_len: usize) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("x".repeat(text_len)),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

/// A user message whose single content entry is an inline image data URL.
fn image_message(payload_len: usize) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image_url: format!("data:image/png;base64,{}", "A".repeat(payload_len)),
            detail: None,
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: kinds("user.image"),
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: kinds("user.text"),
    }
}

/// A contextual fragment core injects as a user-role message, tagged with its own kind.
fn instruction_message(kind: &str, text: &str) -> ResponseItem {
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

fn unknown_role_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "tool".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn configuration_update() -> ResponseItem {
    ResponseItem::ConfigurationUpdate {
        reasoning: ConfigurationReasoning {
            effort: ReasoningEffort::Medium,
        },
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

fn usage(total: i64, items_seq: u64) -> UsageSnapshot {
    anchor(total, total, /*output*/ 0, items_seq)
}

fn observe_items(profiler: &mut ContextProfiler, turn_id: &str, items: &[&ResponseItem]) {
    for item in items {
        profiler.observe(ProfilerEvent::Item { turn_id, item });
    }
}

fn costs(profiler: &ContextProfiler) -> Vec<TokenCost> {
    profiler
        .state()
        .snapshot
        .items
        .iter()
        .map(|item| item.cost)
        .collect()
}

fn item_bytes(item: &ResponseItem) -> usize {
    serde_json::to_vec(item).expect("serializable item").len()
}

/// The initial estimate an item carries until an anchor prices it.
fn item_cost(item: &ResponseItem) -> TokenCost {
    let classification = classify(item);
    TokenCost::Estimated(item_tokens(
        classification.category,
        &classification.parts,
        item_bytes(item),
    ))
}

/// The apportioning weights are the items' current estimates, never their bytes.
fn weights(items: &[&ResponseItem]) -> Vec<i64> {
    items.iter().map(|item| item_cost(item).tokens()).collect()
}

fn summary(seq: u64, turn_index: u32, item: &ResponseItem, group: GroupKey) -> ItemSummary {
    let classification = classify(item);
    ItemSummary {
        seq,
        turn_index,
        category: classification.category,
        pricing: classification.pricing,
        bytes: item_bytes(item),
        cost: item_cost(item),
        label: item_kind(item).to_string(),
        group,
        item_id: None,
        parts: classification.parts,
    }
}

fn group_of(summary: &ItemSummary) -> ItemGroup {
    ItemGroup {
        key: summary.group.clone(),
        category: summary.category,
        cost: summary.cost,
        label: summary.label.clone(),
        members: vec![summary.seq],
    }
}

#[test]
fn single_turn_folds_items_anchor_and_turn_delta() {
    let user = user_message("hi");
    let reasoning = reasoning_item();
    let answer = message_item("done");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&user, &reasoning, &answer]);
    let usage = UsageSnapshot {
        reasoning_output_tokens: 30,
        ..anchor(1_200, 1_200, 40, 3)
    };
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage.clone(),
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });

    // The reasoning item takes the measured reasoning subset and the message takes the rest; the
    // user message has no earlier same-turn anchor to price it, so it keeps its estimate.
    let mut items = vec![
        summary(1, 0, &user, GroupKey::Ungrouped(1)),
        summary(2, 0, &reasoning, GroupKey::Ungrouped(2)),
        summary(3, 0, &answer, GroupKey::Ungrouped(3)),
    ];
    items[1].cost = TokenCost::Exact(30);
    items[2].cost = TokenCost::Exact(10);
    let estimated_added = items.iter().map(|item| item.cost.tokens()).sum();
    let expected = ProfilerState {
        snapshot: ContextSnapshot {
            window: None,
            reported_context_tokens: Some(1_200),
            initial_context: None,
            by_category: vec![
                (Category::UserMessage, items[0].cost),
                (Category::AgentMessage, items[2].cost),
                (Category::Reasoning, items[1].cost),
            ],
            baseline_tokens: None,
            drift_tokens: 0,
            groups: items.iter().map(group_of).collect(),
            turns: vec![TurnDelta {
                turn_id: TURN.to_string(),
                index: 0,
                item_seq_range: 1..=3,
                estimated_added,
                measured_before: None,
                measured_after: Some(1_200),
            }],
            items,
        },
        invalidated: None,
        classification_warning_count: 0,
        unsizable_item_count: 0,
        anchors: vec![usage],
    };
    assert_eq!(&expected, profiler.state());
}

#[test]
fn call_and_output_share_one_group_across_turns() {
    let call = custom_tool_call("call_1");
    let filler = message_item("thinking out loud");
    let output = custom_tool_call_output("call_1");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &call,
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    profiler.observe(ProfilerEvent::Item {
        turn_id: OTHER_TURN,
        item: &filler,
    });
    profiler.observe(ProfilerEvent::Item {
        turn_id: OTHER_TURN,
        item: &output,
    });

    let expected = vec![
        ItemGroup {
            key: GroupKey::ToolCall("call_1".to_string()),
            category: Category::ToolCall,
            cost: TokenCost::Estimated(item_cost(&call).tokens() + item_cost(&output).tokens()),
            label: "CustomToolCall".to_string(),
            members: vec![1, 3],
        },
        group_of(&summary(2, 1, &filler, GroupKey::Ungrouped(2))),
    ];
    assert_eq!(expected, profiler.state().snapshot.groups);
}

#[test]
fn item_without_open_turn_opens_implicit_turn() {
    let item = message_item("mid-stream attach");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &item,
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });

    let expected = vec![TurnDelta {
        turn_id: TURN.to_string(),
        index: 0,
        item_seq_range: 1..=1,
        estimated_added: item_cost(&item).tokens(),
        measured_before: None,
        measured_after: None,
    }];
    assert_eq!(expected, profiler.state().snapshot.turns);
    assert_eq!(
        vec![summary(1, 0, &item, GroupKey::Ungrouped(1))],
        profiler.state().snapshot.items
    );
}

#[test]
fn invalidation_freezes_everything_but_the_window() {
    let first = message_item("before");
    let second = message_item("after");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &first,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(900, 900, 55, 1),
    });
    profiler.observe(ProfilerEvent::Invalidated {
        reason: InvalidationReason::Compacted,
    });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &second,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(4_000, 2),
    });
    profiler.observe(ProfilerEvent::WindowUpdated {
        turn_id: TURN,
        window: 272_000,
    });

    let mut frozen = summary(1, 0, &first, GroupKey::Ungrouped(1));
    frozen.cost = TokenCost::Exact(55);
    let state = profiler.state();
    assert_eq!(vec![frozen], state.snapshot.items);
    assert_eq!(vec![anchor(900, 900, 55, 1)], state.anchors);
    assert_eq!(Some(272_000), state.snapshot.window);
    assert_eq!(Some(InvalidationReason::Compacted), state.invalidated);
}

#[test]
fn interrupted_turn_leaves_both_turns_unmeasured() {
    let item = message_item("interrupted");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &item,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(2_500, 1),
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: false,
    });
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: OTHER_TURN,
        completed: true,
    });

    let turns = &profiler.state().snapshot.turns;
    assert_eq!(None, turns[0].measured_added());
    assert_eq!(None, turns[0].measured_after);
    assert_eq!(None, turns[1].measured_before);
    assert_eq!(None, turns[1].measured_added());
}

#[test]
fn forged_usage_seq_invalidates_without_anchoring() {
    let item = message_item("one item");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &item,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(700, 2),
    });

    let state = profiler.state();
    assert_eq!(
        Some(InvalidationReason::SequenceMismatch {
            anchor_items_seen: 2,
            profiler_items_seen: 1,
        }),
        state.invalidated
    );
    assert_eq!(Vec::<UsageSnapshot>::new(), state.anchors);
    assert_eq!(item_cost(&item), state.snapshot.items[0].cost);
}

/// A response without usage is a boundary: nothing across it is priced, but the next response is.
#[test]
fn missing_usage_isolates_the_next_response() {
    let reasoning_a = reasoning_item();
    let call_a = custom_tool_call("call_a");
    let output_a = custom_tool_call_output("call_a");
    let reasoning_b = reasoning_item();

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: OTHER_TURN,
        usage: anchor(1_000, 1_000, 0, 0),
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: OTHER_TURN,
        completed: true,
    });
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    for item in [&reasoning_a, &call_a] {
        profiler.observe(ProfilerEvent::Item {
            turn_id: TURN,
            item,
        });
    }
    profiler.observe(ProfilerEvent::UsageMissing { turn_id: TURN });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &output_a,
    });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &reasoning_b,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 100,
            ..anchor(1_600, 1_500, 100, 4)
        },
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });

    let state = profiler.state();
    let costs: Vec<TokenCost> = state.snapshot.items.iter().map(|item| item.cost).collect();
    assert_eq!(
        vec![
            item_cost(&reasoning_a),
            item_cost(&call_a),
            item_cost(&output_a),
            TokenCost::Exact(100),
        ],
        costs
    );
    assert_eq!(None, state.invalidated);
    let turn = &state.snapshot.turns[1];
    assert_eq!(Some(1_000), turn.measured_before);
    assert_eq!(Some(1_600), turn.measured_after);
}

/// Missing usage on a turn's final response leaves both turn boundaries unmeasured.
#[test]
fn missing_usage_at_turn_end_clears_both_boundaries() {
    let item = message_item("last response");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 0),
    });
    profiler.observe(ProfilerEvent::Item {
        turn_id: TURN,
        item: &item,
    });
    profiler.observe(ProfilerEvent::UsageMissing { turn_id: TURN });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: OTHER_TURN,
        completed: true,
    });

    let turns = &profiler.state().snapshot.turns;
    assert_eq!(None, turns[0].measured_after);
    assert_eq!(None, turns[1].measured_before);
}

#[test]
fn folding_the_same_events_twice_yields_identical_state() {
    let call = custom_tool_call("call_1");
    let output = custom_tool_call_output("call_1");
    let reasoning = reasoning_item();
    let fold = |profiler: &mut ContextProfiler| {
        profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
        for item in [&call, &reasoning, &output] {
            profiler.observe(ProfilerEvent::Item {
                turn_id: TURN,
                item,
            });
        }
        profiler.observe(ProfilerEvent::WindowUpdated {
            turn_id: TURN,
            window: 272_000,
        });
        profiler.observe(ProfilerEvent::Usage {
            turn_id: TURN,
            usage: usage(3_100, 3),
        });
        profiler.observe(ProfilerEvent::TurnEnded {
            turn_id: TURN,
            completed: true,
        });
    };

    let mut first = ContextProfiler::new();
    let mut second = ContextProfiler::new();
    fold(&mut first);
    fold(&mut second);

    let encode = |profiler: &ContextProfiler| {
        serde_json::to_string(profiler.state()).expect("serializable state")
    };
    assert_eq!(encode(&first), encode(&second));
}

#[test]
fn capture_shape_prices_the_tool_output_from_the_anchor_delta() {
    let prompts: Vec<ResponseItem> = (1..=5).map(|n| user_message(&format!("u{n}"))).collect();
    let reasoning = reasoning_item();
    let answer = message_item("on it");
    let call = custom_tool_call("call_1");
    let output = custom_tool_call_output("call_1");
    let second_reasoning = reasoning_item();

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    let first_span: Vec<&ResponseItem> =
        prompts.iter().chain([&reasoning, &answer, &call]).collect();
    observe_items(&mut profiler, TURN, &first_span);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 100,
            ..anchor(25_422, 25_230, 192, 8)
        },
    });
    observe_items(&mut profiler, TURN, &[&output, &second_reasoning]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 93,
            ..anchor(29_230, 29_137, 93, 10)
        },
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });

    // 100 of the 192 output tokens are measured reasoning; the message and the call share the 92
    // that are left.
    let shares = apportion(92, &weights(&[&answer, &call]));
    let expected: Vec<TokenCost> = prompts
        .iter()
        .map(item_cost)
        .chain([TokenCost::Exact(100)])
        .chain(shares.iter().copied().map(TokenCost::Estimated))
        .chain([TokenCost::Exact(3_715), TokenCost::Exact(93)])
        .collect();
    assert_eq!(expected, costs(&profiler));
    assert_eq!(92, shares.iter().sum::<i64>());
}

#[test]
fn first_anchor_prices_output_items_without_a_previous_anchor() {
    let reasoning = reasoning_item();
    let call = custom_tool_call("call_1");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&reasoning]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 77,
            ..anchor(1_000, 1_000, 77, 1)
        },
    });
    observe_items(&mut profiler, TURN, &[&call]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(1_100, 1_100, 40, 2),
    });

    assert_eq!(
        vec![TokenCost::Exact(77), TokenCost::Exact(40)],
        costs(&profiler)
    );
}

#[test]
fn two_input_items_split_the_delta_by_estimate() {
    let envelope = item_bytes(&sized_tool_output("call_0", 0));
    let small = sized_tool_output("call_1", 1);
    let big = sized_tool_output("call_2", 2 * envelope + 3);

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 0),
    });
    observe_items(&mut profiler, TURN, &[&big, &small]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_100, 2),
    });

    // Both items are plain text, so the estimator carries the 3:1 byte ratio into the weights.
    assert_eq!(3 * item_bytes(&small), item_bytes(&big));
    let shares = apportion(100, &weights(&[&big, &small]));
    assert_eq!(
        vec![
            TokenCost::Estimated(shares[0]),
            TokenCost::Estimated(shares[1]),
        ],
        costs(&profiler)
    );
    assert_eq!(vec![75, 25], shares);
}

#[test]
fn equal_weights_hand_the_rounding_remainder_to_the_last_item() {
    let outputs: Vec<ResponseItem> = (1..=3)
        .map(|n| sized_tool_output(&format!("call_{n}"), 8))
        .collect();

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 0),
    });
    observe_items(&mut profiler, TURN, &outputs.iter().collect::<Vec<_>>());
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_100, 3),
    });

    assert_eq!(
        vec![
            TokenCost::Estimated(33),
            TokenCost::Estimated(33),
            TokenCost::Estimated(34),
        ],
        costs(&profiler)
    );
}

#[test]
fn weightless_items_split_evenly_with_the_remainder_first() {
    assert_eq!(vec![34, 33, 33], apportion(100, &[0, 0, 0]));
    assert_eq!(Vec::<i64>::new(), apportion(100, &[]));
    assert_eq!(vec![0, 0], apportion(-5, &[3, 1]));
}

#[test]
fn a_negative_delta_leaves_the_estimate_in_place() {
    let output = custom_tool_call_output("call_1");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(5_000, 0),
    });
    observe_items(&mut profiler, TURN, &[&output]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(4_000, 1),
    });

    assert_eq!(vec![item_cost(&output)], costs(&profiler));
}

#[test]
fn items_stranded_by_an_interrupted_turn_are_never_repriced() {
    let anchored = custom_tool_call_output("call_1");
    let stranded = custom_tool_call_output("call_2");
    let next_turn = custom_tool_call_output("call_3");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&anchored]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 1),
    });
    observe_items(&mut profiler, TURN, &[&stranded]);
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: false,
    });
    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: OTHER_TURN,
    });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: OTHER_TURN,
        usage: usage(1_000, 2),
    });
    observe_items(&mut profiler, OTHER_TURN, &[&next_turn]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: OTHER_TURN,
        usage: usage(1_300, 3),
    });

    assert_eq!(
        vec![
            item_cost(&anchored),
            item_cost(&stranded),
            TokenCost::Exact(300),
        ],
        costs(&profiler)
    );
}

#[test]
fn repricing_rebuilds_the_aggregates_whole() {
    let reasoning = reasoning_item();
    let answer = message_item("done");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&reasoning, &answer]);

    let estimated = vec![
        summary(1, 0, &reasoning, GroupKey::Ungrouped(1)),
        summary(2, 0, &answer, GroupKey::Ungrouped(2)),
    ];
    assert_eq!(
        vec![
            (Category::AgentMessage, estimated[1].cost),
            (Category::Reasoning, estimated[0].cost),
        ],
        profiler.state().snapshot.by_category
    );
    assert_eq!(
        estimated.iter().map(group_of).collect::<Vec<_>>(),
        profiler.state().snapshot.groups
    );

    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 40,
            ..anchor(900, 900, 100, 2)
        },
    });

    let mut repriced = estimated;
    repriced[0].cost = TokenCost::Exact(40);
    repriced[1].cost = TokenCost::Exact(60);
    assert_eq!(
        vec![
            (Category::AgentMessage, repriced[1].cost),
            (Category::Reasoning, repriced[0].cost),
        ],
        profiler.state().snapshot.by_category
    );
    assert_eq!(
        repriced.iter().map(group_of).collect::<Vec<_>>(),
        profiler.state().snapshot.groups
    );
    assert_eq!(repriced, profiler.state().snapshot.items);
}

#[test]
fn a_group_is_exact_only_when_every_member_is() {
    let exact_call = custom_tool_call("call_1");
    let exact_output = custom_tool_call_output("call_1");
    let shared_message = message_item("thinking");
    let shared_call = custom_tool_call("call_2");
    let shared_output = custom_tool_call_output("call_2");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&exact_call]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(1_000, 1_000, 50, 1),
    });
    observe_items(
        &mut profiler,
        TURN,
        &[&exact_output, &shared_message, &shared_call],
    );
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(1_260, 1_200, 60, 4),
    });
    observe_items(&mut profiler, TURN, &[&shared_output]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(1_400, 1_400, 0, 5),
    });

    let shares = apportion(60, &weights(&[&shared_message, &shared_call]));
    let groups = &profiler.state().snapshot.groups;
    assert_eq!(TokenCost::Exact(250), groups[0].cost);
    assert_eq!(TokenCost::Estimated(shares[1] + 140), groups[2].cost);
}

/// Injected instruction fragments are user-role input, so an anchor delta prices them; before the
/// pricing kind was split off the display category they were left unpriced forever.
#[test]
fn instructions_share_the_input_delta_with_tool_output() {
    let reminder = instruction_message("current_time.reminder", "it is late");
    let output = custom_tool_call_output("call_1");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 0),
    });
    observe_items(&mut profiler, TURN, &[&reminder, &output]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_100, 2),
    });

    assert_eq!(
        Category::Instructions,
        profiler.state().snapshot.items[0].category
    );
    let shares = apportion(100, &weights(&[&reminder, &output]));
    assert_eq!(
        vec![
            TokenCost::Estimated(shares[0]),
            TokenCost::Estimated(shares[1]),
        ],
        costs(&profiler)
    );
    assert_eq!(100, shares.iter().sum::<i64>());
}

/// A `ConfigurationUpdate` could land in either measured total, so its span is left alone - but the
/// anchor still closes the span, and the next one prices normally.
#[test]
fn an_ambiguous_item_leaves_its_span_estimated_and_the_next_span_recovers() {
    let update = configuration_update();
    let poisoned = sized_tool_output("call_1", 4);
    let recovered = custom_tool_call_output("call_2");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 0),
    });
    observe_items(&mut profiler, TURN, &[&update, &poisoned]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_100, 2),
    });
    observe_items(&mut profiler, TURN, &[&recovered]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_400, 3),
    });

    let state = profiler.state();
    assert_eq!(
        vec![
            item_cost(&update),
            item_cost(&poisoned),
            TokenCost::Exact(300),
        ],
        costs(&profiler)
    );
    assert_eq!(0, state.classification_warning_count);
    assert_eq!(3, state.anchors.len());
}

/// An unknown role is both a display warning and a pricing ambiguity, and neither invalidates.
#[test]
fn an_unknown_role_message_poisons_only_its_own_span() {
    let odd = unknown_role_message("mystery");
    let reasoning = reasoning_item();
    let later = reasoning_item();

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(1_000, 1_000, 0, 0),
    });
    observe_items(&mut profiler, TURN, &[&odd, &reasoning]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: anchor(1_100, 1_050, 50, 2),
    });
    observe_items(&mut profiler, TURN, &[&later]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 60,
            ..anchor(1_160, 1_100, 60, 3)
        },
    });

    let state = profiler.state();
    assert_eq!(
        vec![item_cost(&odd), item_cost(&reasoning), TokenCost::Exact(60),],
        costs(&profiler)
    );
    assert_eq!(1, state.classification_warning_count);
    assert_eq!(None, state.invalidated);
}

/// `reasoning_output_tokens` is a subset of `output_tokens`, so the two kinds of output are priced
/// from their own measured totals rather than apportioned against each other.
#[test]
fn reasoning_and_generated_output_are_priced_from_separate_measured_totals() {
    let reasoning = reasoning_item();
    let answer = message_item("done");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&reasoning, &answer]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 100,
            ..anchor(2_000, 1_850, 150, 2)
        },
    });

    assert_eq!(
        vec![TokenCost::Exact(100), TokenCost::Exact(50)],
        costs(&profiler)
    );
}

#[test]
fn two_reasoning_items_share_the_measured_reasoning_subset() {
    let first = reasoning_item();
    let second = reasoning_item();
    let answer = message_item("done");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    observe_items(&mut profiler, TURN, &[&first, &second, &answer]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: UsageSnapshot {
            reasoning_output_tokens: 100,
            ..anchor(2_000, 1_850, 150, 3)
        },
    });

    let shares = apportion(100, &weights(&[&first, &second]));
    assert_eq!(
        vec![
            TokenCost::Estimated(shares[0]),
            TokenCost::Estimated(shares[1]),
            TokenCost::Exact(50),
        ],
        costs(&profiler)
    );
}

/// An inline image is tens of kilobytes of base64 that cost about a paragraph. Weighting by bytes
/// would hand it nearly the whole delta; weighting by the estimate hands it its flat image cost.
#[test]
fn an_image_takes_an_estimate_weighted_share_not_a_byte_weighted_one() {
    let output = sized_tool_output("call_1", 20_000);
    let image = image_message(40_000);

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_000, 0),
    });
    observe_items(&mut profiler, TURN, &[&output, &image]);
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(4_000, 2),
    });

    let shares = apportion(3_000, &weights(&[&output, &image]));
    assert_eq!(
        vec![
            TokenCost::Estimated(shares[0]),
            TokenCost::Estimated(shares[1]),
        ],
        costs(&profiler)
    );
    let by_bytes = apportion(
        3_000,
        &[item_bytes(&output) as i64, item_bytes(&image) as i64],
    );
    assert!(
        shares[1] * 2 < by_bytes[1] && shares[1] * 2 < 3_000,
        "image took {} of 3,000, against {} by bytes",
        shares[1],
        by_bytes[1]
    );
}
