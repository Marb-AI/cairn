#!/usr/bin/env python3
"""Reconstruct round trips from a run's tool-call log.

A round trip is one inference, not one tool call. Calls issued together in one turn are
dispatched at once and arrive milliseconds apart; calls in separate turns are separated
by an inference, which is seconds. So turns are gaps, and the threshold sits in the empty
band between the two - measured at 1.8-2.6 s between turns against sub-100 ms within one.

Reported per run: turns (the metric), calls (what was spent), and the largest
within-turn gap, so a run where the threshold was doing real work is visible rather than
assumed.

Usage: cluster.py runs/<run-id>.jsonl [gap-seconds]
"""

import json
import sys

# Between the two observed bands and far from both. Printed with the result so a run that
# sits near it can be re-examined instead of trusted.
DEFAULT_GAP = 1.0

# The parent's own calls, which bracket every run: the Agent call that starts it and the
# marker command that closes it.
MARKER = "EVAL-RUN-END"

# Loading the arm's instructions. Excluded for the same reason the skill's tokens are
# excluded from the per-question number: in the real product the guide is already in
# context when the question arrives, so the call that puts it there is harness overhead
# rather than a step towards the answer. Both arms pay exactly one of these.
PREAMBLE = "eval/arms/"


def load(path):
    rows = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    gap = float(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_GAP
    rows = [
        r
        for r in load(sys.argv[1])
        if r.get("tool") != "Agent"
        and MARKER not in r.get("detail", "")
        and PREAMBLE not in r.get("detail", "")
    ]
    if not rows:
        print("no calls recorded")
        return 1
    rows.sort(key=lambda r: r["t"])

    turns, widest_within = 1, 0.0
    for prev, cur in zip(rows, rows[1:]):
        d = cur["t"] - prev["t"]
        if d > gap:
            turns += 1
        else:
            widest_within = max(widest_within, d)

    tools = {}
    for r in rows:
        tools[r["tool"]] = tools.get(r["tool"], 0) + 1

    print(f"turns (round trips): {turns}")
    print(f"calls:               {len(rows)}")
    print(f"wall clock:          {rows[-1]['t'] - rows[0]['t']:.1f}s")
    print(f"widest within-turn:  {widest_within:.2f}s   (threshold {gap}s)")
    print("by tool:             " + ", ".join(f"{k} {v}" for k, v in sorted(tools.items())))
    print("\ncalls in order:")
    for r in rows:
        print(f"  {r['tool']:<8} {r['detail'][:150]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
