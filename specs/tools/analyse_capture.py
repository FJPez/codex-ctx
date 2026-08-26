#!/usr/bin/env python3
"""Analyse an M1 capture: a rollout-trace bundle plus (optionally) the probe log.

Answers the two open M1 questions:

  Q1  Does exactly one item arrive between each pair of usage anchors?
      If not, anchor-delta attribution cannot cost items individually and the
      delta must be apportioned by estimate.

  Q2  Does ContextManager truncate before sending?
      Compares what the raw stream carried against what the trace says was sent.
      Needs the probe log.

Usage:
    analyse_capture.py <trace-bundle-dir> [probe.log]

The bundle must already be reduced:
    just codex debug trace-reduce <trace-bundle-dir>
"""

import json
import re
import sys
from pathlib import Path


def load_ordered_calls(bundle: Path):
    """Inference calls in wire order, taken from trace.jsonl rather than sorted
    by token count - a compaction makes input_tokens non-monotonic."""
    state = json.loads((bundle / "state.json").read_text())
    calls = state["inference_calls"]

    order = []
    for line in (bundle / "trace.jsonl").read_text().splitlines():
        if not line.strip():
            continue
        payload = json.loads(line).get("payload", {})
        if payload.get("type") == "inference_started":
            cid = payload.get("inference_call_id")
            if cid in calls:
                order.append(cid)

    seen = set()
    ordered = [calls[c] for c in order if not (c in seen or seen.add(c))]
    # Fall back to token order if trace.jsonl gave us nothing useful.
    if not ordered:
        ordered = sorted(calls.values(), key=lambda c: c["usage"]["input_tokens"])
    return state, ordered


def body_text_len(body) -> int:
    """Length of the actual text sent, not the JSON repr."""
    if body is None:
        return 0
    if isinstance(body, str):
        return len(body)
    if isinstance(body, dict) and "parts" in body:
        return sum(len(p.get("text", "")) for p in body["parts"])
    return len(json.dumps(body))


# `ResponseItem` variants in declaration order (protocol/src/models.rs:963), so
# older probe logs that recorded `Discriminant(N)` can still be read.
DISCRIMINANTS = [
    "AdditionalTools", "Message", "AgentMessage", "Reasoning", "LocalShellCall",
    "FunctionCall", "ToolSearchCall", "FunctionCallOutput", "CustomToolCall",
    "CustomToolCallOutput", "ToolSearchOutput", "WebSearchCall",
    "ImageGenerationCall", "Compaction", "CompactionTrigger", "ContextCompaction",
    "Other",
]


def normalise_kind(kind: str) -> str:
    m = re.fullmatch(r"Discriminant\((\d+)\)", kind)
    if m:
        idx = int(m.group(1))
        return DISCRIMINANTS[idx] if idx < len(DISCRIMINANTS) else kind
    return kind


def parse_probe(path: Path):
    """Observed raw-stream item sizes, in arrival order, per kind."""
    observed = []
    for line in path.read_text().splitlines():
        m = re.search(r"raw_item\s+turn=(\S+)\s+kind=(\S+?)\s+.*?bytes=(\d+)", line)
        if m:
            observed.append(
                {
                    "turn": m.group(1),
                    "kind": normalise_kind(m.group(2)),
                    "bytes": int(m.group(3)),
                }
            )
    return observed


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    bundle = Path(sys.argv[1])
    if not (bundle / "state.json").exists():
        print(f"no state.json in {bundle} - run `just codex debug trace-reduce {bundle}` first")
        return 1

    state, calls = load_ordered_calls(bundle)
    items = state["conversation_items"]

    print(f"{len(calls)} inference calls, {len(items)} conversation items\n")

    # ---- Q1: items per anchor delta -------------------------------------
    print("=" * 78)
    print("Q1  items added between consecutive usage anchors")
    print("=" * 78)
    print(f"{'#':>2} {'input':>8} {'output':>7} {'total':>8} {'Δ':>7} {'n':>3}  items added")

    prev_total = None
    prev_seen: set[str] = set()
    multi = []
    negatives = []

    for n, call in enumerate(calls, 1):
        usage = call["usage"]
        total = usage["input_tokens"] + usage["output_tokens"]
        added = [i for i in call["request_item_ids"] if i not in prev_seen]
        delta = usage["input_tokens"] - prev_total if prev_total is not None else None

        desc = ", ".join(
            f"{items[a]['kind']}({body_text_len(items[a].get('body'))}B)" for a in added
        )
        flag = ""
        if delta is not None and len(added) > 1:
            flag = "  <-- MULTI"
            multi.append((n, len(added), delta))
        if delta is not None and delta < 0:
            flag += "  <-- NEGATIVE"
            negatives.append((n, delta))

        print(
            f"{n:>2} {usage['input_tokens']:>8} {usage['output_tokens']:>7} {total:>8} "
            f"{('-' if delta is None else delta):>7} {len(added):>3}  {desc[:70]}{flag}"
        )

        prev_total = total
        prev_seen = set(call["request_item_ids"]) | set(call["response_item_ids"])

    print()

    # Was the condition Q1 tests for even present? If no response ever emitted
    # more than one tool call, "one item per delta" is vacuous - code mode wraps
    # all work in a single call, so multi-item deltas cannot arise.
    max_calls_per_response = 0
    for call in calls:
        n_calls = sum(
            1
            for i in call["response_item_ids"]
            if "call" in items[i]["kind"].lower() and "output" not in items[i]["kind"].lower()
        )
        max_calls_per_response = max(max_calls_per_response, n_calls)

    if multi:
        print(f"Q1 ANSWER: NO - {len(multi)} anchor pair(s) carried more than one item.")
        print("           Deltas must be apportioned by estimate across those items.")
        for n, count, delta in multi:
            print(f"           call {n}: {count} items sharing {delta} tokens")
    elif max_calls_per_response <= 1:
        print("Q1 ANSWER: VACUOUS - no response emitted more than one tool call")
        print(f"           (max was {max_calls_per_response}), so multi-item deltas could not")
        print("           arise. Likely code mode wrapping all work in one call.")
        print("           Re-run with code mode disabled to test the other path.")
    else:
        print("Q1 ANSWER: YES - exactly one item per delta, and the capture did")
        print(f"           contain responses with up to {max_calls_per_response} tool calls.")
        print("           Per-item token costs are measurable, not estimated.")

    if negatives:
        print()
        print(f"ANOMALY: {len(negatives)} negative delta(s) - context shrank between anchors.")
        for n, delta in negatives:
            print(f"         call {n}: {delta} tokens")
        print("         `input(n+1) = total(n) + new items` does not hold here.")
        print("         Expected across turn boundaries; investigate if within a turn.")
    print()

    # ---- Q2: observed vs sent -------------------------------------------
    if len(sys.argv) < 3:
        print("(no probe log given - skipping Q2)")
        return 0

    probe = Path(sys.argv[2])
    if not probe.exists():
        print(f"probe log not found: {probe}")
        return 1

    print("=" * 78)
    print("Q2  observed on the raw stream  vs  sent to the model")
    print("=" * 78)

    observed = parse_probe(probe)
    obs_outputs = [o for o in observed if "output" in o["kind"].lower()]

    # Sent items must be ordered by first appearance across the ordered calls.
    # `conversation_items` is a dict whose iteration order does NOT match arrival
    # order, and pairing on it produced a nonsensical -787% "diff".
    seen_order: list[str] = []
    for call in calls:
        for i in call["request_item_ids"] + call["response_item_ids"]:
            if i not in seen_order:
                seen_order.append(i)
    sent_outputs = [items[i] for i in seen_order if "output" in items[i]["kind"].lower()]

    if not obs_outputs:
        print("no tool-output items found in the probe log")
        return 0

    if len(obs_outputs) != len(sent_outputs):
        print(
            f"NOTE: {len(obs_outputs)} observed vs {len(sent_outputs)} sent tool outputs - "
            "pairing positionally, trailing rows may be an interrupted turn.\n"
        )

    print(f"{'observed B':>11} {'sent text B':>12} {'diff':>8} {'diff %':>8}  verdict")

    # A real ContextManager truncation removes a large ABSOLUTE amount. On a tiny
    # payload the JSON envelope alone is most of the bytes, so a percentage test
    # on its own produces false positives - it flagged a 382 -> 52 byte item at 86%.
    MIN_ABSOLUTE_LOSS = 4096

    ctx_truncated = False
    for obs, sent in zip(obs_outputs, sent_outputs):
        sent_len = body_text_len(sent.get("body"))
        diff = obs["bytes"] - sent_len
        pct = (diff / obs["bytes"] * 100) if obs["bytes"] else 0
        if diff > MIN_ABSOLUTE_LOSS and pct > 20:
            verdict = "TRUNCATED by ContextManager"
            ctx_truncated = True
        else:
            verdict = "intact (JSON envelope)"
        print(f"{obs['bytes']:>11} {sent_len:>12} {diff:>8} {pct:>7.1f}%  {verdict}")

    print()
    print(
        "Q2 ANSWER: ContextManager truncation observed."
        if ctx_truncated
        else "Q2 ANSWER: no ContextManager truncation - differences are JSON envelope only."
    )

    # ---- Tool-layer truncation ------------------------------------------
    # Distinct from ContextManager: the tool caps its own output upstream of
    # everything we observe, and says so in the body text.
    print()
    print("=" * 78)
    print("Tool-layer truncation (upstream of the raw stream, self-declaring)")
    print("=" * 78)

    warn = re.compile(r"truncated output \(original token count: (\d+)\)")
    found = False
    for key in items:
        text = ""
        body = items[key].get("body")
        if isinstance(body, dict) and "parts" in body:
            text = "".join(p.get("text", "") for p in body["parts"])
        m = warn.search(text)
        if m:
            found = True
            print(
                f"  {key}: tool reported {int(m.group(1)):,} original tokens, "
                f"{len(text):,} chars survived"
            )
    if not found:
        print("  none - no tool output declared itself truncated")
    else:
        print()
        print("  The tool truncates before the item exists, so both the raw stream")
        print("  and the trace already carry the capped content. Readable from the")
        print("  body text - no need to reproduce any truncation logic.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
