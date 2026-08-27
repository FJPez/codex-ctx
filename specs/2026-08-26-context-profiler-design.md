# Context Profiler (`/ctx`) - MVP Design

Status: **M1 complete; M2 is next. Architecture approved, correctness questions answered.**
Date: 2026-08-26
Companion: [findings](./2026-08-26-context-profiler-findings.md) - the empirical evidence behind these decisions

> **Read this first.** The raw event stream is *not* the active model context. Items are cloned
> and broadcast **before** `ContextManager` truncates them, so a large tool output can appear on the
> wire at its full size while a much smaller version reaches the model. See
> [The transformation in the middle](#the-transformation-in-the-middle).
>
> We do not reproduce that transformation. We **measure across it**: the token cost of an item is
> the difference between consecutive usage anchors. See
> [Attribution by anchor delta](#attribution-by-anchor-delta).

## Problem

Codex shows a context-window percentage and nothing else. A user cannot see what is consuming
their window, what each prompt added, or what compaction removed. The percentage itself is
computed by subtracting a hardcoded 12,000-token constant (`tui/src/token_usage.rs:9`) from a
figure the user cannot decompose.

## Goal

A read-only context profiler built into the Codex TUI, reachable as `/ctx`, that breaks the
current context window down into attributable parts and reconciles that breakdown against the
real measured token usage.

Verified example of what this makes visible - the entire context of a fresh session before the
user's prompt has any effect:

| item | role | bytes | what |
|---|---|---|---|
| `additional_tools` | developer | 35,865 | tool schemas (`functions` 30,302 + `collaboration` 5,505) |
| `message` | developer | 18,086 | base system prompt |
| `message` | developer | 33,099 | `<skills_instructions>` |
| `message` | developer | 2,603 | multi-agent role |
| `message` | developer | 583 | `<multi_agent_mode>` |
| `message` | user | 25,506 | `AGENTS.md` |
| `message` | user | 358 | the user's actual prompt |

116,100 serialised bytes, `input_tokens: 25,230`. The prompt is **0.3% of the serialised bytes**
(358 / 116,100) - we have no per-item token count, so the share of *tokens* is unknown.

The largest **serialised-byte** contributors in this capture were skills instructions and tool
schemas, not `AGENTS.md`, which is what most people would guess. That ordering is not yet
established for *tokens*: our own measurement of a reasoning item (1,593 bytes → 398 estimated vs 14 actual) shows
bytes and tokens can diverge by ~28× after the estimator's 4-bytes/token conversion. The stronger token-consumption claim waits on M2
calibration.

## Non-goals for the MVP

- No context management: no archiving, pinning, editing, or restoring. Those are gated behind the
  append-only constraint documented in `CLAUDE.md` and are deliberately deferred until profiling
  has been dogfooded.
- No tokenizer. The design normalises estimates against measured totals precisely so that
  tokenizer accuracy is not a product guarantee.
- No changes to `codex-core`.

## The transformation in the middle

The observability picture, stated accurately:

```
        hidden from us: tool schemas + base system prompt
                              │
  raw ResponseItems ──────┐   │
  (pre-truncation)        │   │
                          ▼   ▼
                     ContextManager
                  truncate + normalize        ← we cannot see this
                          │
                          ▼
                    model request
                          │
                          ▼
                 authoritative usage          ← we can see this
```

`record_prepared_conversation_items` (`core/src/session/mod.rs:3190`) clones the items **before**
recording them, then broadcasts that clone:

```rust
let response_items = items.iter().map(|e| e.item.clone()).collect();  // clone first
state.history.record_annotated_items(&items, …);                      // ← truncates here
self.persist_rollout_items(&rollout_items).await;                     // originals
self.send_raw_response_items(turn_context, &response_items)           // the untruncated clone
```

`process_item` (`history.rs:473`) truncates `FunctionCallOutput` and `CustomToolCallOutput` at
`policy * 1.2`, with a default policy of `bytes(10_000)` (`openai_models.rs:931`) - an effective
cut around **12,000 bytes**.

Consequences:

- **Naive attribution may over-report large tool outputs.** How much is **unmeasured**. See the
  box below - an earlier draft claimed 45% from Spike A data, and the arithmetic does not support
  it.
- **The live/rollout equivalence test cannot catch this.** Both sources are taken from the same
  pre-truncation clone, so they agree with each other while both misstate effective context. Only
  the trace oracle sees the difference.

> **The magnitude is unknown, and an earlier draft got this wrong.**
>
> Spike A showed a 17,301-byte `CustomToolCallOutput` contributing 3,715 measured tokens
> (29,137 − 25,422). That was written up as "consistent with ~12,000 bytes surviving, roughly a 45%
> over-report". Checking it:
>
> ```
> if the item survived intact:  17,301 / 3,715 = 4.66 bytes/token
> if truncated to ~12,000:      12,000 / 3,715 = 3.23 bytes/token
> session density (Spike B):   116,100 / 25,230 = 4.60 bytes/token
> ```
>
> 4.66 sits almost exactly on the session's own density. The evidence leans toward **no truncation
> having occurred** on that item, the opposite of what was claimed.
>
> **Spike C has since confirmed the correction directly.** Comparing observed against sent for a
> whole session, nothing was truncated - including a 24,567-byte output. Observed and sent differ
> by 4-5%, which is JSON escaping (findings §9.1). The truncation code path is verified real; its
> impact in practice was not what this section originally claimed.

**Truncation is not the whole transformation.** `for_prompt` runs `normalize_history` *after*
`process_item` has truncated, and that pass also: synthesises `"aborted"` outputs for calls that
have none (`ensure_call_outputs_present`), drops orphan outputs (`remove_orphan_outputs`), and
substitutes placeholders for unsupported images and audio (`strip_images_when_unsupported`,
`strip_audio_when_unsupported`). Reproducing truncation alone would leave four transformations
unmodelled.

**Proposed mitigation, with a verified obstacle.** `truncate_function_output_payload` is a thin
wrapper over `truncate_text` and `truncate_function_output_items_with_policy`, both `pub` in
**`codex-utils-output-truncation`** (which depends only on `codex-protocol` and
`codex-utils-string`). So the *algorithm* is reusable.

The *policy* is not. `truncation_policy` has **zero non-test occurrences** in `tui/src`,
`app-server-protocol/src`, or `app-server/src` - the only hit is a test fixture
(`tui/src/chatwidget/tests/helpers.rs:285`). The TUI consumes `ModelPreset`, not `ModelInfo`, so
the active model's policy never crosses the app-server boundary. Reproducing truncation faithfully
would need a protocol change, which the MVP rules out.

**Resolved by Spike C - we sidestep it.** See [Attribution by anchor delta](#attribution-by-anchor-delta)
below. We never need the policy, because we measure the difference across an anchor boundary rather
than reproducing what Codex trimmed.

Spike C found **no `ContextManager` truncation at all** across three captures, including on a
24,567-byte tool output - roughly double the derived threshold.

**The layer that actually fires is the tool's, and it declares itself.** A `cat` of a 373KB file
never became a 373KB item; the body's own text reads
`Warning: truncated output (original token count: 103200)`, capped to 40,151 chars before the item
existed. That is upstream of the raw stream, so both the live feed and the trace already carry the
capped content - nothing to reproduce. It also hands `/ctx` a feature for free: *"this output was
cut from 103,200 tokens"*, read straight from the warning. Findings §10.2.

Media items are explicitly **unsupported** in the MVP and marked as such rather than silently
mis-attributed.

The spec still says **pre-history `ResponseItem` feed**, never "exact model-visible feed", because
the clone-before-truncate mechanism is real regardless of how often it bites.

## Architecture

```
AppServerClient (owned by App)
   │  ServerNotification
   ▼
tui/src/context_profiler/mod.rs        adapter: unwrap + route by thread_id
   │  ProfilerEvent
   ▼
codex-context-profiler                 attribution + reconciliation
   │  ContextSnapshot
   ▼
tui/src/context_profiler/view.rs       rendering only, no arithmetic
```

```
codex-rs/context-profiler/             new crate; codex-protocol + utility crates only
  src/lib.rs
  src/model.rs
  src/accumulator.rs
  src/classify.rs
  src/reconcile.rs
  src/*_tests.rs

codex-rs/tui/src/context_profiler/
  mod.rs                               ServerNotification -> ProfilerEvent
  view.rs                              ContextSnapshot -> ratatui
```

### Decisions

**The profiler is a fed fold, not a subscriber.** The app-server connection is singular and owned
by `App`; thread routing already exists (`tui/src/app/app_server_event_targets.rs:97`). A
subscribing profiler would need a second connection or an inverted dependency. Precedent:
`codex-analytics` exposes `AnalyticsReducer::ingest` and is fed by callers
(`analytics/src/reducer.rs:510`).

**Layering rule.** `codex-context-profiler` may depend on `codex-protocol` and model-agnostic
utility crates (`codex-utils-output-truncation`, `codex-utils-string`). It must **not** depend on
`codex-core`, `codex-tui`, or `codex-app-server-protocol`.

The argument is semantic layer, not dependency weight: `ResponseItem` is a model-context concept,
`RawResponseItemCompletedNotification` is a transport concept. This also means the live path and
the rollout path converge on the same ingest API - `RolloutItem::ResponseItem(envelope)` and
`RawResponseItemCompletedNotification { item }` both carry `codex_protocol::models::ResponseItem`.

**Two adapters, one accumulator.** Live and rollout adapters both stop at `ProfilerEvent`.

**Payload-bounded, not memory-bounded.** Item payloads are summarised at ingest and discarded, so
memory does not scale with tool-output size - which is what matters, since the TUI already excludes
raw items from its replay buffer for exactly that reason (`tui/src/app/thread_events.rs:154`). But
we do retain every `ItemSummary`, `TurnDelta` and `UsageSnapshot`, so memory remains
O(events). Fine for the MVP because summaries are tiny; capping sealed-epoch detail and anchor
history later needs no change to the public model.

**Feature-flagged.** `rawResponseItem/*` is internal-only and listed in `AGENTS.md:106` as a
breaking-change surface.

### Required change outside the new crate

`ThreadStartParams` must set `experimental_raw_events` for **profiled interactive threads only**.
Without it the app-server drops both raw streams
(`app-server/src/request_processors/thread_lifecycle.rs:349`).

**Do not set it unconditionally in `thread_start_params_from_config`.** That helper has three
non-test call sites (`tui/src/app_server_session.rs:436`, `:754`, `:1580`) and is used for helper
threads as well as the primary session. Raw items are large and share the 128-slot event channel,
so enabling them everywhere directly raises lag probability - the same failure `Invalidated` exists
to handle. M1 threads a scoped flag through the relevant call site.

The connection-level gate is already satisfied - the TUI sets `experimental_api: true` on both
connection paths (`tui/src/lib.rs:433`, `:590`).

`ThreadResumeParams` has no such field, so resumed threads cannot enable the stream at all. See
[M7 is harder than "hydrate once"](#m7-is-harder-than-hydrate-once).

## Data model

```rust
pub enum ProfilerEvent<'a> {
    TurnStarted { turn_id: &'a str },
    Item        { turn_id: &'a str, item: &'a ResponseItem },
    Usage       { turn_id: &'a str, usage: UsageSnapshot },
    TurnEnded   { turn_id: &'a str, completed: bool },
    /// Attribution can no longer be trusted from here on.
    Invalidated { reason: InvalidationReason },
}

pub enum InvalidationReason {
    /// The app-server event consumer lagged; `AppServerEvent::Lagged { skipped: usize }`.
    EventsDropped { skipped: usize },
    /// `thread/compacted`. History was rewritten out from under us.
    Compacted,
}
```

**`Invalidated` collapses three earlier concepts into one.** A previous draft had a separate
`StreamGap` event, a `Compacted` event, epoch sealing, and an `AttributionCompleteness` enum.
For the MVP all four reduce to the same product behaviour: *we can no longer account for what is
in the window, so say so instead of showing a plausible number.*

That is the honest MVP answer to compaction as well as to dropped events. Proper epoch handling -
sealing the pre-compaction state so it can be inspected and compared - is what **M5** is for. Until
then a compaction ends attribution for the session, which is a real limitation and a visible one.

`completed: bool` rather than a three-variant outcome: nothing in the MVP branches on
*why* a turn didn't complete, only on whether its measured delta is trustworthy. The naming point
that drove the earlier enum (don't adopt core's "aborted" vocabulary when the wire says
`Interrupted`/`Failed`) is satisfied by not naming it at all.

Variants mirror the v2 wire semantics we actually receive - `TurnStatus { Completed, Interrupted,
Failed, InProgress }` (`app-server-protocol/src/protocol/v2/turn.rs:31`) - rather than core's
internal "aborted" vocabulary. The rollout adapter needs an end-condition regardless, since
`EventMsg::TurnAborted` is persisted alongside `TurnComplete` (`rollout/src/policy.rs:110`); Spike D
records the exact mapping between the two. A non-`Completed` turn renders `—` for its measured
delta, never a number.

**`Lagged` is connection-level and cannot be routed to one thread.**
`AppServerEvent::Lagged { skipped }` carries no `thread_id`, so the adapter must broadcast it:

```rust
for profiler in registry.live_profilers_mut() {
    profiler.observe(ProfilerEvent::Invalidated { reason: EventsDropped { skipped } });
}
```

Every live profile is invalidated (`invalidated = Some(EventsDropped { .. })`), because we cannot
know which thread lost events. Do not
attempt to derive a `thread_id` for this event - there isn't one.

Dropped events are not a hypothetical. The channel is `CHANNEL_CAPACITY = 128`
(`app-server-transport/src/transport/mod.rs:25`) and a single measured turn produced several
hundred notifications; `AppServerEvent::Lagged { skipped }` exists and the TUI already handles it
(`tui/src/app/app_server_events.rs:62`).

**`Lagged` is connection-level and carries no `thread_id`,** so the adapter broadcasts:

```rust
for profiler in registry.values_mut() {
    profiler.observe(ProfilerEvent::Invalidated {
        reason: InvalidationReason::EventsDropped { skipped },
    });
}
```

Every live profile is invalidated, because we cannot know which thread lost events. Do not try to
derive a `thread_id` - there isn't one.

Single `observe(ProfilerEvent<'_>)` entry point rather than discrete methods, per
`AGENTS.md:89` (small API surface) and `:21` (exhaustive matches - adding a variant produces a
compile error at the adapter rather than a silently unhandled case).

`turn_id` is on every variant so an item arriving outside a turn is attributable rather than
silently misfiled.

```rust
/// One usage anchor. Numbers only - no `response_id`, which nothing in the MVP
/// renders or joins against, and which the rollout path cannot reproduce.
///
/// The six token fields duplicate `codex_protocol::protocol::TokenUsage`. That is
/// deliberate: the rename of `total_tokens` to `reported_context_tokens` is the
/// point, because `ThreadTokenUsage` carries both a cumulative `total` and an
/// occupancy `last` and confusing them produces plausible nonsense.
pub struct UsageSnapshot {
    pub reported_context_tokens: i64,   // from `last`, NEVER `total`
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub model_context_window: Option<i64>,
    pub items_seq: u64,                 // items observed when this anchor arrived
}
```

A previous draft split this into `UsageSnapshot` + `UsageAnchor`. The split existed only to hold
`response_id`, which had no consumer. Merged.

### Which stream produces `Usage` (exactly one, per path)

Measured: a single turn emitted **four** `rawResponse/completed` and **four**
`thread/tokenUsage/updated` events carrying the *same* value - `token_usage.last` equals
`raw_usage.total_tokens` (25,422 / 29,230 / 31,639 / 33,143). Feeding both into `observe` would
double-count every anchor.

| path | anchor source | role |
|---|---|---|
| Live | `rawResponse/completed` | **primary anchor** - arrives first, carries `response_id` and the input/output split |
| Live | `thread/tokenUsage/updated` | supplies `model_context_window`; anchor **only** as fallback when raw events are unavailable |
| Rollout | `EventMsg::TokenCount` | anchor (the only usage record persisted) |

**The merge needs an explicit mechanism.** Raw usage arrives first and carries no window; the
window arrives later on `thread/tokenUsage/updated`. A folded event cannot be amended
retroactively, so the adapter buffers:

```
RawResponseCompleted{ usage: Some(u) }  →  pending[turn_id] = u
ThreadTokenUsageUpdated                 →  if last.total_tokens == pending.reported
                                              emit one merged Usage
                                           else
                                              emit pending unmerged, window = None
flush on TurnEnded                      →  emit any unpaired pending
```

`RawResponseCompletedNotification.usage` is `Option<…>`. When it is `None` — cancelled or failed
attempts — **emit no anchor** and increment `missing_usage_count` in diagnostics. An anchor with a
guessed value would corrupt every subsequent residual. Covered by a Spike D test.

The fallback path is narrow: if raw events are off there are no items either, so `/ctx` is in its
degraded state showing Codex's own number.

Live and rollout therefore anchor on *different* streams. That the values agree is not assumed;
it is what the live/rollout equivalence test checks.

The field is deliberately not named `total_tokens`. `ThreadTokenUsage` carries both `total` and
`last` with opposite meanings (`tui/src/token_usage.rs:37`): `total` accumulates across the
session and exceeds the window on any long thread; `last` tracks occupancy.

### No epochs in the MVP

A previous draft carried `Epoch`, `EpochSummary`, `EpochStartReason`, sealing semantics, a
live-epoch-only aggregation invariant, and `TurnDelta::crossed_compaction`. In M1-M4 there is
**exactly one epoch, always**, because compaction invalidates the profile and resume is M7. All of
it was dead code until M5.

M5 reintroduces epochs properly - sealing the pre-compaction state so before/after can be compared,
which is M5's entire product value. Doing it now buys nothing and costs an invariant
(`ContextSnapshot` aggregates must use only the live epoch) that is easy to violate and impossible
to test with one epoch.

### Residual: two numbers, not four buckets

```rust
// on ContextSnapshot:
pub baseline_tokens: Option<i64>,   // reconciled: tool schemas + base system prompt
pub drift_tokens: i64,              // remainder of unknown cause
```

The earlier `ResidualBucket { reason: ResidualReason, .. }` had four variants of which two -
`CarriedOver` (needs compaction) and `PriorHistory` (needs resume) - are unreachable before M5/M7.
Two named numbers say the same thing.

`drift_tokens` is deliberately *not* called estimator error. A growing remainder can mean estimator
error, an unobserved token-bearing item, or new Codex behaviour; we know at least one such class
exists (`additional_tools`, never on any stream we read). Naming it after one hypothesis would
misdirect whoever debugs it.

`SessionBaseline` was identified empirically (see findings) as *tool schemas plus base system
prompt*, reconciled at ~11,700 tokens in a default configuration.

**`SessionBaseline` is frozen to the initial hidden baseline.** Tool schemas can change mid-session
(see the view section), and those changes are invisible to us, so their cost lands in
`ReconciliationDrift`. The MVP does not attempt to relabel it. This matters for how the row is
read: it is *startup hidden baseline*, not current system-and-tools cost.

```rust
pub enum Category {
    UserMessage,    // not "UserPrompt" - we classify by failing to match known tags,
                    // which is negative inference, not proof of human provenance
    AgentMessage,
    Reasoning,
    ToolCall,       // FunctionCall | LocalShellCall | CustomToolCall
                    // | ToolSearchCall | WebSearchCall | ImageGenerationCall
    ToolOutput,     // FunctionCallOutput | CustomToolCallOutput | ToolSearchOutput
    Instructions,   // injected contextual fragments
    Compaction,     // ResponseItem::Compaction | ContextCompaction
    Other,
}

pub enum GroupKey {
    ToolCall(String),   // call_id: pairs a call with its output
    Ungrouped(u64),     // seq: the item is its own group
}

pub struct ItemSummary {
    pub seq: u64,
    pub turn_index: u32,
    pub category: Category,
    pub estimated_tokens: i64,
    pub label: String,          // capped
    pub group: GroupKey,
    pub item_id: Option<String>,
}

/// One or more items that must be presented as a unit - typically a tool call
/// with its output. This is the display unit for "largest contributors",
/// never a bare `ItemSummary`.
pub struct ItemGroup {
    pub key: GroupKey,
    pub category: Category,     // the group's dominant category
    pub estimated_tokens: i64,  // sum across members
    pub label: String,
    pub members: Vec<u64>,      // ItemSummary::seq values
}
```

Grouping is total - every item belongs to exactly one group - so the view never handles an
`Option` and cannot render a half-pair. This matters because `normalize.rs:227`
(`remove_corresponding_for`) shows call/output pairing is a hard invariant in core.

**Do not scope `GroupKey::ToolCall` by turn.** Core pairs globally by `call_id`
(`remove_corresponding_for`, `ensure_call_outputs_present`, `remove_orphan_outputs`), and an
interrupted turn whose output lands in the next turn would fail to pair under turn scoping - the
exact orphan this grouping prevents. For collision safety add a diagnostics counter, not a
composite key.

`Message { role: "user" }` splits into `UserMessage` vs `Instructions` using the public
`*_OPEN_TAG` constants in `codex_protocol::protocol` and `parse_hook_prompt_fragment`
(`protocol/src/items.rs:655`). Core's authoritative matcher list is `pub(crate)`
(`core/src/context/contextual_user_message.rs`), so our classification will drift. Mitigation: a
tagged-but-unrecognised user message lands in `Instructions` **and** increments a counter surfaced
in `/ctx`, turning a silent accuracy bug into a visible signal during dogfooding.

```rust
pub struct TurnDelta {
    pub turn_id: String,
    pub index: u32,
    /// Global `ItemSummary::seq` range.
    pub item_seq_range: RangeInclusive<u64>,
    pub estimated_added: i64,
    /// The last anchor from BEFORE this turn started, captured on `TurnStarted`.
    /// `None` for the first turn of a session, where no prior anchor exists.
    pub measured_before: Option<i64>,
    /// The last anchor observed during this turn.
    pub measured_after: Option<i64>,
}

impl TurnDelta {
    pub fn measured_added(&self) -> Option<i64> {
        self.measured_before
            .zip(self.measured_after)
            .map(|(before, after)| after - before)
    }
}
```

**Compaction can happen mid-turn**, not only between turns: `CompactionPhase { StandaloneTurn,
PreTurn, MidTurn }` (`analytics/src/facts.rs:436`), with `run_auto_compact` called at `turn.rs:473`
inside the loop that begins at `:303`. A turn spanning one would compute e.g. `68k − 180k = −112k`,
conflating "removed by compaction" with "added by the turn".

In the MVP that case cannot arise, because compaction invalidates the whole profile
(`InvalidationReason::Compacted`) and no further deltas are shown. M5, which reintroduces epochs,
must handle turns that span an epoch boundary - a turn can belong to two epochs, and the earlier
draft's `crossed_compaction` flag was a partial answer to a problem M5 has to solve properly.

`measured_before` is the **pre-turn** anchor, not the first anchor *within* the turn. Measured
ordering makes this load-bearing: the first usage of a turn already reflects the user's prompt and
the first model response, so `last_in_turn − first_in_turn` silently omits everything the prompt
itself contributed - catastrophically so on turn one, where it would omit the entire initial
context. Capturing the previous global anchor on `TurnStarted` makes
`measured_after − measured_before` mean what it claims: the change in measured active context
across the whole turn. The intermediate anchors still exist for reconciliation.

```rust
/// What `/ctx` renders. Read back via `ContextProfiler::state()`.
pub struct ContextSnapshot {
    pub window: Option<i64>,
    pub reported_context_tokens: Option<i64>,
    pub initial_context: Option<InitialContextSummary>,
    pub by_category: Vec<(Category, i64)>,
    pub baseline_tokens: Option<i64>,
    pub drift_tokens: i64,
    pub groups: Vec<ItemGroup>,          // complete list; the view caps it
    pub turns: Vec<TurnDelta>,
}

impl ContextSnapshot {
    /// Items only. Derived, not stored - a stored copy can disagree with its inputs.
    pub fn attributed_tokens(&self) -> i64 { self.by_category.iter().map(|(_, t)| t).sum() }
    pub fn explained_tokens(&self) -> i64 {
        self.attributed_tokens() + self.baseline_tokens.unwrap_or(0) + self.drift_tokens
    }
}

/// Startup context, anchored to the authoritative first-request measurement rather
/// than assembled from independent estimates. Three stored fields; the rest derive.
pub struct InitialContextSummary {
    pub first_request_input_tokens: i64,      // measured
    pub estimated_user_input_tokens: i64,     // estimated
    pub estimated_instruction_tokens: i64,    // estimated
}

impl InitialContextSummary {
    pub fn startup_context_tokens(&self) -> i64 {
        self.first_request_input_tokens - self.estimated_user_input_tokens
    }
    /// System prompt + tool schemas. Whatever the measured total leaves after the
    /// observed instruction items are estimated - so the parts sum by construction
    /// and the headline cannot exceed what was actually sent.
    pub fn hidden_tokens(&self) -> i64 {
        self.startup_context_tokens() - self.estimated_instruction_tokens
    }
}

pub struct ProfilerState {
    pub snapshot: ContextSnapshot,
    /// `None` while attribution is trustworthy.
    pub invalidated: Option<InvalidationReason>,
    /// Surfaced in `/ctx`: classification gaps we know about.
    pub unrecognized_fragment_count: u32,
    /// Anchors we could not record because usage was absent.
    pub missing_usage_count: u32,
    /// Reconciliation input, not diagnostics.
    pub anchors: Vec<UsageSnapshot>,
}
```

**`hidden_tokens()` and `baseline_tokens` are the same quantity by two routes** - one from the
first-request decomposition, one from the first-anchor residual. They must agree. The accumulator
computes `baseline_tokens` from the anchor residual and asserts consistency with
`initial_context.hidden_tokens()`; a material disagreement is a bug in one of the two, and an
M1 test asserts it.

### What an earlier draft carried, and why it is gone

| Cut | Why |
|---|---|
| `ProfilerProvenance`, `SnapshotSource` | Every field is M5/M7. `Mixed` is, by our own M7 analysis, unreachable from a cold resume - yet a `mixed` snapshot test was mandatory. |
| `ProfilerDiagnostics` as a struct | Two of its five counters survive. A struct for two fields plus the reconciliation input was misfiling `anchors` under "anomaly counters". |
| `turn_id_mismatch_count` | Added *because* Spike A measured **zero** mismatches, and §4.3 explains why they cannot occur. |
| `out_of_turn_item_count`, `dropped_event_count` | No evidence of the first; the second restates `invalidated` as a number. |
| `AttributionCompleteness` | Three states, one undefined (`IncompleteReason` was never written down). `Degraded` is a caller-side branch: the TUI knows at thread start whether it set the flag. |
| `attributed_tokens` field | Equals the sum of `by_category`. Stored derived state can disagree with its inputs. |
| 2 of 5 `InitialContextSummary` fields | Pure derivations of the other three. |

**The organising constraint is gone too.** All of that machinery existed to make
`assert_eq!(live.snapshot, hydrated.snapshot)` correct by construction - a test that cannot run
until **M7**, on a path this document says is architecturally unresolved. It was also still broken:
`window` is `None` when the live usage merge fails but always `Some(..)` from the rollout's
`TokenCountEvent`, so the fourth leak survived the structural fix. Reinstate the constraint at M7,
when a second path exists to compare against.

The three-way split is what makes strict live-vs-rollout equality expressible at all. Superseded
alternatives in findings §9.

**Percentages are not the profiler's job.** `BASELINE_TOKENS = 12000` is TUI display policy
(`tui/src/token_usage.rs:9`), not a context-profiling concept, and duplicating it inside a crate we
deliberately kept at the `codex-protocol` layer would leak exactly the boundary we drew. The
profiler exposes `reported_context_tokens`, `window`, and the attributed/residual composition; the
TUI computes both percentages with its own helper.

The TUI turns `baseline_tokens` into percentages using its own
`percent_of_context_window_remaining`, once with `BASELINE_TOKENS` and once with
`snapshot.baseline_tokens`. Those two computations live in the TUI's profiler module, not in
`view.rs`, which stays free of arithmetic.

Note "reconciled", not "measured". We never observe the baseline directly; we infer it as a
residual. Spike B identified *what* it consists of (tool schemas plus base system prompt), but the
live quantity is still arrived at through our estimator.

`ContextSnapshot` carries what `/ctx` renders. Invalidation state and counters sit beside it on
`ProfilerState` rather than inside it.

The snapshot carries complete lists; truncation happens in the renderer. This keeps the pager
overlay a pure view addition later rather than a model change.

## Attribution by anchor delta

**Measured, in Spike C:** between every pair of consecutive anchors in the captured session,
**exactly one item was added**. Its true token cost is therefore arithmetic, not estimation:

```
tokens(item added between anchor n and n+1) = input_tokens(n+1) − total_tokens(n)
```

Four measured examples (findings §9.2): 1,040 / 3,373 / 2,219 / 5,043 tokens for four specific tool
outputs. Not estimates.

This reorders the whole attribution model:

| item class | cost |
|---|---|
| **tool outputs** - the large items, and the entire "largest contributors" view | **exact**, from anchor deltas |
| **model outputs** - reasoning, messages, tool calls | **exact in aggregate** per response, from `output_tokens` |
| several items between one pair of anchors (parallel tool calls) | estimate, to apportion the measured delta |
| startup context | estimate, to apportion the measured first-request total |

The estimator survives, but demoted: it apportions measured totals rather than being the primary
source of per-item numbers. It also **sidesteps truncation entirely** - we never need to know what
`ContextManager` trimmed, because the delta spans the transformation.

**One item per delta held 6/6 across two further captures - but it is a property of code mode,
not of Codex.** Under code mode the model writes a single script per response, so a response emits
exactly one tool call however many operations the task needs; asking for three independent file
reads produced one `wc -l`. Multi-item deltas cannot arise in that configuration, and
`-c features.code_mode=false` did not disable it (findings §10.3).

So M2 **implements apportioning defensively** - a delta covering several items is split by estimate
- and tests it with synthetic `ProfilerEvent` sequences rather than a capture. That is cheaper and
more controllable than fighting the config, and the `<-- MULTI` flag in
`specs/tools/analyse_capture.py` will catch a real case if M6 dogfooding produces one.

**Two measured caveats on the arithmetic:**

- **It does not hold across turn boundaries.** One capture showed `input(n+1)` *below* `total(n)` by
  192 tokens - context shrank between turns. Harmless, since tool outputs live within turns, but the
  accumulator must not treat a negative delta as a bug (findings §10.4).
- **Interrupted turns strand items past the last anchor.** Those items have no measured cost and can
  only be estimated (findings §10.1).

## Reconciliation

**Anchor on `total_tokens` (`input + output`), counting every item seen so far.**

Verified empirically: usage arrives *after* a response's own output items and *before* the tool
output that becomes the next request's input. `total_tokens` equals `input + output` exactly
(25,230 + 192 = 25,422; 29,137 + 93 = 29,230). Anchoring on the total removes the need for any
"which items were outputs" bookkeeping.

**Re-solve at every anchor**, never once:

```
residual_n  = reported_n − Σ est(items ≤ seq_n)     // = baseline + drift_n
baseline    = residual at the first anchor of epoch 0
              in a Live session with ≥1 item observed
drift_n     = residual_n − baseline
```

The baseline reference is the **first trustworthy anchor**, deliberately not a minimum over early
anchors - `min` is wrong whenever the estimator over-estimates, which is the case to expect here.
Reasoning in findings §9.

A hydrated session (M7) has no clean baseline anchor, since its prefix was never observed. That is
an M7 problem, not an MVP one.

**Upgrade path, not built for the MVP:** with roughly four anchors per turn, regressing `residual_n`
against `items_n` yields the baseline as the intercept and per-item estimator bias as the slope.
The M1 dataset will show whether the first-anchor estimate is systematically off. If it is not, we
never build the fit.

Estimator errors are additive, so drift grows with item count. Solving once would freeze the
baseline at whatever the first anchor said and let drift silently contaminate it. `drift_n / Σ est`
also yields an estimator error ratio worth surfacing as a confidence indicator.

Anchor density is per response, not per turn - a single turn produced four anchors in testing, so
the decomposition has plenty of data.

## View

**Surface:** inline history cell in M4, matching `/status`
(`tui/src/status/card.rs` builds a `CompositeHistoryCell`). Each `/ctx` is a frozen snapshot in
scrollback, which gives a manual timeline for free. A full-screen pager overlay
(`Overlay::new_static_with_renderables`, `tui/src/pager_overlay.rs:73`) follows later for the
complete item list.

```
/ctx                                                    ● live

  Context      84,210 / 272,000            72% remaining

  Startup context (before turn 1)         ~25,150
    instructions (AGENTS.md, skills, …)   ~13,430
    system + tools baseline (reconciled)  ~11,720
    ↳ measured: first request = 25,230 input tokens

  Attributed to items                    share of context
    Tool outputs             ~44,600   53%  ██████████████
    Instructions             ~13,430   16%  ████
    Agent messages            ~8,940   11%  ███
    Reasoning                 ~2,400    3%  █
    User messages             ~1,120    1%  ▍
                             ───────
                             ~70,490   84%

  Not attributable
    System + tools baseline  ~11,720   14%  ████
    Reconciliation drift       2,000    2%   ±2.8%
  ──────────────────────────────────────────────────────
  Explained                   84,210  100%

  Largest contributors (estimated)
   1  shell   cargo test -p codex-core       ~18,400   22%
   2  read    core/src/session/turn.rs       ~11,200   13%
   3  instr   skills_instructions             ~7,190    9%

  Last 5 turns                       added    context
   16  "check the trace format"      +19,880    84,210
   15  "does resume replay…"              —    64,330   ↳ compaction
```

Illustrative figures, but the startup block uses the **real capture**: 25,230 measured input
tokens, minus ~78 for the user's prompt, decomposed as ~13,430 observed instructions plus ~11,720
reconciled hidden baseline. Tildes mark estimated values; untilded numbers are measured or
summed from measured ones.

**Three denominators appear on this screen and they must stay visually distinct.** The header is
percent of window *remaining* (reconciling to the status bar). The breakdown's share column is
percent of *current context* - the "what is it made of" question - which is why it carries its own
`share of context` heading rather than a bare `%`. Turn deltas are absolute tokens.

**Attributed and explained are different totals.** Items sum to 70,490; the baseline is by
definition what we *cannot* attribute to any item, so folding it into an "attributed" line would
be self-contradictory. The split is the product's central distinction made visible: what we can
name, versus what we can only reconcile.

A turn with no prior anchor (first of an epoch, or first after a compaction seals one) renders
`—` in the added column, never `0`. Zero would read as "this turn added nothing".

**The headline is startup context, and it is a snapshot, not an invariant.** Calling it "fixed
overhead" would be wrong: `build_prompt` takes `tools` from
`step_context.tool_router.model_visible_specs()` and step contexts are captured per step inside
the tool loop, while `build_skills_and_plugins` injects skill instructions mid-session when a
skill is mentioned. Both halves can grow. So the profiler captures an `InitialContextSummary`
around the first trustworthy inference and renders that separately from current composition. The
defensible claim is *"Codex was already carrying ~25,150 tokens when you started"*, not *"this
amount is fixed for the session"*.

**The figure is bounded by measurement, not assembled from estimates.** `InitialContextSummary` is
derived *from* `first_request_input_tokens`, so the parts sum to a measured whole by construction
and the headline cannot exceed what was actually sent. A superseded draft violated this; see
findings §9 and the process note.

Reconciled baselines in a default configuration come out near 11,700, so the two percentages agree
to within a point and putting them side by side would look like a bug for no benefit. The view
surfaces the comparison **only when they diverge materially** - which happens on small windows (the
denominator moves from 20,000 to 6,700 at a 32k window) or heavy tool/skill configurations.

When `completeness` is not `Complete`, the breakdown is replaced, never annotated:

```
  ⚠ Context breakdown incomplete
    3 app-server events were dropped.
    Codex context: 71% remaining.
    Attribution unavailable until the session is replayed from its rollout.
```

The verified, actionable claim is the top block: *this much of your window is spent before your
prompt does anything, and here is what it is.*

**Degraded state** when raw events are unavailable: show Codex's own number, explain the gap,
never render a partial breakdown.

**Provenance** is an M7 concern. The MVP profiles live sessions only, so the header carries no
source marker.

## Testing

Per `AGENTS.md:169` new test modules use `#[path = "..._tests.rs"]` sibling files; per `:213`
`pretty_assertions::assert_eq` with deep equality on whole objects. Run with `just test -p <crate>`,
never `cargo test` directly. Snapshot review uses `cargo insta pending-snapshots` /
`cargo insta accept` per `AGENTS.md:194-198`.

**1. Accumulator units.** `Vec<ProfilerEvent>` in, whole `ContextSnapshot` asserted out. The first
test to write is **multiple usage anchors within one turn**, because that is the common case, not
an edge case.

**One case is mandatory and comes from experience, not theory.** The M1 analysis script produced
wrong output twice, and both bugs were the same mistake: *assuming a container's iteration order
carries meaning*. One paired observed against sent items by dict order (a `-787%` diff); the other
used a percentage threshold with no absolute floor (an `86% TRUNCATED` verdict on a 382-byte item).
Neither was caught by a test - both were visible only because a number looked implausible.

The accumulator does exactly this class of work: ordering by `seq`, pairing calls with outputs,
aligning anchors with the items preceding them. So there must be a test whose input is
**deliberately shuffled relative to arrival order**, asserting the fold still produces the correct
result. Without it the same bug reappears somewhere it yields plausible numbers rather than an
obvious `-787%`. Findings §10.5. Then: residual establishment at the first anchor, drift updates at later anchors
(named so they do not imply the baseline is permanently fixed), epoch sealing, `call_id` grouping,
`Resume` epochs, out-of-turn items, anomaly counters.

**2. The oracle test.** At a usage anchor the accumulator has *already* observed that response's
output items, while `request_item_ids` describes the input **before** those existed. Comparing them
directly compares two different states. The reduced trace carries both lists, so the correct
identity is an **ordered sequence** comparison:

```
reconstructed model-history items at anchor N
                            ==  request_item_ids(N) ++ response_item_ids(N)
                                 (minus base_system_prompt, which we never see)
residual accounts for { base_system_prompt, additional_tools }
```

Confirmed by the measured data: call 1 had `n_req: 6, n_resp: 3`, call 2 had `n_req: 10`. The
missing item between 9 and 10 is the tool output, which belongs to neither list for call 1.

"**Reconstructed model-history items**", not "items observed" - the distinction is load-bearing.
Raw observed bodies are pre-truncation; `request_item_ids` resolves to what was actually sent. The
comparison therefore runs against our *post-transformation* reconstruction, which is precisely what
Spike C has to get right. If Spike C fails, this test fails, and that is the correct signal.

The reduced model has no real Responses item ids (`item_id` echoes the synthetic key), so compare
ordered tuples of `(kind, role, call_id, body_len)` rather than id lists. Plus the arithmetic
check that validates the whole model in one line:

```
| solved_baseline − tokens(additional_tools + system_prompt) | < tolerance
```

No fuzzy tolerances on set membership. A strict assertion makes new Codex behaviour visible
immediately; `assert!(difference.len() < 5)` becomes a permanent excuse.

**3. Live/rollout equivalence - M7, not now.** Profile live, replay the rollout, `assert_eq!` the
snapshots. The rollout drops `AdditionalTools`, `CompactionTrigger` and `Other`
(`rollout/src/policy.rs:42`), but none appear on the live stream either, so the expected delta is
empty. Note this test cannot run until a rollout adapter exists, which is why the MVP data model
is **not** shaped around making it pass.

**4. Estimator calibration regression.** The measured ladder gives exact token deltas for
identifiable item groups. Bound generously - catch order-of-magnitude regressions like the
reasoning case, not normal noise. A tight bound becomes a test people tune until it passes.

**5. View snapshots.** `insta` coverage is mandatory for user-visible UI (`AGENTS.md:184`).
States: normal, raw-events-unavailable, invalidated (dropped events), invalidated (compaction),
large residual, empty/new session, and one huge contributor with a long label (layout stress).

### Fixtures

Two kinds, deliberately independent so the oracle test is not circular:

- **Semantic fixture** - a scrubbed `ProfilerEvent` stream for equivalence, classification,
  grouping and reconciliation tests. Only kind, byte length, tags, `call_id`, `turn_id` and usage
  matter, so content can be synthetic. The scrubber must be deterministic so fixture diffs stay
  stable, and guarded by `assert_no_home_paths()`-style checks over committed files.
- **Oracle fixture** - a minimal scrubbed representation of the reduced trace. It must carry the
  `conversation_items` metadata as well as the inference calls, because the ids in
  `request_item_ids` are synthetic (`conversation_item:1`) and are useless without the table that
  resolves them into the structural tuples the test compares. No bodies are needed, which is also
  what keeps the fixture free of source content:

  ```rust
  pub struct OracleFixture {
      pub conversation_items: Vec<OracleItem>,
      pub inference_calls: Vec<OracleInferenceCall>,
  }

  pub struct OracleItem {
      pub synthetic_id: String,       // "conversation_item:1"
      pub kind: String,               // "message" | "reasoning" | "custom_tool_call" | …
      pub role: String,
      pub call_id: Option<String>,
      pub body_len: usize,            // length only, never content
  }

  pub struct OracleInferenceCall {
      pub request_item_ids: Vec<String>,
      pub response_item_ids: Vec<String>,
      pub usage: OracleUsage,
  }
  ```

**Calibration fixtures are captured from a deliberately public session** - a scratch repo with
OSS files and a synthetic `AGENTS.md` - rather than scrubbed. Scrubbing content while keeping the
real content's token counts would compare `estimate(synthetic)` against `tokens(real)`, which
measures nothing. Density is the entire point of calibration, so it needs real content.

**Never commit raw traces.** Bundles contain prompts, source, tool output and absolute paths in
plaintext (`rollout-trace/README.md:3-8`).

### Deliberately not tested

- That `thread_start_params_from_config` sets the flag - `AGENTS.md:30` forbids testing statically
  defined values. Fold it into the existing deep-equality params test
  (`tui/src/app_server_session.rs:2686`).
- Core integration tests under `core/suite` - `AGENTS.md:114` reserves those for agent-logic
  changes; we change none. Raw-event delivery has upstream coverage
  (`app-server/tests/suite/v2/turn_start.rs:835`).
- Tokenizer accuracy, which is explicitly not a product guarantee.

## Milestones

| # | Deliverable | Status |
|---|---|---|
| M0 | Build and run the fork with a tool-call turn | **Done** |
| M1 | Answer one question. No crate, no production code. See below. | **Done** |
| M2 | The crate, in four reviewed stages - see below | Next |
| M3 | Reconciliation: continuous re-solve, baseline/drift, `InitialContextSummary` | |
| M4 | `/ctx` inline card + `insta` coverage | |
| M5 | Epochs and compaction: sealing, before/after, turns spanning a boundary, compaction-kind inference | |
| M6 | Dogfood on real work; validate attribution; find out which views are actually used | |
| M7 | Rollout hydration - `RolloutItem` adapter, provenance, live-vs-rollout equivalence | |

### M2, staged

Roughly 1,700-2,000 lines with tests - 3-4 changes' worth under `AGENTS.md:127`'s change-size
guidance, so it lands as four sequential branches off `context-inspector-mvp`, each reviewed as a
PR against that branch (never `main`, which stays an upstream mirror) and merged with a merge
commit. Upstream syncs happen on `context-inspector-mvp` only, between stages, never into an open
branch. Each stage compiles and passes `just test -p codex-context-profiler` at every commit.

| Stage | Branch | Scope (~lines) | Exit criterion |
|---|---|---|---|
| M2a | `context-profiler-crate` | Crate skeleton, `BUILD.bazel` + lock update, `ProfilerEvent` and all model types, no logic (~350) | Workspace builds; `just bazel-lock-check` passes |
| M2b | `profiler-live-adapter` | Scoped `experimental_raw_events`, notification→event, usage buffering, `Lagged` broadcast, `ProfilerRegistry` on `App`, JSONL writer (~500) | A live session's adapter JSONL matches the M1 probe log |
| M2c | `profiler-accumulator` | Fold, grouping, turn deltas, anchor-delta attribution incl. multi-item apportioning (~500) | Fed the M1 captures, reproduces `analyse_capture.py`'s measured deltas (1,040 / 3,373 / 2,219 / 5,043); handles the interrupted turn's stranded items and the −192 boundary; **shuffled-input test passes** (findings §10.5) |
| M2d | `profiler-classification` | Classification + unrecognised-fragment counter, estimator with `Reasoning` special-case, scrubber + committed fixtures (~400) | Category totals over the captures are sane; fixtures pass `assert_no_home_paths` |

Ordering is dependency order, and deliberately puts the two exact, provable layers (adapter,
accumulator) before the fuzzy one (classification, estimation) - a wrong number in M2d is then
M2d's fault, not something beneath it. M2b is the only stage touching `codex-tui`.

### M1, re-scoped

M1's exit question is:

> **Can we reconstruct what was actually sent to the model from what we can observe?**

Nothing in a crate skeleton, `BUILD.bazel`, a JSONL schema, or a deterministic scrubber helps
answer that, and all of it would be redone once classification exists. An earlier draft bundled
them into M1 anyway. Removed.

What M1 actually is:

| Task | Status |
|---|---|
| **0. `TruncationPolicy` reachability** | **Resolved** - not needed; anchor deltas measure across the transformation |
| **Spike C** - observed vs sent | **Answered.** No `ContextManager` truncation in three captures. The tool layer truncates and declares itself. Findings §9.1, §10.2 |
| **Spike D** - interrupted turn | **Answered.** `status=Interrupted`, no `raw_usage` at all, items stranded past the last anchor. Findings §10.1 |
| **Q1** - one item per delta | **Answered with a caveat.** Holds 6/6, but as a property of code mode; multi-item deltas are unobservable in this config. Findings §10.3 |

**M1 is complete.** Three captures answered every question it was scoped around, and two produced
findings the milestone was not looking for: the delta arithmetic breaks across turn boundaries
(§10.4), and the analysis tooling's own bugs dictated a mandatory accumulator test (§10.5).

Deliberately **not** pursued: forcing parallel tool calls. `-c features.code_mode=false` did not
disable code mode, so the next step would be a config investigation rather than a capture - real
time for a question that only affects a fallback path M2 will implement defensively regardless.

### Spikes

Spikes A (raw stream flows; ordering; anchor density) and B (trace oracle) are complete - see
findings. **Two spikes remain and one is a closed design decision.** C and D are **M1's acceptance
scenarios rather than separate detours**, since both run against the instrumentation M1 builds.

**Spike C - reproducing the history transformation.** Not just truncation: can we reproduce the
model-relevant `ContextManager` transformation for the item classes the MVP supports?

Run one session with a tool output well past 12,000 bytes (aim for 200KB+; Spike A's 17KB was only
~45% over and the effect was ambiguous). Compare the probe's raw `bytes=` against the trace's
`conversation_items[].body_len`, then test whether applying `codex-utils-output-truncation`
reproduces the sent size exactly.

Keep the fixture deliberately narrow at first - **text only, successful call/output pairs, no
media** - so `normalize_history` should be identity and truncation is the only variable. Spike D's
interrupted turn then becomes the natural test of the synthesised-output branch
(`ensure_call_outputs_present`). Media items are marked unsupported for the MVP rather than
silently mis-attributed.

Open sub-questions: can the profiler obtain the active model's `TruncationPolicy`, does
`policy * 1.2` behave as expected, and does the audio path matter (it needs core-private
`estimate_audio_token_count`). **Run first** - it is the only spike whose outcome changes M2's
design.

**Spike D - usage pairing edge cases.** Interrupt a turn mid-tool-call. Establish whether a
`raw_usage` with `usage: None` arrives or the notification is skipped entirely, whether
`token_usage` still fires, and whether `TurnAborted` arrives. Feeds the adapter's buffering policy.
Same harness, minutes of work after C.

**Spike E - event lag: deliberately not run.** The channel is `CHANNEL_CAPACITY = 128` and a single
measured turn produced several hundred notifications, so lag is an expected condition rather than a
tail risk - which is presumably why `AppServerEvent::Lagged` exists and is already handled.
Measuring its frequency would not change what we build: invalidation handling has to exist either
way, and implementing it is cheaper than instrumenting to count occurrences.

## Open questions

1. **Truncation strategy** (Spike C). If reproduction via `codex-utils-output-truncation` is exact,
   attribution stays direct. If the policy is unobtainable from the TUI, the fallback is inferring
   effective contribution from successive usage anchors. Blocks M2.
2. **Estimator calibration strategy** - global correction factor, per-category factors, or none.
   Needs drift data before deciding.
3. **Rollout-backed compaction enrichment on the live path** (M5). `CompactedItem` carries
   `replacement_history`, which the live notification does not, but the recorder writes
   asynchronously (`rollout/src/recorder.rs:1971`) so there is no flush guarantee when
   `thread/compacted` arrives.

### Resolved since the first draft

**Profiler instances live in an `App`-owned registry**, not `ThreadSessionState`:

```rust
struct ProfilerRegistry { threads: HashMap<ThreadId, ContextProfiler> }
```

`ThreadSessionState` (`tui/src/session_state.rs:30`) is a `Clone + PartialEq` settings snapshot
shared across widgets; a growing mutable reducer holding epochs and items does not belong there.
Notification routing already extracts `ThreadId` centrally
(`tui/src/app/app_server_event_targets.rs:97`), including for raw items, raw usage, token usage and
compaction, so the insertion point is clean and it handles side threads and forks naturally.

**Raw events are enabled narrowly, not globally.** `thread_start_params_from_config` has three
non-test call sites (`tui/src/app_server_session.rs:436`, `:754`, `:1580`) and is used for helper
threads as well as the primary session. Raw items are large and share the 128-slot channel, so
enabling them everywhere directly raises lag probability - the same problem `Invalidated` exists to
handle. M1 scopes the flag to the thread being profiled; if that proves awkward, it may enable
broadly *and measure event volume*, which feeds the same question.

### M7 is harder than "hydrate once"

`ThreadResumeParams` has no `experimental_raw_events` field, and the flag lives on in-memory
`ThreadState`, so **a thread resumed in a fresh process emits no raw events for its future turns
either**. M7 therefore cannot be "replay the rollout, then profile live". It needs one of:

- **A.** tail the rollout as new turns are persisted (no protocol change, but polling a file the
  recorder writes asynchronously);
- **B.** post-hoc profiling only - `/ctx` on a resumed session shows a hydrated snapshot that does
  not update live;
- **C.** add `experimental_raw_events` to `ThreadResumeParams` - a protocol change we ruled out for
  the MVP.

This also means `SnapshotSource::Mixed` is only reachable by rejoining an already-running thread
whose in-memory flag survives, not by resuming a cold one. Decide at M7; documented now so the
milestone does not arrive with the wrong architecture assumed.

## Note on location

Filed under `specs/` rather than `docs/` because `AGENTS.md:32` reserves `docs/` and bars general
product or user-facing documentation from it. These are internal engineering specs for this fork,
so they get their own top-level folder. Later design docs (M5 compaction, the eventual
context-management design) belong here too.
