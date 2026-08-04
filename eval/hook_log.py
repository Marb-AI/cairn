#!/usr/bin/env python3
"""PreToolUse hook: one JSON line per subagent tool call, for counting round trips.

A round trip is one inference, not one tool call: several calls issued together in one
turn cost one inference and run concurrently. Nothing in the payload says which turn a
call belongs to, so the reconstruction is by timing - calls in the same turn arrive
milliseconds apart, calls in different turns are separated by an inference. `cluster.py`
does that grouping; this only records.

Keyed by `agent_id`, which the payload carries for subagent calls and omits for the
parent's own. That is what makes arms runnable in parallel: without it every concurrent
run interleaves into one file and none of them can be attributed. The parent's calls are
dropped by the same test, so no marker or bracketing is needed.

Writes nothing when no protocol is active, so in an ordinary session it costs one
file-existence check per tool call.

It must never raise. A PreToolUse hook that fails blocks the tool call, and a hook that
blocks every tool call locks the session out of fixing it - which is exactly how this
file came to be written before the hook that calls it was enabled.
"""

import json
import os
import sys
import time

ACTIVE = "/home/workspaces/cairn/cairn/eval/runs/ACTIVE"


def main() -> None:
    try:
        with open(ACTIVE) as fh:
            run_dir = fh.read().strip()
    except OSError:
        return
    if not run_dir:
        return

    try:
        payload = json.loads(sys.stdin.read() or "{}")
    except Exception:
        return

    agent = payload.get("agent_id")
    if not agent:
        # The parent's own call. Not part of any arm.
        return

    tool_input = payload.get("tool_input") or {}
    if not isinstance(tool_input, dict):
        tool_input = {}
    # Enough to audit which tool an arm actually reached for, and to catch an arm using
    # the one it was denied, without copying whole file bodies into the log.
    detail = (
        tool_input.get("command")
        or tool_input.get("file_path")
        or tool_input.get("pattern")
        or tool_input.get("path")
        or ""
    )
    record = {
        "t": round(time.time(), 3),
        "agent": agent,
        "tool": payload.get("tool_name", "?"),
        "detail": str(detail)[:400],
    }
    base = os.path.join(os.path.dirname(ACTIVE), run_dir)
    os.makedirs(base, exist_ok=True)
    with open(os.path.join(base, agent + ".jsonl"), "a") as fh:
        fh.write(json.dumps(record) + "\n")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        # Recording a run is never worth blocking the run.
        pass
