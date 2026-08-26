# Context Profiler - Findings Log

Date: 2026-08-26
Companion to: [design](./2026-08-26-context-profiler-design.md)

Everything discovered while designing the context profiler, and which decision each finding
changed. Recorded because most of these are non-obvious, several contradicted our starting
assumptions, and two were only settled by running real sessions.

Findings marked **[measured]** come from running the fork with a live model turn, not from reading
code.

---

## 1. Architecture

### 1.1 The TUI does not call `codex-core` directly

It speaks app-server v2 JSON-RPC, normally to an **in-process** app-server
(`tui/src/lib.rs:264`, `InProcessAppServerClient`), optionally remote (`:426`).

**Impact:** any profiler data must cross a real serialisation boundary. Ruled out reading
`ContextManager` directly and set the whole shape of the design.

### 1.2 `rawResponseItem/completed` already streams pre-history items, and the TUI throws them away

`RawResponseItemCompletedNotification { thread_id, turn_id, item: ResponseItem }`
(`app-server-protocol/src/protocol/v2/item.rs:1391`). Emitted from `send_raw_response_items`
(`core/src/session/mod.rs:3589`), called by `record_prepared_conversation_items` (`:3190`) - the
single funnel that also writes to `ContextManager` and persists the rollout.

The TUI receives and routes it (`tui/src/app/app_server_event_targets.rs:97`) then explicitly
discards it (`tui/src/chatwidget/protocol.rs:220`), with this comment at
`tui/src/app/thread_events.rs:154`:

> raw response items and realtime audio can carry large payloads, so cloning them into every
> thread's replay buffer only retains data the TUI cannot use.

**Impact:** the single biggest de-risk of the project - the feed exists and needs no core changes.
It also set the payload-discarding rule: summarise at ingest, drop the body.

> **Superseded in part by §8.** This is the **pre-truncation** feed. `ContextManager` shrinks large
> tool outputs *after* this broadcast, so raw item size is not active-context size.

### 1.3 The v2 item stream (what the transcript renders from) is lossy

`app-server-protocol/src/protocol/v2/thread.rs:1235`:

> The ThreadItems stored in each Turn are lossy since we explicitly do not persist all agent
> interactions, such as command executions.

**Impact:** avoided the obvious-looking mistake of attributing context from `thread/items/list`,
which would have silently under-reported.

### 1.4 `codex-analytics` is the house pattern for a fed reducer

Separate crate, consumes domain types, exposes `AnalyticsReducer::ingest`
(`analytics/src/reducer.rs:510`) and `track_*` methods. It never subscribes to anything.

**Impact:** settled "should the profiler subscribe?" - no, it is fed. Same dependency direction,
different control flow.

### 1.5 The TUI already defines its own measurement types

`tui/src/token_usage.rs:12` declares its own `pub struct TokenUsage` rather than reusing
`codex_protocol::protocol::TokenUsage`.

**Impact:** justified `UsageSnapshot` as a profiler-owned type instead of adopting a protocol DTO.

---

## 2. The append-only constraint

### 2.1 It is a wire protocol constraint, not just a cache

`core/src/client.rs:1256` `get_incremental_items` asserts the new history is element-wise
identical to the prefix the server already holds, then sends only the suffix via
`previous_response_id`. A mismatch returns `None` and forces a full resend.

**Impact:** reframed "no history rewrite" (`AGENTS.md:93`) from an optimisation into a hard
protocol property. The only expressible operations are *append a suffix* or *start over*. This is
the core reason context editing is deferred out of the MVP.

### 2.2 History items are not independent

`normalize.rs:227` `remove_corresponding_for` pairs FunctionCall/Output, ToolSearchCall/Output,
CustomToolCall/Output and LocalShellCall. Dropping half a pair sends the API an output for a call
it never saw.

**Impact:** `GroupKey` is in the data model from day one. The display unit is a call/output group,
never a transcript row.

### 2.3 Sanctioned rewrites exist, and are recorded as appends

`replace_annotated` (compaction, `compact.rs:320` / `compact_remote.rs:449`),
`drop_last_n_user_turns` (rollback), `remove_first_item` (window eviction) - all bump
`history_version`. Compaction appends `RolloutItem::Compacted { message, replacement_history }`;
rollback appends `EventMsg::ThreadRolledBack`.

**Impact:** established the checkpoint pattern for future context-management features, and the
"seal, don't reset" epoch design.

### 2.4 `estimate_item_token_count` is `pub(crate)` and coarse

`core/src/context_manager/history.rs:628-630`: "a coarse lower bound, not tokenizer-accurate".
Also, `estimate_token_count_with_base_instructions` (`:268`) counts base-instruction text plus
items but **not** tool schemas.

**Impact:** we reimplement the estimator, and even core does not have a complete baseline number.
Reinforced the decision to normalise against measured totals rather than chase estimator fidelity.

---

## 3. Gating

### 3.1 Both raw streams are gated by one flag

`app-server/src/request_processors/thread_lifecycle.rs:349`:

```rust
if matches!(&event.msg, EventMsg::RawResponseItem(_) | EventMsg::RawResponseCompleted(_))
    && !raw_events_enabled { continue; }
```

Driven by `ThreadStartParams.experimental_raw_events`
(`app-server-protocol/src/protocol/v2/thread.rs:156`, `#[experimental]`), which the TUI does not
set.

**Impact:** the flag is the feature's power switch, not a precision knob. Made Spike A the first
thing to run.

### 3.2 The connection-level gate is already satisfied

`InitializeCapabilities.experimental_api` (`v1.rs:47`) is set to `true` on both TUI connection
paths (`tui/src/lib.rs:433`, `:590`).

**Impact:** corrected an earlier claim that this was an additional obstacle. It is one field, not
two gates.

### 3.3 `ThreadResumeParams` has no `experimental_raw_events` field

It carries `#[experimental]` fields for `history` and `path`, but not raw events. The flag lives
on in-memory `ThreadState`, so it does not survive into a new app-server process.

**Impact:** a resumed thread cannot enable the stream at all. This, more than the replay
behaviour, is why M7 rollout hydration is the *only* route to profiling a resumed session - and
why fresh-sessions-first was the right MVP scope.

---

## 4. Rollout persistence

### 4.1 The rollout carries durable equivalents for all four profiler event classes

`rollout/src/policy.rs` persists `EventMsg::TokenCount`, `TurnStarted`, `TurnComplete`
unconditionally (`:107`), and `RolloutItem::Compacted` as an executive marker (`:16`).
`TokenCountEvent { info: Option<TokenUsageInfo> }` carries `last`, `total`, and
`model_context_window`.

**Impact:** hydrated sessions get **full reconciliation**, not degraded attribution. Confirmed
`ProfilerEvent` sits at the right abstraction level, since both sources normalise into it with no
new variants.

### 4.2 Response items are filtered, and exactly three are dropped

`should_persist_response_item` (`rollout/src/policy.rs:42`) returns `false` for
`AdditionalTools`, `CompactionTrigger` and `Other`.

**Impact:** turned the live/rollout equivalence test from a fuzzy "materially equivalent"
assertion into a strict one with an enumerated delta. In practice the delta is empty, because none
of those three appear on the live stream either (see 6.2).

### 4.3 Items carry their own stamped turn id

`ResponseItem::turn_id()` (`protocol/src/models.rs:1277`) reads
`internal_chat_message_metadata_passthrough.turn_id`, with `set_turn_id_if_missing` alongside.

**Impact:** the rollout adapter is smaller than expected - only `Compacted` needs replay position.
On the live path it gives a free integrity check against the notification's `turn_id`.

### 4.4 `thread/compacted` carries no discriminator

`ContextCompactedNotification { thread_id, turn_id }` (`v2/thread.rs:1986`). The richer
`CompactedItem` (with `replacement_history`, `window_id`, `previous_window_id`) exists only in the
rollout.

**Impact:** originally `CompactionKind { Remote, Unknown }`. **Superseded:** the design now drops
compaction kind entirely from `ProfilerEvent` for M1-M4. An enum whose value is almost always
`Unknown` earns nothing at the API boundary; the accumulator can infer `Remote` at M5 from an
observed `ResponseItem::Compaction`, which is positive evidence rather than a default. Also raised
the open question of whether M5 should read the rollout for compaction detail.

---

## 5. `BASELINE_TOKENS`, and a reasoning error worth recording

`tui/src/token_usage.rs:9`:

```rust
const BASELINE_TOKENS: i64 = 12000;

let effective_window = context_window - BASELINE_TOKENS;
let used = (tokens_in_context_window() - BASELINE_TOKENS).max(0);
let remaining = (effective_window - used).max(0);
```

Feeds the live indicator (`chatwidget.rs:1167`), status controls (`status_controls.rs:390`) and
the `/status` card (`status/card.rs:346`).

**The error:** we initially claimed a larger true baseline means Codex over-reports remaining
context. It is the opposite. The constant cancels in the numerator:
`remaining = (W − B) − (U − B) = W − U`, so `percent = (W − U) / (W − B)`. A larger baseline gives
a *larger* percentage remaining.

Worked example at W=272,000 and U=84,210: assuming 12,000 gives 72% remaining; a measured 25,300
gives 76%.

**Impact:** demoted "the status bar is lying about your headroom" from headline to footnote. The
distortion is modest at large windows but severe at small ones - the denominator goes from 20,000
to 6,700 at a 32k window, a 3x error. The verified headline became the baseline breakdown instead.

---

## 6. Spike A - live event stream **[measured]**

Method: `experimental_raw_events: true` plus a throwaway probe logging every `ServerNotification`,
one fresh thread, one tool-call turn. Both edits reverted afterwards.

### 6.1 The stream flows

Raw items arrived first try, in-process. The project's single point of failure held.

### 6.2 Ordering, and a simplification that fell out of it

Usage arrives **after** a response's own output items and **before** the tool output that becomes
the next request's input:

```
raw_item  CustomToolCall        670      ← resp_A output
raw_usage resp_A  input=25230 output=192 total=25422
raw_item  CustomToolCallOutput  17301    ← input to resp_B
raw_item  Reasoning             1593     ← resp_B output
raw_usage resp_B  input=29137 output=93  total=29230
```

`total_tokens = input + output` exactly (25,230+192=25,422; 29,137+93=29,230).

**Impact:** anchor on `total_tokens` and count every item seen. Removed the need for
"which items were outputs" bookkeeping that the original `items_seq` design implied.

### 6.3 Anchor density is per response

Four `raw_usage` and four `token_usage` events in a single turn.

**Impact:** the baseline/drift decomposition gets several data points per turn, so the
continuous-re-solve design has enough samples to work with.

### 6.4 Turn attribution is exact

Every `raw_item` had `stamped_turn` matching the notification's `turn_id`. Zero mismatches.

### 6.5 Bytes are a catastrophic token proxy for reasoning

One `Reasoning` item: **1,593 bytes** of JSON, `reasoning_output_tokens: 14`. The
`encrypted_content` blob dwarfs the tokens it represents - roughly 100x out.

**Impact:** the estimator must special-case `Reasoning`. This is exactly the class of error the
calibration test exists to catch, found before any estimator was written.

### 6.6 `total` vs `last`, confirmed

```
last=25422  total=25422
last=29230  total=54652
last=31639  total=86291
last=33143  total=119434     window=258400
```

`total` accumulates past the window; `last` tracks occupancy.

---

## 7. Spike B - the trace oracle **[measured]**

Method: `CODEX_ROLLOUT_TRACE_ROOT=/tmp/traces just codex`, then
`just codex debug trace-reduce <bundle>`.

### 7.1 Enabling is purely the env var

`ThreadTraceContext::start_root_or_disabled` (`rollout-trace/src/thread.rs:106`) reads only
`CODEX_ROLLOUT_TRACE_ROOT`, called from `Session::new` (`core/src/session/session.rs:978`) for
every non-subagent session.

Practical note: one session produced **three** bundles - the real one plus two auxiliary threads
the TUI spawns. `trace-reduce` takes a single bundle, so a `trace-*/` glob fails.

### 7.2 The full context of a fresh session

`payloads/4.json`, the first request, 7 input items:

| item | role | bytes | what |
|---|---|---|---|
| `additional_tools` | developer | 35,865 | `functions` 30,302 + `collaboration` 5,505 |
| `message` | developer | 18,086 | base system prompt |
| `message` | developer | 33,099 | `<skills_instructions>` |
| `message` | developer | 2,603 | multi-agent role |
| `message` | developer | 583 | `<multi_agent_mode>` |
| `message` | user | 25,506 | `AGENTS.md` |
| `message` | user | 358 | the actual prompt |

116,100 serialised bytes, `input_tokens: 25,230`. The prompt is **0.3% of the serialised bytes**.

**Impact:** the product thesis, supported. The largest **serialised-byte** contributors are skills
instructions and tool schemas, not `AGENTS.md` - an ordering nobody would guess, which is the
argument for building the tool. The equivalent claim about *tokens* is not yet established: §6.5
shows the byte-based estimator can be ~28× out for some item classes. Awaiting M2 calibration.

**This table also bounds the startup-context headline.** The entire first request is 25,230 input
tokens and it contains everything, so startup context (all of the above bar the user's prompt) is
necessarily **below 25,230**. Distributing proportionally by bytes gives ~11,720 hidden
(`additional_tools` + system prompt), ~13,430 observed instructions, ~78 prompt - summing to 25,230
by construction. That third independent route to ~11,700 is the strongest confirmation of the
baseline we have.

### 7.3 The residual is a nameable quantity

Spike A's live stream showed five messages (`33129, 2604, 583, 25542, 358`). Against Spike B's
seven items, the live stream is missing exactly `additional_tools` (35,865) and the base system
prompt (18,086) - 53,951 bytes, ~11,700 tokens. An independent back-of-envelope from Spike A's
arithmetic put the baseline at ~11,400.

Two independent routes agree: **the residual is exactly "tool schemas + base system prompt".**

`base_instructions` lives in `Prompt` and never passes through `record_conversation_items`, so it
emits no raw event - consistent with the code path.

**Impact:** validated the entire reconciliation model against ground truth before implementing it,
and gave the `/ctx` residual row an accurate label instead of a vague one.

### 7.4 The reduced trace is a usable oracle, with caveats

`state.json` holds `inference_calls` with `request_item_ids`, `response_item_ids` and exact
`usage`, plus `conversation_items` carrying `body`, `kind`, `role`, `call_id`.

The incremental ladder from one turn:

| # | req items | input_tokens | output | Δ input |
|---|---|---|---|---|
| 1 | 6 | 25,230 | 159 | - |
| 2 | 10 | 26,429 | 126 | +1,199 |
| 3 | 13 | 29,928 | 92 | +3,499 |
| 4 | 16 | 32,239 | 104 | +2,311 |
| 5 | 19 | 37,386 | 521 | +5,147 |

Caveats:

- **All requests are WebSocket `response.create`.** The first carries full `input`; the rest carry
  `previous_response_id` plus deltas. The raw payloads are therefore *not* window snapshots - the
  reduced `state.json` is the artifact.
- **No `instructions` or `tools` top-level keys** on the WS path. An earlier claim that the trace
  could itemise the baseline via those fields was wrong; the same information is present in a
  different shape (system prompt as a developer message, tool schemas as `additional_tools`).
- **`item_id` echoes the synthetic key** (`conversation_item:1`), so there is no join to real
  Responses item ids. Ordered comparison must use `(kind, role, call_id, body_len)`.
- **`request_item_ids` excludes `additional_tools`** - 6 ids for 7 input items. The reducer drops
  it, matching the rollout policy.
- **Cancelled and failed attempts carry no token usage**, so interrupted turns are blind spots.
- Bundles contain prompts, source, and absolute paths in plaintext
  (`rollout-trace/README.md:3-8`).

**Impact:** the oracle test is real and strict. Also drove the fixture-privacy design, and the
decision to capture calibration fixtures from a deliberately public session rather than scrubbing
a private one - scrubbing content while keeping the real content's token counts would compare
`estimate(synthetic)` against `tokens(real)`, which measures nothing.

---

## 8. The transformation in the middle - the raw stream is not the active context

Found during a deeper code review after the spikes, and the most consequential finding in this
document. It invalidates §1.2's claim that "attribution needs no heuristics".

### 8.1 Items are broadcast before they are truncated

`record_prepared_conversation_items` (`core/src/session/mod.rs:3190`):

```rust
let response_items = items.iter().map(|e| e.item.clone()).collect();  // clone FIRST
state.history.record_annotated_items(&items, …);                      // ← truncates here
self.persist_rollout_items(&rollout_items).await;                     // originals
self.send_raw_response_items(turn_context, &response_items)           // the untruncated clone
```

`record_annotated_items` → `record_items_with_metadata` → `process_item` (`history.rs:473`), which
applies `truncate_function_output_payload` to `FunctionCallOutput` and `CustomToolCallOutput` at
`policy * 1.2`. Default policy is `TruncationPolicyConfig::bytes(10_000)`
(`openai_models.rs:931`), so the effective cut is around **12,000 bytes**.

So the live raw stream and the rollout both carry the **pre-truncation** item, while
`ContextManager` - and therefore the model request - holds a smaller one.

**Impact:** naive attribution over-reports large tool outputs. From our own Spike A data, a
17,301-byte `CustomToolCallOutput` contributed 3,715 measured tokens, consistent with ~12,000 bytes
surviving - roughly a **45% over-report**. A 500KB test log would be off by ~40×. That is fatal to
"largest contributors", the feature most likely to drive user action.

### 8.2 The live/rollout equivalence test cannot detect it

Both sources derive from the same pre-truncation clone, so `assert_eq!(live, rollout)` passes while
both misstate effective context. Only the trace oracle - whose `conversation_items[].body` comes
from what was actually sent - can see the difference.

**Impact:** raised the trace oracle from "nice validation" to the only load-bearing correctness
check, and created Spike C.

### 8.3 The truncation is reproducible, not merely inferrable

`truncate_function_output_payload` (`history.rs:570`) is a thin wrapper over two functions that are
`pub` in a separate crate:

```rust
FunctionCallOutputBody::Text(content) => truncate_text(content, policy)
FunctionCallOutputBody::ContentItems(items) =>
    truncate_function_output_items_with_policy(items, policy, estimate_audio_token_count)
```

`codex-utils-output-truncation` depends only on `codex-protocol` and `codex-utils-string`, so
depending on it does not breach the profiler's layering rule.

**Impact:** the best mitigation is to apply Codex's own truncation rather than infer the effect
from usage anchors. Remaining unknowns for Spike C: whether the TUI can obtain the active model's
`TruncationPolicy`, and whether the audio path (needing core-private `estimate_audio_token_count`)
matters.

### 8.4 The event channel is small enough that lag is expected

```rust
// app-server-transport/src/transport/mod.rs:25
pub const CHANNEL_CAPACITY: usize = 128;
```

A single measured turn produced several hundred notifications. `AppServerEvent::Lagged { skipped }`
exists (`app-server-client/src/lib.rs:98`) and the TUI already performs resync work on it
(`tui/src/app/app_server_events.rs:62`).

**Impact:** dropped events would silently produce a plausible but false breakdown, which is worse
than showing nothing. Introduced `ProfilerEvent::StreamGap` and `AttributionCompleteness`, and
decided *not* to spike lag frequency - the handling has to exist regardless, and building it is
cheaper than measuring it. Also connects to raw-event scoping: raw items are large and share those
128 slots, so enabling them on every helper thread raises lag probability.

### 8.5 Compaction happens mid-turn, so turns and epochs are not 1:1

`CompactionPhase { StandaloneTurn, PreTurn, MidTurn }` (`analytics/src/facts.rs:436`), and
`run_auto_compact` is called at `session/turn.rs:473` - **inside** the tool loop that begins at
`:303`, not only via `run_pre_sampling_compact` before it.

**Impact:** a `TurnDelta` spanning a compaction would compute something like `68k − 180k = −112k`,
conflating "removed by compaction" with "added by the turn". Added `crossed_compaction: bool`, with
`measured_added()` returning `None` in that case. Also falsified the design's statement that a new
epoch begins between turns.

### 8.6 `TurnStatus` has three terminal values, and "aborted" is not one of them

`TurnStatus { Completed, Interrupted, Failed, InProgress }`
(`app-server-protocol/src/protocol/v2/turn.rs:31`). Core's internal vocabulary uses
`EventMsg::TurnAborted`, which is persisted (`rollout/src/policy.rs:110`), but that is not what the
live adapter receives.

**Impact:** `TurnOutcome { Completed, Interrupted, Failed }` rather than `{ Completed, Aborted }`.
Spike D records the core↔v2 mapping for the rollout adapter.

### 8.7 `Lagged` is connection-level and carries no thread id

`AppServerEvent::Lagged { skipped }` (`app-server-client/src/lib.rs:98`) identifies no thread.

**Impact:** `StreamGap` must be broadcast to every live profiler in the registry, marking all of
them `Incomplete`. There is no `thread_id` to derive, and attempting to attribute the gap to one
thread would leave the others silently wrong.

### 8.8 Tool schemas and instructions are not fixed for a session

`build_prompt` takes `tools` from `step_context.tool_router.model_visible_specs()`, and step
contexts are captured per step inside the tool loop (`session/turn.rs:303`). `build_skills_and_plugins`
injects skill instructions mid-session when a skill is mentioned.

**Impact:** "fixed overhead this session" was wrong. Replaced with an `InitialContextSummary`
captured at the first trustworthy inference - a snapshot claim ("Codex was already carrying ~25,150
tokens when you started") rather than an invariant. That figure is derived *from*
`first_request_input_tokens` rather than assembled from independent estimates, so it cannot exceed
what was actually sent.

## 9. Spike C - measured, from surviving Spike B artefacts **[measured]**

Both the Spike B trace bundle and its probe log survived on disk, so observed-vs-sent could be
compared for the **same session** without a new capture.

### 9.1 No truncation occurred, including at 24.5KB

`conversation_items[].body` in the reduced trace is what was *sent*; the probe log records what was
*observed* on the raw stream.

| observed (raw JSON bytes) | sent (text bytes) | Δ tokens | bytes/token |
|---|---|---|---|
| 4,792 | 4,244 | 1,040 | 4.08 |
| 15,876 | 15,186 | 3,373 | 4.50 |
| 14,152 | 13,495 | 2,219 | 6.08 |
| **24,567** | **23,284** | 5,043 | 4.62 |

Observed and sent differ by 4-5%, which is JSON envelope and escaping. **Nothing was truncated**,
including an output at roughly double the ~12,000-byte threshold derived from
`TruncationPolicyConfig::bytes(10_000) * 1.2`.

**Impact:** the transformation-in-the-middle concern (§8) is much smaller than feared, at least for
this configuration. It also retroactively supports §8's own correction box: the earlier 45%
over-report claim was wrong, and the direction it was wrong in is now confirmed.

**Caveats, stated deliberately.** One session, one model, one config. This does not show truncation
never fires - it shows it did not fire here at 24.5KB. The model's real `truncation_policy` may be
larger than the default we assumed, and these bodies were `ContentItems` (a `parts` array), which
take `truncate_function_output_items_with_policy` rather than `truncate_text`. A capture with a
200KB plain-`Text` output is still worth doing.

### 9.2 Anchor deltas give **exact** per-item token costs

Between every pair of consecutive anchors in that session, **exactly one item was added**. So its
true cost is arithmetic, not estimation:

```
tokens(item added between anchor n and n+1) = input_tokens(n+1) − total_tokens(n)
```

The four deltas above are measured token costs for four specific tool outputs.

**Impact - this is the most useful finding in the document.** It reframes attribution:

| item class | how we cost it |
|---|---|
| tool outputs (the large items, and the whole "largest contributors" view) | **exact**, from anchor deltas |
| model outputs - reasoning, messages, tool calls | **exact in aggregate** per response, from `output_tokens` |
| several items arriving between one pair of anchors | estimate, to apportion the measured delta |
| startup context | estimate, apportioning the measured first-request total |

It also **sidesteps truncation entirely**: we never need to know what Codex trimmed, because we
measure the difference across the boundary. `TruncationPolicy` being unreachable from the TUI
(§8.3) stops being a blocker.

**The new load-bearing assumption** is one item per delta. It held four times out of four here, but
parallel tool calls would break it. That is what the next capture must test, and it is now the
question M2's design hinges on.

## 10. Superseded decisions and corrections

The design spec states decisions; this section holds the reasoning for the ones that replaced an
earlier, wrong answer. Kept here so the spec stays implementable rather than argumentative.

### 10.1 Why the baseline is the first trustworthy anchor, not `min` over early anchors

The superseded rule was "baseline ≈ min over early anchors", reasoned as: drift grows with item
count, so the earliest anchor is least contaminated, so take the minimum.

That holds only when the estimator **under**-estimates. With `residual_n = baseline + n·ε` and
ε < 0, the residual *shrinks* with item count, so `min` selects the **latest** and most contaminated
anchor. The rule was correct half the time and maximally wrong the other half.

Over-estimation is the case to expect: a byte-based estimator over-counts reasoning items by
roughly 28× (§6.5). So the replacement is deterministic - the first anchor of epoch 0 in a `Live`
session with at least one item observed.

### 10.2 Why `source` and `window_id` cannot live in `ContextSnapshot`

An earlier draft placed `source: SnapshotSource` and `Epoch::window_id` inside the compared
snapshot while also specifying strict live-vs-rollout equality. Those are contradictory:
`Live != Hydrated` by definition, and `window_id` is `None` on the live path but populated from
`CompactedItem` on the rollout path. The test could never have passed.

The same defect recurred twice more before being caught structurally - via
`ContextSnapshot.completeness` (acquisition quality, not context) and via
`UsageSnapshot.response_id` reaching the snapshot through `Epoch::last_usage`. Hence the rule:
the compared type contains only what both sources can reproduce, enforced by construction rather
than by a normalisation method.

### 10.3 Corrections table

Recorded so they are not silently re-derived.

| Claim | Correction |
|---|---|
| A larger true baseline means Codex over-reports remaining context | Opposite. The constant cancels in the numerator; larger baseline gives a *larger* percentage remaining (§5) |
| The connection-level experimental gate is an additional obstacle | Already satisfied; `experimental_api: true` on both TUI paths (§3.2) |
| The trace exposes `instructions` and `tools` so the baseline can be itemised | Not on the WS path. Same data, different shape (§7.4) |
| The rollout has all four event kinds we need | Durable *equivalents*; response items are filtered (§4.2) |
| Rollout→ProfilerEvent conversion needs `current_turn` threading for items | Items are self-describing via `turn_id()`; only `Compacted` needs replay position (§4.3) |
| Item 1 (ordering) can only be settled by our own dump | The rollout trace answers it more directly (§7) |
| `conversation_items` is empty | A jq precedence error. It has 20 entries with `body` (§7.4) |
| Assert ordered item ids in the oracle test | No real ids exist in the reduced model; compare structural tuples (§7.4) |
| "Attribution needs no heuristics - the exact feed already exists" (§1.2) | The feed is **pre-truncation**. `ContextManager` shrinks large tool outputs after the broadcast, so attribution is a reconciled estimate (§8.1) |
| Oracle compares our items against `request_item_ids` | Different points in time. At an anchor we have already seen that response's outputs, so the identity is `request_item_ids ++ response_item_ids` (§7.4, §8) |
| `ContextSnapshot` carries `source` and `Epoch::window_id`, and snapshots compare strictly | Contradictory - `Live != Hydrated` by definition. Provenance moved to a third struct |
| "Fixed overhead this session" | Tools and instructions both grow mid-session; it is a startup snapshot (§8.5) |
| "Measured baseline" | Reconciled, not measured. Spike B identified the *cause*; the live quantity still comes through our estimator |
| "The prompt is 0.3% of it" | 0.3% of serialised **bytes** (358/116,100), stated next to a token figure. Per-item token share is unknown |
| `BASELINE_TOKENS` reproduced inside the profiler crate | It is TUI display policy; keeping it in the profiler leaks the layer boundary we drew |
| `min over early anchors` for the baseline | Wrong under over-estimation - `min` then picks the most contaminated anchor (§10.1) |
| "Startup context 33,100" quoted as the defensible claim | **Arithmetically impossible.** The whole first request was 25,230 input tokens and contained everything, so startup is necessarily below that. See the process note below (§7.2) |
| `ContextSnapshot.completeness` | Acquisition quality, not analytic state. A gap-affected live capture and an intact rollout replay describe the same context; moved to `ProfilerState` |
| `UsageSnapshot.response_id` | Leaked acquisition state back into the compared snapshot via `Epoch::last_usage`. Moved to `UsageAnchor` |
| `Compacted { kind }` on the event | Neither source has a discriminator; the accumulator infers it at M5 from observed `ResponseItem::Compaction` (§4.4) |
| Spike C reproduces "truncation" | `for_prompt` also synthesises missing outputs, drops orphans and substitutes media placeholders. Spike C covers the transformation, not one step of it (§8) |
| "Depends on `codex-protocol` ONLY" | Now also `codex-utils-output-truncation`. The rule is `codex-protocol` + model-agnostic utility crates; never core, TUI, or app-server protocol |
| "Every residual instance has a known cause" | Contradicts `ReconciliationDrift`, which exists precisely for remainder of unknown cause |

## Process note

The 33,100 error is worth recording as a process failure, not just a corrected number. It began as
an invented value in an illustrative mockup for a hypothetical session, then got quoted in prose as
the defensible claim about a real one, and survived a self-review pass.

**Rule adopted:** any figure appearing in prose must trace to a capture. Mockup numbers must be
visibly synthetic, and estimated values carry a tilde while measured ones do not.
