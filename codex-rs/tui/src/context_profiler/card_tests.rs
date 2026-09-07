use codex_context_profiler::ContextProfiler;
use codex_context_profiler::InvalidationReason;
use codex_context_profiler::ObservationStart;
use codex_context_profiler::ProfilerEvent;
use codex_context_profiler::ProfilerState;
use codex_context_profiler::TokenCost;
use codex_context_profiler::UsageSnapshot;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use ratatui::prelude::Line;

use super::super::card::Contributor;
use super::super::card::Row;
use crate::context_profiler::build;
use crate::context_profiler::new_context_card_cell;
use crate::history_cell::HistoryCell;

const FIRST_TURN: &str = "tu_1";
const SECOND_TURN: &str = "tu_2";
const WINDOW: i64 = 272_000;

fn kinds(kind: &str) -> Option<InternalChatMessageMetadataPassthrough> {
    Some(InternalChatMessageMetadataPassthrough {
        content_item_kinds: Some(vec![ContentItemKind(kind.to_string())]),
        ..Default::default()
    })
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

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: kinds("assistant.text"),
    }
}

fn reasoning() -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: None,
        encrypted_content: Some("x".repeat(120)),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call(name: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: "{\"path\":\"src/lib.rs\"}".to_string(),
        encrypted_function_args: None,
        call_id: "call_1".to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(call_id.to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("f".repeat(420)),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn anchor(input: i64, output: i64, items_seq: u64) -> UsageSnapshot {
    UsageSnapshot {
        reported_context_tokens: input + output,
        input_tokens: input,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: 0,
        items_seq,
    }
}

/// Two turns with consistent anchors: the first establishes a baseline, the second measures a
/// tool call and its output.
fn observe_session(profiler: &mut ContextProfiler, tool_name: &str) {
    let first_user = user_message("Summarise how the context profiler attributes tokens.");
    let agents = instruction_message("agents_md.instructions", &"a".repeat(600));
    let environment = instruction_message("environment_context.instructions", &"e".repeat(400));
    let first_reasoning = reasoning();
    let first_answer = assistant_message("It reconciles estimates against measured anchors.");

    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: FIRST_TURN,
    });
    profiler.observe(ProfilerEvent::WindowUpdated {
        turn_id: FIRST_TURN,
        window: WINDOW,
    });
    for item in [
        &first_user,
        &agents,
        &environment,
        &first_reasoning,
        &first_answer,
    ] {
        profiler.observe(ProfilerEvent::Item {
            turn_id: FIRST_TURN,
            item,
        });
    }
    profiler.observe(ProfilerEvent::Usage {
        turn_id: FIRST_TURN,
        usage: anchor(
            /*input*/ 20_000, /*output*/ 800, /*items_seq*/ 5,
        ),
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: FIRST_TURN,
        completed: true,
    });

    let second_user = user_message("Read src/lib.rs and tell me what it exports.");
    let second_reasoning = reasoning();
    let call = function_call(tool_name);
    let output = function_call_output("call_1");
    let second_answer = assistant_message("It re-exports the profiler types.");

    profiler.observe(ProfilerEvent::TurnStarted {
        turn_id: SECOND_TURN,
    });
    for item in [&second_user, &second_reasoning, &call] {
        profiler.observe(ProfilerEvent::Item {
            turn_id: SECOND_TURN,
            item,
        });
    }
    profiler.observe(ProfilerEvent::Usage {
        turn_id: SECOND_TURN,
        usage: anchor(
            /*input*/ 21_200, /*output*/ 600, /*items_seq*/ 8,
        ),
    });
    for item in [&output, &second_answer] {
        profiler.observe(ProfilerEvent::Item {
            turn_id: SECOND_TURN,
            item,
        });
    }
    profiler.observe(ProfilerEvent::Usage {
        turn_id: SECOND_TURN,
        usage: anchor(
            /*input*/ 22_600, /*output*/ 400, /*items_seq*/ 10,
        ),
    });
    profiler.observe(ProfilerEvent::TurnEnded {
        turn_id: SECOND_TURN,
        completed: true,
    });
}

fn session(start: ObservationStart, tool_name: &str) -> ContextProfiler {
    let mut profiler = ContextProfiler::new(start);
    observe_session(&mut profiler, tool_name);
    profiler
}

fn normal_profiler() -> ContextProfiler {
    session(ObservationStart::SessionStart, "read_file")
}

fn normal_state() -> ProfilerState {
    normal_profiler().state().clone()
}

fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn render(state: &ProfilerState, width: u16) -> String {
    let cell = new_context_card_cell(build(state));
    render_lines(&cell.display_lines(width)).join("\n")
}

#[test]
fn normal() {
    insta::assert_snapshot!(render(&normal_state(), 80));
}

#[test]
fn no_baseline() {
    let profiler = session(ObservationStart::MidStream, "read_file");
    insta::assert_snapshot!(render(profiler.state(), 80));
}

#[test]
fn pending_items() {
    let mut profiler = normal_profiler();
    let trailing = function_call_output("call_2");
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: "tu_3" });
    profiler.observe(ProfilerEvent::Item {
        turn_id: "tu_3",
        item: &trailing,
    });
    insta::assert_snapshot!(render(profiler.state(), 80));
}

#[test]
fn invalidated_dropped_events() {
    let mut profiler = normal_profiler();
    profiler.observe(ProfilerEvent::Invalidated {
        reason: InvalidationReason::EventsDropped { skipped: 3 },
    });
    insta::assert_snapshot!(render(profiler.state(), 80));
}

#[test]
fn invalidated_compacted() {
    let mut profiler = normal_profiler();
    profiler.observe(ProfilerEvent::Invalidated {
        reason: InvalidationReason::Compacted,
    });
    insta::assert_snapshot!(render(profiler.state(), 80));
}

#[test]
fn no_data() {
    let profiler = ContextProfiler::new(ObservationStart::SessionStart);
    insta::assert_snapshot!(render(profiler.state(), 80));
}

/// Labels give way to the ellipsis; numbers never do.
#[test]
fn narrow_40_columns() {
    insta::assert_snapshot!(render(&normal_state(), 40));
}

#[test]
fn categories_are_ordered_by_tokens_and_share_the_reported_context() {
    assert_eq!(
        build(&normal_state()).categories,
        vec![
            Row {
                name: "Agent messages",
                tokens: TokenCost::Estimated(1_156),
                share_percent: Some(5),
            },
            Row {
                name: "Tool outputs",
                tokens: TokenCost::Exact(800),
                share_percent: Some(3),
            },
            Row {
                name: "Tool calls",
                tokens: TokenCost::Estimated(573),
                share_percent: Some(2),
            },
            Row {
                name: "Instructions",
                tokens: TokenCost::Estimated(227),
                share_percent: Some(1),
            },
            Row {
                name: "Reasoning",
                tokens: TokenCost::Estimated(71),
                share_percent: Some(0),
            },
            Row {
                name: "User messages",
                tokens: TokenCost::Estimated(34),
                share_percent: Some(0),
            },
        ]
    );
}

#[test]
fn contributors_are_the_three_largest_groups() {
    let state = normal_state();
    assert!(
        state.snapshot.groups.len() > 3,
        "a fourth group is excluded"
    );
    assert_eq!(
        build(&state).contributors,
        vec![
            Contributor {
                rank: 1,
                category: "Tool calls",
                label: "read_file".to_string(),
                tokens: TokenCost::Estimated(1_373),
                share_percent: Some(6),
            },
            Contributor {
                rank: 2,
                category: "Agent messages",
                label: "assistant.text".to_string(),
                tokens: TokenCost::Estimated(756),
                share_percent: Some(3),
            },
            Contributor {
                rank: 3,
                category: "Agent messages",
                label: "assistant.text".to_string(),
                tokens: TokenCost::Exact(400),
                share_percent: Some(2),
            },
        ]
    );
}

#[test]
fn a_shrinking_reported_total_drifts_negative() {
    let mut profiler = normal_profiler();
    profiler.observe(ProfilerEvent::TurnStarted { turn_id: "tu_4" });
    profiler.observe(ProfilerEvent::Usage {
        turn_id: "tu_4",
        usage: anchor(
            /*input*/ 15_000, /*output*/ 0, /*items_seq*/ 10,
        ),
    });

    assert_eq!(
        build(profiler.state()).not_attributable,
        vec![
            Row {
                name: "System + tools baseline",
                tokens: TokenCost::Estimated(19_755),
                share_percent: Some(132),
            },
            Row {
                name: "Reconciliation drift",
                tokens: TokenCost::Estimated(-7_616),
                share_percent: None,
            },
        ]
    );
}

#[test]
fn without_a_baseline_the_remainder_is_reported_minus_attributed() {
    let profiler = session(ObservationStart::MidStream, "read_file");

    assert_eq!(
        build(profiler.state()).not_attributable,
        vec![Row {
            name: "Not attributed",
            tokens: TokenCost::Estimated(20_139),
            share_percent: Some(88),
        }]
    );
}

#[test]
fn long_contributor_label() {
    let profiler = session(ObservationStart::SessionStart, &"n".repeat(70));
    insta::assert_snapshot!(render(profiler.state(), 80));
}
