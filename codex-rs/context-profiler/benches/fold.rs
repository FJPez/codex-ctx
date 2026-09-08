//! Baseline cost of the profiler fold, before M5 changes what history the profiler retains.

use std::collections::HashMap;
use std::sync::LazyLock;

use codex_context_profiler::ContextProfiler;
use codex_context_profiler::ObservationStart;
use codex_context_profiler::ProfilerEvent;
use codex_context_profiler::UsageSnapshot;
use codex_context_profiler::serialized_size;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseItem;
use divan::Bencher;

const SIZES: [usize; 5] = [500, 2_000, 5_000, 10_000, 20_000];

/// Every response in the workload reports the same output total.
const OUTPUT_TOKENS: i64 = 150;
/// Mirrors the estimator's 4.64 bytes per token, so anchors land near the estimates they price.
const BYTES_PER_HUNDRED_TOKENS: i64 = 464;
/// A plausible startup context: tool schemas plus the system prompt.
const STARTUP_INPUT_TOKENS: i64 = 2_000;

fn main() {
    divan::main();
}

/// One `Item` fold on a fully priced history.
#[divan::bench(args = SIZES, sample_count = 50, sample_size = 1)]
fn item(bencher: Bencher, size: usize) {
    let prepared = prepared(size);
    bencher
        // Divan excludes `with_inputs` and the returned value's drop from the measured timing.
        .with_inputs(|| prepared.folded.clone())
        .bench_local_values(|mut profiler| {
            profiler.observe(ProfilerEvent::Item {
                turn_id: &prepared.probe_turn_id,
                item: &prepared.probe,
            });
            profiler
        });
}

/// One `Usage` fold closing a span of unpriced items, so both the output and input passes run.
#[divan::bench(args = SIZES, sample_count = 50, sample_size = 1)]
fn anchor(bencher: Bencher, size: usize) {
    let prepared = prepared(size);
    bencher
        .with_inputs(|| prepared.unpriced.clone())
        .bench_local_values(|mut profiler| {
            profiler.observe(ProfilerEvent::Usage {
                turn_id: &prepared.unpriced_turn_id,
                usage: prepared.closing.clone(),
            });
            profiler
        });
}

/// The whole session folded from scratch.
#[divan::bench(args = SIZES, sample_count = 3, sample_size = 1)]
fn ingest(bencher: Bencher, size: usize) {
    let workload = &prepared(size).workload;
    bencher.bench_local(|| {
        let mut profiler = ContextProfiler::new(ObservationStart::SessionStart);
        for step in &workload.steps {
            profiler.observe(workload.event(step));
        }
        profiler
    });
}

/// One folded event, owning what `ProfilerEvent` only borrows.
enum Step {
    TurnStarted { turn: usize },
    Item { turn: usize, item: usize },
    Usage { turn: usize, usage: UsageSnapshot },
    TurnEnded { turn: usize },
}

/// A synthetic session shaped like the recorded traces: three items per anchor, three to four
/// anchors per turn.
struct Workload {
    turn_ids: Vec<String>,
    items: Vec<ResponseItem>,
    steps: Vec<Step>,
    clock: AnchorClock,
}

impl Workload {
    fn event<'a>(&'a self, step: &'a Step) -> ProfilerEvent<'a> {
        match step {
            Step::TurnStarted { turn } => ProfilerEvent::TurnStarted {
                turn_id: &self.turn_ids[*turn],
            },
            Step::Item { turn, item } => ProfilerEvent::Item {
                turn_id: &self.turn_ids[*turn],
                item: &self.items[*item],
            },
            Step::Usage { turn, usage } => ProfilerEvent::Usage {
                turn_id: &self.turn_ids[*turn],
                usage: usage.clone(),
            },
            Step::TurnEnded { turn } => ProfilerEvent::TurnEnded {
                turn_id: &self.turn_ids[*turn],
                completed: true,
            },
        }
    }
}

/// Walks the reported totals forward: last response's output becomes this request's input, so the
/// input delta across a span is the span's own estimated size.
#[derive(Clone, Copy)]
struct AnchorClock {
    input_tokens: i64,
    previous_output_tokens: i64,
}

impl AnchorClock {
    fn anchor(&mut self, span_bytes: usize, items_seq: u64) -> UsageSnapshot {
        let span_tokens = span_bytes as i64 * 100 / BYTES_PER_HUNDRED_TOKENS;
        self.input_tokens += span_tokens + self.previous_output_tokens;
        self.previous_output_tokens = OUTPUT_TOKENS;
        UsageSnapshot {
            reported_context_tokens: self.input_tokens + OUTPUT_TOKENS,
            input_tokens: self.input_tokens,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: OUTPUT_TOKENS,
            reasoning_output_tokens: 0,
            items_seq,
        }
    }
}

/// Everything the three cases need, built once per size and outside every timed region.
struct Prepared {
    workload: Workload,
    /// The whole workload folded; its last accepted anchor priced every item.
    folded: ContextProfiler,
    probe_turn_id: String,
    probe: ResponseItem,
    /// `folded` plus a same-turn anchor and one unpriced response after it.
    unpriced: ContextProfiler,
    unpriced_turn_id: String,
    closing: UsageSnapshot,
}

static PREPARED: LazyLock<HashMap<usize, Prepared>> =
    LazyLock::new(|| SIZES.iter().map(|&size| (size, prepare(size))).collect());

fn prepared(size: usize) -> &'static Prepared {
    #[allow(clippy::expect_used)]
    PREPARED.get(&size).expect("every size is prepared")
}

fn prepare(size: usize) -> Prepared {
    let workload = build_workload(size);
    let mut folded = ContextProfiler::new(ObservationStart::SessionStart);
    for step in &workload.steps {
        folded.observe(workload.event(step));
    }

    let unpriced_turn_id = "turn-unpriced".to_string();
    let mut unpriced = folded.clone();
    let mut clock = workload.clock;
    let mut items_seq = workload.items.len() as u64;
    unpriced.observe(ProfilerEvent::TurnStarted {
        turn_id: &unpriced_turn_id,
    });
    // A first span so the timed anchor has a same-turn predecessor to take an input delta from.
    let priced = response_items(workload.items.len() + 8);
    let mut span_bytes = 0;
    for item in &priced {
        span_bytes += item_bytes(item);
        items_seq += 1;
        unpriced.observe(ProfilerEvent::Item {
            turn_id: &unpriced_turn_id,
            item,
        });
    }
    unpriced.observe(ProfilerEvent::Usage {
        turn_id: &unpriced_turn_id,
        usage: clock.anchor(span_bytes, items_seq),
    });
    let trailing = response_items(workload.items.len() + 16);
    let mut span_bytes = 0;
    for item in &trailing {
        span_bytes += item_bytes(item);
        items_seq += 1;
        unpriced.observe(ProfilerEvent::Item {
            turn_id: &unpriced_turn_id,
            item,
        });
    }
    let closing = clock.anchor(span_bytes, items_seq);

    Prepared {
        probe_turn_id: "turn-probe".to_string(),
        probe: function_call_output(workload.items.len() + 24, 1_024),
        folded,
        unpriced,
        unpriced_turn_id,
        closing,
        workload,
    }
}

fn build_workload(size: usize) -> Workload {
    let mut workload = Workload {
        turn_ids: Vec::new(),
        items: Vec::new(),
        steps: Vec::new(),
        clock: AnchorClock {
            input_tokens: STARTUP_INPUT_TOKENS,
            previous_output_tokens: 0,
        },
    };
    while workload.items.len() < size {
        let turn = workload.turn_ids.len();
        workload.turn_ids.push(format!("turn-{turn}"));
        workload.steps.push(Step::TurnStarted { turn });
        let responses = turn_responses(turn, workload.items.len());
        for response in responses {
            push_response(&mut workload, turn, response);
            if workload.items.len() >= size {
                break;
            }
        }
        workload.steps.push(Step::TurnEnded { turn });
    }
    workload
}

/// The responses of one turn: three, plus a reminder response on every fourth turn.
fn turn_responses(turn: usize, first_call: usize) -> Vec<Vec<ResponseItem>> {
    let output_bytes = 1_024 + (turn % 3) * 1_024;
    let mut responses = vec![
        vec![
            user_message(&"u".repeat(120)),
            reasoning(),
            function_call(first_call),
        ],
        vec![
            function_call_output(first_call, output_bytes),
            reasoning(),
            assistant_message(&"a".repeat(300)),
        ],
        vec![
            function_call(first_call + 3),
            function_call_output(first_call + 3, 600),
        ],
    ];
    if turn % 4 == 3 {
        responses.push(vec![instruction_message(&"i".repeat(200))]);
    }
    responses
}

fn push_response(workload: &mut Workload, turn: usize, response: Vec<ResponseItem>) {
    let mut span_bytes = 0;
    for item in response {
        span_bytes += item_bytes(&item);
        workload.items.push(item);
        workload.steps.push(Step::Item {
            turn,
            item: workload.items.len() - 1,
        });
    }
    let items_seq = workload.items.len() as u64;
    let usage = workload.clock.anchor(span_bytes, items_seq);
    workload.steps.push(Step::Usage { turn, usage });
}

/// The items of one response, used to extend a prepared profiler past its workload.
fn response_items(first_call: usize) -> Vec<ResponseItem> {
    vec![
        function_call_output(first_call, 1_024),
        reasoning(),
        assistant_message(&"a".repeat(300)),
    ]
}

fn item_bytes(item: &ResponseItem) -> usize {
    serialized_size(item).unwrap_or(0)
}

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

fn instruction_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: kinds("current_time.reminder"),
    }
}

/// Core stamps `unknown` on its own outputs, so live agent messages carry no usable kind.
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

fn reasoning() -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: None,
        encrypted_content: Some("r".repeat(150)),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call(index: usize) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "exec".to_string(),
        namespace: None,
        arguments: format!("{{\"command\":\"{}\"}}", "c".repeat(180)),
        encrypted_function_args: None,
        call_id: format!("call-{index}"),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call_output(index: usize, bytes: usize) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(format!("call-{index}")),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("o".repeat(bytes)),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}
