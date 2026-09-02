# How the profiler knows the agent's current context

The model is stateless and the server exposes no "give me the current context with sizes" request.
The profiler therefore *reconstructs* the context by watching it being built, then proves the
reconstruction against the provider's own token accounting. This file diagrams that process:
the normal path first, then every case where reconstruction degrades and how each is made
legible rather than silently wrong.

Sources: design spec sections 2-4, findings sections 6 and 10 (measured captures).

## 1. The reconstruction pipeline

The real context lives server-side in `ContextManager.items`. Because history is append-only,
mirroring it from the delta stream is trivially correct: both sides only ever push.

```mermaid
flowchart LR
    subgraph server [codex-core]
        CM["ContextManager.items<br/>(the real context)"]
    end
    subgraph stream [app-server v2 raw stream]
        RI["rawResponseItem/completed<br/>(one per appended item)"]
        RU["rawResponse/completed<br/>(usage anchor per response)"]
        TU["thread/tokenUsage/updated<br/>(occupancy + window)"]
    end
    subgraph tui [TUI profiler]
        AD["adapter (M2b)<br/>notifications to events"]
        TR["JSONL trace"]
        AC["accumulator (M2c)<br/>the mirror list"]
        RE["reconciliation (M3)<br/>mirror vs anchors"]
        UI["/ctx card (M4)"]
    end
    CM -->|append| RI
    CM -->|serialize + send| RU
    RI --> AD
    RU --> AD
    TU --> AD
    AD --> TR
    AD --> AC
    AC --> RE
    RE --> UI
```

## 2. Normal case: a measured turn

Anchor deltas turn attribution into measurement. Between consecutive anchors,
`input(n+1) - total(n)` is exactly the token cost of the items that arrived in between,
counted by the provider's tokenizer, not estimated by us. `items_seq` stamped on each anchor
is the join key between "which items" and "how many tokens".

```mermaid
sequenceDiagram
    participant M as Model API
    participant C as codex-core
    participant P as Profiler
    C->>M: request 1 (5 input items + hidden scaffolding)
    M-->>C: output items + usage
    C->>P: input items (items_seq 1..5)
    C->>P: response outputs: reasoning, message, tool call (items_seq 6..8)
    C->>P: anchor A: input 25230, output 192, total 25422 (items_seq 8)
    Note over P: pending anchor stored
    C->>P: tool output item (items_seq 9)
    C->>P: token_usage: last 25422, window 258400
    Note over P: 25422 == 25422, matches_anchor true (consumed)
    C->>M: request 2 (history, 10 items)
    M-->>C: output items + usage
    C->>P: reasoning item (items_seq 10)
    C->>P: anchor B: input 29137, output 93, total 29230 (items_seq 10)
    Note over P: cost of item 9 (the 17301-byte tool output)<br/>= input B - total A = 3715 tokens, measured exactly
```

## 3. Reconciliation: the three layers of "current context"

What `/ctx` will show is assembled from three sources of decreasing visibility, and the sum
must equal the measured occupancy. Drift is displayed, never hidden.

```mermaid
flowchart TD
    A["Layer 1: watched items<br/>mirror rows with measured costs<br/>from anchor deltas"]
    B["Layer 2: startup context<br/>instructions, environment, skills<br/>one burst before the first anchor,<br/>summarised as InitialContextSummary"]
    C["Layer 3: hidden residual<br/>base system prompt + tool schemas<br/>never on the raw stream,<br/>sized as anchor minus attributed (~11.7k)"]
    S["sum of layers"]
    R["reported_context_tokens<br/>(latest anchor, from `last` never `total`)"]
    A --> S
    B --> S
    C --> S
    S --> Q{"sum == measured?"}
    R --> Q
    Q -->|yes| OK["breakdown IS the current context,<br/>to the token"]
    Q -->|no| DR["show the drift explicitly"]
```

## 4. Degraded case: compaction (MVP invalidates, M5 segments)

Compaction is a sanctioned history rewrite: the server appends a checkpoint whose
`replacement_history` supersedes the prefix. The mirror is now wrong and cannot be patched
from deltas. On v2 it arrives only as `ItemCompleted` with `ThreadItem::ContextCompaction`
(the deprecated `ContextCompacted` notification is never sent).

```mermaid
sequenceDiagram
    participant C as codex-core
    participant P as Profiler
    participant U as /ctx
    C->>P: items + anchors (mirror in sync)
    C->>C: auto-compact: replacement history appended
    C->>P: ItemCompleted: ContextCompaction
    Note over P: Invalidated (Compacted)<br/>pending anchor cleared,<br/>items_seq keeps counting
    P->>U: "profile reset by compaction"
    Note over P,U: MVP: profile invalidated.<br/>M5: read the compaction item and<br/>open a new epoch instead.
```

## 5. Degraded case: dropped events (channel lag)

The TUI's broadcast channel can lag under load and skip notifications. A gap in the item
stream would silently corrupt every later anchor join, so the profiler marks the segment
boundary instead of guessing.

```mermaid
sequenceDiagram
    participant C as app-server
    participant Ch as broadcast channel
    participant P as Profiler
    C->>Ch: notifications
    Ch--xP: Lagged, skipped N
    Note over P: Invalidated (EventsDropped, skipped N)<br/>broadcast to every live adapter:<br/>lag is connection-level, no thread id
    Note over P: pending anchor cleared,<br/>items_seq continues,<br/>the Invalidated record IS the segment boundary
    C->>Ch: later notifications
    Ch->>P: recorded normally in the next segment
```

## 6. Degraded case: missing usage and interrupted turns

Two distinct shapes, both measured in the M1 captures. A response can complete with
`usage: None` (a `MissingUsage` record is written). An interrupted turn ends with no
`rawResponse/completed` at all, so its trailing items are stranded past the last anchor:
no `MissingUsage` is written because no response completed.

```mermaid
sequenceDiagram
    participant C as codex-core
    participant P as Profiler
    rect rgb(235, 235, 235)
        Note over C,P: case A: usage: None
        C->>P: rawResponse/completed, usage: None
        Note over P: MissingUsage record,<br/>pending anchor cleared
        C->>P: token_usage update
        Note over P: matches_anchor: None<br/>nothing valid to compare, not "false"
    end
    rect rgb(235, 235, 235)
        Note over C,P: case B: interrupted turn
        C->>P: items after the last anchor
        C->>P: turn ended, status Interrupted
        Note over P: TurnEnded, completed false.<br/>Stranded items have no measured cost<br/>until the next turn's first anchor.
    end
```

## 7. Gap case: attaching late (resume, fork, mid-stream)

Raw events are requested at `thread/start`; resume cannot enable them retroactively, so a
resumed thread's past never reaches the raw stream. The mirror has no history to build from.
The rollout JSONL on disk is the third copy of the memory and the only one that survives a
restart, which is why hydration is a milestone (M7), not a hack.

```mermaid
flowchart TD
    ST{"how did this<br/>thread begin?"}
    ST -->|"thread/start with<br/>raw events enabled"| FULL["full observation:<br/>mirror complete from item zero"]
    ST -->|"fork becomes the<br/>displayed thread"| MID["mid-stream attach:<br/>Attached record marks the join,<br/>items before it unobserved"]
    ST -->|"thread/resume<br/>(no raw events flag)"| RES["no raw stream for the past"]
    RES --> M7["M7: hydrate the mirror by<br/>replaying the rollout JSONL,<br/>then observe live from there"]
    MID --> PART["profile covers the observed<br/>suffix only, labelled as such"]
```
