#!/usr/bin/env python3
"""Turn counts per run, from the agent-keyed logs plus MAP.tsv.

Usage: score.py <run-dir>

The grep arm is only re-run when its own preamble changes, so later rounds hold cairn
runs only and borrow the grep medians from the round that produced them. Naming that
borrowing here keeps it visible instead of hidden in a copy of this file."""
import json, os, statistics as st, sys
BASE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                    sys.argv[1] if len(sys.argv) > 1 else "runs/full")
GAP, PRE = 1.0, "eval/arms/"
def turns(agent):
    p = os.path.join(BASE, agent + ".jsonl")
    if not os.path.exists(p): return None, None, None
    rs = [json.loads(l) for l in open(p) if l.strip()]
    rs = sorted([r for r in rs if PRE not in r["detail"]], key=lambda r: r["t"])
    if not rs: return None, None, None
    n, widest = 1, 0.0
    for a, b in zip(rs, rs[1:]):
        d = b["t"] - a["t"]
        if d > GAP: n += 1
        else: widest = max(widest, d)
    return n, len(rs), widest
# Grep medians from the full protocol (round one), for rounds that re-run cairn alone.
GREP = {"s01": [4, 4, 5], "s02": [6, 7, 9], "s03": [7, 8, 11], "s04": [9, 10, 11],
        "s05": [11, 14, 18], "s06": [15, 15, 15], "s07": [4, 4, 4], "s08": [4, 4, 5],
        "s09": [3, 3, 3], "s10": [4, 5, 8]}
# Scenarios whose question was reworded on 2026-08-05. The numbers above were measured
# against the old wording, so borrowing them here would compare two different questions and
# call it a ratio - silently, which is the whole failure this file exists to avoid. Both
# arms must be re-run for these; there is nothing to fall back to.
REWORDED = {"s01", "s04", "s09"}
data = {}
for line in open(os.path.join(BASE, "MAP.tsv")):
    if not line.strip(): continue
    agent, sc, arm, rep = line.split()
    t, c, w = turns(agent)
    if t is None: print("MISSING", agent, sc, arm, rep); continue
    data.setdefault(sc, {}).setdefault(arm, []).append(t)
    if w > 0.6: print("  ! %s %s r%s widest within-turn %.2fs"%(sc,arm,rep,w))
print("%-5s %-18s %-18s %s"%("","cairn (3 runs)","grep (3 runs)","median ratio"))
tc=tg=0
for sc in sorted(data):
    c, g = sorted(data[sc].get("cairn",[])), sorted(data[sc].get("grep",[]))
    if len(c) < 3:
        print("%-5s %-18s incomplete" % (sc, c))
        continue
    if len(g) < 3:
        if sc in REWORDED:
            print("%-5s %-18s grep incomplete (%d/3) and NOT borrowable - reworded "
                  "2026-08-05, round-one numbers are a different question" % (sc, c, len(g)))
            continue
        g = sorted(GREP.get(sc, []))
    mc, mg = st.median(c), st.median(g)
    tc+=mc; tg+=mg
    print("%-5s %-18s %-18s %.2f"%(sc, "%s med %g"%(c,mc), "%s med %g"%(g,mg), mc/mg))
if tc: print("\nsum of medians: cairn %g  grep %g  ratio %.2f"%(tc,tg,tc/tg))
