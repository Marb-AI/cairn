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

# Used only when the data cannot place the threshold itself: too few gaps, or no clear
# empty band between the two modes. Printed with the result either way, so a number that
# came from a fallback is never mistaken for one that came from measurement.
DEFAULT_GAP = 1.0

# Where a threshold could plausibly sit. Below the floor is a batch dispatch; above the
# ceiling everything is an inference by any reading.
SEARCH_LO, SEARCH_HI = 0.05, 5.0
# How empty the band has to be before it counts as a valley rather than a coincidence.
# Adjacent gaps inside the within-turn mode sit 0.01-0.1 s apart, so this is several
# times the spacing the data shows when there is no structure.
MIN_VALLEY = 0.25


def turn_threshold(gaps, default=DEFAULT_GAP):
    """Place the within-turn / between-turn threshold from the gaps themselves.

    Returns `(seconds, valley)` where `valley` is the empty band it came from, or `None`
    when the data could not place it and the default stands.

    The constant this replaces was 1.0 s, chosen when the two bands were "1.8-2.6 s
    between turns against sub-100 ms within one". Under three-way parallelism the
    within-turn band stretched: pooled over 44 runs it now runs to **1.03 s**, with the
    next gap anywhere at **1.93 s**. So 1.0 sat inside the lower mode rather than between
    the modes, and two batched calls were being counted as separate turns.

    The correction moves numbers in this tool's favour, which is exactly why it is derived
    rather than chosen: the widest empty band is where it is, and the same rule would move
    them the other way if the data said so.
    """
    xs = sorted(g for g in gaps if SEARCH_LO < g < SEARCH_HI)
    if len(xs) < 20:
        return default, None
    widest, at = 0.0, None
    for a, b in zip(xs, xs[1:]):
        if b - a > widest:
            widest, at = b - a, (a, b)
    if at is None or widest < MIN_VALLEY:
        return default, None
    mid = (at[0] + at[1]) / 2
    # A threshold above the typical gap would call most inferences "the same turn", which
    # is the failure this whole reconstruction exists to avoid.
    ordered = sorted(gaps)
    median = ordered[len(ordered) // 2]
    if mid >= median:
        return default, None
    return mid, at

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
