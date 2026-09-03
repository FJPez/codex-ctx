use super::*;
use crate::event::InvalidationReason;
use crate::snapshot::ContextSnapshot;
use crate::usage::UsageSnapshot;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;

const TURN: &str = "tu_1";
const OTHER_TURN: &str = "tu_2";

fn message_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
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
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("ok".to_string()),
            success: Some(true),
        },
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

fn usage(total: i64, items_seq: u64) -> UsageSnapshot {
    UsageSnapshot {
        reported_context_tokens: total,
        input_tokens: total,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        items_seq,
    }
}

fn item_bytes(item: &ResponseItem) -> usize {
    serde_json::to_vec(item).expect("serializable item").len()
}

fn item_cost(item: &ResponseItem) -> TokenCost {
    TokenCost::Estimated(byte_proxy(item_bytes(item)))
}

fn summary(seq: u64, turn_index: u32, item: &ResponseItem, group: GroupKey) -> ItemSummary {
    ItemSummary {
        seq,
        turn_index,
        category: category(item),
        bytes: item_bytes(item),
        cost: item_cost(item),
        label: item_kind(item).to_string(),
        group,
        item_id: None,
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
    let user = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "hi".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let reasoning = reasoning_item();
    let answer = message_item("done");

    let mut profiler = ContextProfiler::new();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: TURN });
    for item in [&user, &reasoning, &answer] {
        profiler.observe(ProfilerEvent::Item {
            turn_id: TURN,
            item,
        });
    }
    profiler.observe(ProfilerEvent::Usage {
        turn_id: TURN,
        usage: usage(1_200, 3),
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: TURN,
        completed: true,
    });

    let items = vec![
        summary(1, 0, &user, GroupKey::Ungrouped(1)),
        summary(2, 0, &reasoning, GroupKey::Ungrouped(2)),
        summary(3, 0, &answer, GroupKey::Ungrouped(3)),
    ];
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
        unrecognized_fragment_count: 0,
        seq_mismatch_count: 0,
        anchors: vec![usage(1_200, 3)],
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
        usage: usage(900, 1),
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

    let state = profiler.state();
    assert_eq!(
        vec![summary(1, 0, &first, GroupKey::Ungrouped(1))],
        state.snapshot.items
    );
    assert_eq!(vec![usage(900, 1)], state.anchors);
    assert_eq!(Some(272_000), state.snapshot.window);
    assert_eq!(Some(InvalidationReason::Compacted), state.invalidated);
}

#[test]
fn interrupted_turn_has_no_measured_after_but_seeds_the_next_turn() {
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
    assert_eq!(Some(2_500), turns[1].measured_before);
}

#[test]
fn forged_usage_seq_counts_a_mismatch_and_still_anchors() {
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
    assert_eq!(1, state.seq_mismatch_count);
    assert_eq!(vec![usage(700, 2)], state.anchors);
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
