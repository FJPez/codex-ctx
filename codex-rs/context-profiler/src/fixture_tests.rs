//! Capture-derived fixtures: measured sessions replayed through the fold.
//!
//! Every byte size and every anchor number below is transcribed from a real capture, so a change in
//! attribution shows up as a disagreement with measured reality rather than with a hand-written
//! expectation. Item payloads are padding: only the serialised length is faithful.

use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;

/// Turn ids of the live capture, `context-profiler-20260902-143758-75395.jsonl`.
const TURN_1: &str = "01a0628e-459a-7802-adbd-3d58f4881668";
const TURN_2: &str = "01a0628e-cbd5-7821-8865-45c160818f4b";
const WINDOW: i64 = 258_400;

/// The item shapes the captures contain. Captures record a kind and a byte count, never a role, so
/// message roles are inferred from position: the message opening a turn is the user's.
#[derive(Debug, Clone)]
enum Kind {
    UserMessage,
    AgentMessage,
    Reasoning,
    ToolCall(&'static str),
    ToolOutput(&'static str),
}

impl Kind {
    fn build(&self, payload: String) -> ResponseItem {
        match self {
            Self::UserMessage => ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: payload }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            Self::AgentMessage => ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText { text: payload }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            Self::Reasoning => ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some(payload),
                internal_chat_message_metadata_passthrough: None,
            },
            Self::ToolCall(call_id) => ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: (*call_id).to_string(),
                name: "shell".to_string(),
                namespace: None,
                input: payload,
                internal_chat_message_metadata_passthrough: None,
            },
            Self::ToolOutput(call_id) => ResponseItem::CustomToolCallOutput {
                id: None,
                call_id: (*call_id).to_string(),
                name: None,
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text(payload),
                    success: Some(true),
                },
                internal_chat_message_metadata_passthrough: None,
            },
        }
    }
}

fn serialized_len(item: &ResponseItem) -> usize {
    serde_json::to_vec(item).expect("serializable item").len()
}

/// Builds an item whose serialised length is exactly `bytes`, by padding its string payload.
///
/// The padding is unescaped ASCII, so one character costs one byte and the arithmetic is exact.
fn sized(kind: Kind, bytes: usize) -> ResponseItem {
    let overhead = serialized_len(&kind.build(String::new()));
    assert!(
        bytes >= overhead,
        "{kind:?} serialises to {overhead} bytes empty, so it cannot be sized to {bytes}"
    );
    let item = kind.build("x".repeat(bytes - overhead));
    assert_eq!(
        bytes,
        serialized_len(&item),
        "padded {kind:?} missed its size"
    );
    item
}

/// One capture record, in arrival order.
enum Record {
    TurnStarted(&'static str),
    /// Boxed so one big variant does not inflate every record in the table.
    Item {
        turn: &'static str,
        item: Box<ResponseItem>,
    },
    Usage {
        turn: &'static str,
        usage: UsageSnapshot,
    },
    Window {
        turn: &'static str,
        window: i64,
    },
    TurnEnded {
        turn: &'static str,
        completed: bool,
    },
}

fn item(turn: &'static str, kind: Kind, bytes: usize) -> Record {
    Record::Item {
        turn,
        item: Box::new(sized(kind, bytes)),
    }
}

/// The captures' cache counters are transcribed nowhere because the fold never reads them.
fn usage_at(turn: &'static str, total: i64, input: i64, output: i64, items_seq: u64) -> Record {
    Record::Usage {
        turn,
        usage: UsageSnapshot {
            reported_context_tokens: total,
            input_tokens: input,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: output,
            reasoning_output_tokens: 0,
            items_seq,
        },
    }
}

fn window(turn: &'static str) -> Record {
    Record::Window {
        turn,
        window: WINDOW,
    }
}

fn turn_ended(turn: &'static str, completed: bool) -> Record {
    Record::TurnEnded { turn, completed }
}

fn events(records: &[Record]) -> Vec<ProfilerEvent<'_>> {
    records
        .iter()
        .map(|record| match record {
            Record::TurnStarted(turn) => ProfilerEvent::TurnStarted { turn_id: turn },
            Record::Item { turn, item } => ProfilerEvent::Item {
                turn_id: turn,
                item: item.as_ref(),
            },
            Record::Usage { turn, usage } => ProfilerEvent::Usage {
                turn_id: turn,
                usage: usage.clone(),
            },
            Record::Window { turn, window } => ProfilerEvent::WindowUpdated {
                turn_id: turn,
                window: *window,
            },
            Record::TurnEnded { turn, completed } => ProfilerEvent::TurnEnded {
                turn_id: turn,
                completed: *completed,
            },
        })
        .collect()
}

fn fold(records: &[Record]) -> ContextProfiler {
    let mut profiler = ContextProfiler::new();
    for event in events(records) {
        profiler.observe(event);
    }
    profiler
}

fn costs(state: &ProfilerState) -> Vec<TokenCost> {
    state.snapshot.items.iter().map(|item| item.cost).collect()
}

/// Findings §9.2: four tool outputs whose measured token costs are 1,040 / 3,373 / 2,219 / 5,043,
/// at observed JSON sizes of 4,792 / 15,876 / 14,152 / 24,567 bytes.
///
/// The findings record the deltas and the sizes but not the anchors they were derived from, so the
/// ladder is reconstructed: it starts from the §6.2 anchor (input 25,230 + output 192 = 25,422) and
/// each rung's `input_tokens` is the previous rung's total plus the measured delta.
fn ladder_records() -> Vec<Record> {
    let rungs = [
        (4_792_usize, 1_040_i64, 93_i64),
        (15_876, 3_373, 128),
        (14_152, 2_219, 156),
        (24_567, 5_043, 165),
    ];
    let mut records = vec![
        Record::TurnStarted(TURN_1),
        usage_at(TURN_1, 25_422, 25_230, 192, 0),
    ];
    let mut total = 25_422;
    let mut seq = 0;
    for (index, (bytes, delta, output)) in rungs.into_iter().enumerate() {
        let call_id: &'static str =
            ["call_rung_1", "call_rung_2", "call_rung_3", "call_rung_4"][index];
        records.push(item(TURN_1, Kind::ToolOutput(call_id), bytes));
        records.push(item(TURN_1, Kind::Reasoning, 1_593));
        seq += 2;
        let input = total + delta;
        total = input + output;
        records.push(usage_at(TURN_1, total, input, output, seq));
    }
    records.push(turn_ended(TURN_1, /*completed*/ true));
    records
}

#[test]
fn the_measured_ladder_prices_every_tool_output_to_the_token() {
    let state = fold(&ladder_records()).state().clone();

    // Each span holds one tool output and one reasoning item, so both take a whole measured total.
    let expected = vec![
        TokenCost::Exact(1_040),
        TokenCost::Exact(93),
        TokenCost::Exact(3_373),
        TokenCost::Exact(128),
        TokenCost::Exact(2_219),
        TokenCost::Exact(156),
        TokenCost::Exact(5_043),
        TokenCost::Exact(165),
    ];
    assert_eq!(expected, costs(&state));
    assert_eq!(
        vec![4_792, 1_593, 15_876, 1_593, 14_152, 1_593, 24_567, 1_593],
        state
            .snapshot
            .items
            .iter()
            .map(|item| item.bytes)
            .collect::<Vec<_>>()
    );
}

/// The live capture, record for record: two completed turns, 42 items, 13 anchors.
fn live_trace_records() -> Vec<Record> {
    vec![
        Record::TurnStarted(TURN_1),
        item(TURN_1, Kind::UserMessage, 33_504),
        item(TURN_1, Kind::UserMessage, 2_604),
        item(TURN_1, Kind::UserMessage, 582),
        item(TURN_1, Kind::UserMessage, 25_542),
        item(TURN_1, Kind::UserMessage, 357),
        item(TURN_1, Kind::Reasoning, 1_677),
        item(TURN_1, Kind::AgentMessage, 446),
        item(TURN_1, Kind::ToolCall("call_gLxpy6YKiODVbnikNYW2y9v8"), 570),
        usage_at(TURN_1, 25_328, 25_180, 148, 8),
        item(
            TURN_1,
            Kind::ToolOutput("call_gLxpy6YKiODVbnikNYW2y9v8"),
            4_794,
        ),
        window(TURN_1),
        item(TURN_1, Kind::ToolCall("call_OHo6iaiyDS7ox5yUN3tS4hVb"), 510),
        usage_at(TURN_1, 26_437, 26_368, 69, 10),
        item(
            TURN_1,
            Kind::ToolOutput("call_OHo6iaiyDS7ox5yUN3tS4hVb"),
            696,
        ),
        window(TURN_1),
        item(TURN_1, Kind::Reasoning, 1_721),
        item(TURN_1, Kind::ToolCall("call_L4NSXPrT6B7qNK1Sz7pdugWU"), 606),
        usage_at(TURN_1, 26_674, 26_536, 138, 13),
        item(
            TURN_1,
            Kind::ToolOutput("call_L4NSXPrT6B7qNK1Sz7pdugWU"),
            29_430,
        ),
        window(TURN_1),
        item(TURN_1, Kind::AgentMessage, 1_095),
        usage_at(TURN_1, 33_318, 33_150, 168, 15),
        window(TURN_1),
        turn_ended(TURN_1, /*completed*/ true),
        Record::TurnStarted(TURN_2),
        item(TURN_2, Kind::UserMessage, 395),
        item(TURN_2, Kind::Reasoning, 1_997),
        item(TURN_2, Kind::AgentMessage, 482),
        item(TURN_2, Kind::ToolCall("call_SGig7mv17VI5Byf74cKLIalf"), 821),
        usage_at(TURN_2, 33_386, 33_113, 273, 19),
        item(
            TURN_2,
            Kind::ToolOutput("call_SGig7mv17VI5Byf74cKLIalf"),
            4_903,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 2_189),
        item(TURN_2, Kind::ToolCall("call_e0harE27Zt9PAJF1nWx0SlYZ"), 776),
        usage_at(TURN_2, 34_729, 34_451, 278, 22),
        item(
            TURN_2,
            Kind::ToolOutput("call_e0harE27Zt9PAJF1nWx0SlYZ"),
            41_448,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 2_425),
        item(TURN_2, Kind::ToolCall("call_UAymilgLko4xZiKywsmKLvgH"), 528),
        usage_at(TURN_2, 43_905, 43_671, 234, 25),
        item(
            TURN_2,
            Kind::ToolOutput("call_UAymilgLko4xZiKywsmKLvgH"),
            18_223,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 1_997),
        item(TURN_2, Kind::ToolCall("call_XOLff1onpEbINyh0wy0dnTzv"), 531),
        usage_at(TURN_2, 48_164, 47_992, 172, 28),
        item(
            TURN_2,
            Kind::ToolOutput("call_XOLff1onpEbINyh0wy0dnTzv"),
            36_533,
        ),
        window(TURN_2),
        item(TURN_2, Kind::ToolCall("call_w4kYMbPNkelDoMNit22IuK7B"), 532),
        usage_at(TURN_2, 55_749, 55_671, 78, 30),
        item(
            TURN_2,
            Kind::ToolOutput("call_w4kYMbPNkelDoMNit22IuK7B"),
            32_609,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 1_593),
        item(TURN_2, Kind::ToolCall("call_m1ni5pQV0Yok1mQnIy1fBl29"), 525),
        usage_at(TURN_2, 62_518, 62_417, 101, 33),
        item(
            TURN_2,
            Kind::ToolOutput("call_m1ni5pQV0Yok1mQnIy1fBl29"),
            37_513,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 1_549),
        item(TURN_2, Kind::ToolCall("call_8cygZrZdn7A00DPdBsrw8Ubd"), 528),
        usage_at(TURN_2, 70_306, 70_221, 85, 36),
        item(
            TURN_2,
            Kind::ToolOutput("call_8cygZrZdn7A00DPdBsrw8Ubd"),
            37_817,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 1_549),
        item(TURN_2, Kind::ToolCall("call_mMuuZ0wVaFwXpM02IlFirMcY"), 529),
        usage_at(TURN_2, 77_209, 77_118, 91, 39),
        item(
            TURN_2,
            Kind::ToolOutput("call_mMuuZ0wVaFwXpM02IlFirMcY"),
            37_067,
        ),
        window(TURN_2),
        item(TURN_2, Kind::Reasoning, 1_633),
        item(TURN_2, Kind::AgentMessage, 908),
        usage_at(TURN_2, 84_011, 83_846, 165, 42),
        window(TURN_2),
        turn_ended(TURN_2, /*completed*/ true),
    ]
}

#[test]
fn the_live_trace_folds_into_the_measured_totals() {
    let state = fold(&live_trace_records()).state().clone();

    assert_eq!(13, state.anchors.len());
    assert_eq!(42, state.snapshot.items.len());
    assert_eq!(
        (1..=42).collect::<Vec<u64>>(),
        state
            .snapshot
            .items
            .iter()
            .map(|item| item.seq)
            .collect::<Vec<_>>()
    );
    assert_eq!(Some(WINDOW), state.snapshot.window);
    assert_eq!(Some(84_011), state.snapshot.reported_context_tokens);
}

#[test]
fn the_big_read_is_priced_by_the_anchors_around_it() {
    let state = fold(&live_trace_records()).state().clone();

    // 41,448 bytes of tool output between anchors 34,729 and input 43,671: 8,942 tokens, alone in
    // its span and so exact.
    let big_read = &state.snapshot.items[22];
    assert_eq!(41_448, big_read.bytes);
    assert_eq!(TokenCost::Exact(8_942), big_read.cost);
}

#[test]
fn the_negative_turn_boundary_leaves_the_first_span_on_the_proxy() {
    let state = fold(&live_trace_records()).state().clone();

    // Turn 1 closes at 33,318 and turn 2 opens at input 33,113: the context shrank by 205. The
    // same-turn rule never sees that, because turn 2's first anchor has no previous same-turn
    // anchor, so its one input-kind item keeps the byte proxy.
    let opening_message = &state.snapshot.items[15];
    assert_eq!(395, opening_message.bytes);
    assert_eq!(Category::UserMessage, opening_message.category);
    assert_eq!(TokenCost::Estimated(byte_proxy(395)), opening_message.cost);
}

#[test]
fn both_completed_turns_carry_their_measured_span() {
    let state = fold(&live_trace_records()).state().clone();

    let turns = &state.snapshot.turns;
    assert_eq!(
        vec![1..=15, 16..=42],
        turns
            .iter()
            .map(|turn| turn.item_seq_range.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        vec![(None, Some(33_318)), (Some(33_318), Some(84_011))],
        turns
            .iter()
            .map(|turn| (turn.measured_before, turn.measured_after))
            .collect::<Vec<_>>()
    );
    // Turn 1 has no anchor before it, so its measured span is unknowable; turn 2's is 50,693.
    assert_eq!(
        vec![None, Some(50_693)],
        turns
            .iter()
            .map(TurnDelta::measured_added)
            .collect::<Vec<_>>()
    );
}

/// Findings §10.1: the interrupted turn, with the capture's own sizes and anchor.
fn interrupted_records() -> Vec<Record> {
    vec![
        Record::TurnStarted(TURN_1),
        item(TURN_1, Kind::ToolCall("call_spike_d"), 525),
        usage_at(TURN_1, 42_778, 42_648, 130, 1),
        item(TURN_1, Kind::ToolOutput("call_spike_d"), 291),
        item(TURN_1, Kind::AgentMessage, 526),
        turn_ended(TURN_1, /*completed*/ false),
        Record::TurnStarted(TURN_2),
        usage_at(TURN_2, 42_778, 42_778, 0, 3),
        item(TURN_2, Kind::ToolOutput("call_next_turn"), 291),
        usage_at(TURN_2, 43_078, 43_078, 0, 4),
        turn_ended(TURN_2, /*completed*/ true),
    ]
}

#[test]
fn an_interrupted_turn_strands_its_trailing_items_permanently() {
    let state = fold(&interrupted_records()).state().clone();

    // Only the call reached an anchor. The two items after it are stranded, and the next turn's
    // anchors price only their own span.
    assert_eq!(
        vec![
            TokenCost::Exact(130),
            TokenCost::Estimated(byte_proxy(291)),
            TokenCost::Estimated(byte_proxy(526)),
            TokenCost::Exact(300),
        ],
        costs(&state)
    );
    assert_eq!(None, state.snapshot.turns[0].measured_after);
    assert_eq!(None, state.snapshot.turns[0].measured_added());
}

#[test]
fn folding_the_live_trace_twice_yields_byte_identical_state() {
    let records = live_trace_records();
    let first = fold(&records);
    let second = fold(&records);

    let encode =
        |profiler: &ContextProfiler| serde_json::to_string(profiler.state()).expect("serializable");
    assert_eq!(encode(&first), encode(&second));
}

#[test]
fn the_derived_views_are_ordered_by_seq_not_by_iteration() {
    let state = fold(&live_trace_records()).state().clone();

    let first_members: Vec<u64> = state
        .snapshot
        .groups
        .iter()
        .map(|group| group.members[0])
        .collect();
    let mut ascending = first_members.clone();
    ascending.sort_unstable();
    assert_eq!(ascending, first_members);

    // Category declaration order, over the categories the capture contains.
    assert_eq!(
        vec![
            Category::UserMessage,
            Category::AgentMessage,
            Category::Reasoning,
            Category::ToolCall,
            Category::ToolOutput,
        ],
        state
            .snapshot
            .by_category
            .iter()
            .map(|(category, _)| *category)
            .collect::<Vec<_>>()
    );
}
