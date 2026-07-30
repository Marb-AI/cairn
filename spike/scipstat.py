#!/usr/bin/env python3
"""Statistics over a SCIP index — no dependencies, reads protobuf wire format directly.

Why a custom parser instead of `scip print --json`: a JSON dump of a large index is
hundreds of MB and would have to be streamed anyway. The wire format can be walked
linearly, skipping uninteresting chunks.

Numbers are cross-checked against `scip stats` (see run.sh) — if document counts
disagree, the field numbers below are wrong and the output is invalid.

The decisive metric is UNRESOLVED REFERENCES INTO THE PROJECT'S OWN MODULES.
Everything else (stdlib, third-party) is expected to dangle, because those
dependencies are not part of the index.
"""
import sys
import os
from collections import Counter

# --- field numbers from the SCIP schema (scip.proto) -------------------------
IDX_DOCUMENTS, IDX_EXTERNAL = 2, 3
DOC_PATH, DOC_OCCURRENCES, DOC_SYMBOLS, DOC_LANGUAGE = 1, 2, 3, 4
OCC_SYMBOL, OCC_ROLES = 2, 3
SYMINFO_SYMBOL = 1
ROLE_DEFINITION = 0x1


def read_varint(buf, i):
    result = shift = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, i
        shift += 7


def fields(buf, start=0, end=None):
    """Yield (field_number, wire_type, payload_or_value)."""
    end = len(buf) if end is None else end
    i = start
    while i < end:
        key, i = read_varint(buf, i)
        fnum, wtype = key >> 3, key & 0x7
        if wtype == 0:
            val, i = read_varint(buf, i)
            yield fnum, wtype, val
        elif wtype == 2:
            ln, i = read_varint(buf, i)
            yield fnum, wtype, buf[i:i + ln]
            i += ln
        elif wtype == 5:
            yield fnum, wtype, buf[i:i + 4]
            i += 4
        elif wtype == 1:
            yield fnum, wtype, buf[i:i + 8]
            i += 8
        else:
            raise ValueError(f"unknown wire type {wtype} at offset {i}")


def parse(path):
    with open(path, "rb") as fh:
        data = fh.read()

    docs = 0
    occ_total = occ_def = 0
    defined = set()          # symbols defined somewhere in the index
    external = set()         # symbols listed in external_symbols
    referenced = Counter()   # symbol -> number of reference occurrences
    per_file_occ = Counter()

    for fnum, wtype, payload in fields(data):
        if fnum == IDX_EXTERNAL and wtype == 2:
            for sf, sw, sp in fields(payload):
                if sf == SYMINFO_SYMBOL and sw == 2:
                    external.add(sp.decode("utf-8", "replace"))
        elif fnum == IDX_DOCUMENTS and wtype == 2:
            docs += 1
            relpath = "?"
            local_occ = 0
            for df, dw, dp in fields(payload):
                if df == DOC_PATH and dw == 2:
                    relpath = dp.decode("utf-8", "replace")
                elif df == DOC_SYMBOLS and dw == 2:
                    for sf, sw, sp in fields(dp):
                        if sf == SYMINFO_SYMBOL and sw == 2:
                            defined.add(sp.decode("utf-8", "replace"))
                elif df == DOC_OCCURRENCES and dw == 2:
                    sym, roles = None, 0
                    for of_, ow, op in fields(dp):
                        if of_ == OCC_SYMBOL and ow == 2:
                            sym = op.decode("utf-8", "replace")
                        elif of_ == OCC_ROLES and ow == 0:
                            roles = op
                    occ_total += 1
                    local_occ += 1
                    if sym:
                        if roles & ROLE_DEFINITION:
                            occ_def += 1
                            defined.add(sym)
                        else:
                            referenced[sym] += 1
            per_file_occ[relpath] = local_occ

    return dict(docs=docs, occ_total=occ_total, occ_def=occ_def, defined=defined,
                external=external, referenced=referenced, per_file_occ=per_file_occ)


GENERATED_HINTS = ("_pb2.py", "_pb2_grpc.py", ".pb.go", "_grpc.pb.go", "pb2.pyi")


def is_generated(path):
    return any(h in path for h in GENERATED_HINTS)


def sym_tail(symbol):
    """Descriptor part of a SCIP symbol: '<scheme> <mgr> <pkg> <ver> <descriptor>'."""
    parts = symbol.split(" ", 4)
    return parts[4] if len(parts) > 4 else ""


def module_root(symbol):
    """Top-level module of a symbol.

    Python descriptors carry dotted module paths (`domains.orders.repository`/x),
    Go descriptors carry slash paths. Take the part before the first '/', strip
    backticks, then the first dotted component — so both
    `domains.orders.repository`/chat# and domains/orders/... yield 'domains'.
    """
    tail = sym_tail(symbol)
    if not tail:
        return ""
    head = tail.split("/")[0].strip("`")
    return head.split(".")[0]


def project_roots(per_file_occ):
    """Top-level package directories of the project, taken from indexed documents."""
    roots = set()
    for path_ in per_file_occ:
        head = path_.split("/")[0]
        if head and "." not in head:
            roots.add(head)
    return roots


def sym_package(symbol):
    parts = symbol.split(" ", 4)
    return parts[2] if len(parts) >= 4 else ""


def make_classifier(s):
    """Return (is_project(symbol), group(symbol), how) — per language.

    The two indexers name symbols differently enough that one rule does not fit:

    * scip-go puts the Go module path in the package field, so "is it ours" is an
      exact match on the module. Descriptor roots are useless here (both project
      and third-party symbols start with `github.com/`).
    * scip-python puts the project distribution name in the package field, but
      MISATTRIBUTES third-party packages to it as well. The descriptor's top-level
      module is reliable there, so match against the directories actually indexed.
    """
    sample = next(iter(s["defined"]), "")
    roots = project_roots(s["per_file_occ"])

    if sample.startswith("scip-go"):
        own_pkgs = {sym_package(x) for x in s["defined"]}
        own_pkgs.discard("")
        # Go module paths of the project = packages the index defines symbols in.
        return (lambda sym: sym_package(sym) in own_pkgs,
                lambda sym: sym_package(sym),
                "go: exact match on module path in the package field")

    return (lambda sym: module_root(sym) in roots,
            lambda sym: module_root(sym),
            "python: top-level module matched against indexed directories")


def report(name, s):
    print(f"\n{'=' * 72}\n{name}\n{'=' * 72}")
    print(f"documents             {s['docs']:>10,}")
    print(f"occurrences           {s['occ_total']:>10,}")
    print(f"  definitions         {s['occ_def']:>10,}")
    print(f"symbols defined       {len(s['defined']):>10,}")
    print(f"symbols external      {len(s['external']):>10,}")

    resolvable = s["defined"] | s["external"]
    is_project, group_of, how = make_classifier(s)

    print(f"\n-- PROJECT CODE --   ({how})")
    print(f"  {'module':<34}{'references':>12}{'unresolved':>12}{'%':>8}")
    tot_r = tot_d = 0
    missing = Counter()
    groups = Counter()
    for sym, n in s["referenced"].items():
        if sym.startswith("local ") or not is_project(sym):
            continue
        groups[group_of(sym)] += n
    for root in sorted(groups):
        r = d = 0
        for sym, n in s["referenced"].items():
            if sym.startswith("local ") or not is_project(sym) or group_of(sym) != root:
                continue
            r += n
            if sym not in resolvable:
                d += n
                missing[sym] += n
        if r:
            tot_r += r
            tot_d += d
            print(f"  {root[-34:]:<34}{r:>12,}{d:>12,}{100.0 * d / r:>7.2f}%")
    if tot_r:
        print(f"  {'TOTAL':<34}{tot_r:>12,}{tot_d:>12,}"
              f"{100.0 * tot_d / tot_r:>7.2f}%  <<< DECISIVE NUMBER")
    if missing:
        print("\n  unresolved project symbols (what cairn would fail to find):")
        for sym, n in missing.most_common(10):
            print(f"    {n:>6,}x  {sym_tail(sym)[:84]}")

    # Everything outside the project roots: stdlib and third-party. Dangling here
    # is expected — those dependencies are simply not indexed.
    ext_r = ext_d = 0
    ext_by_root = Counter()
    for sym, n in s["referenced"].items():
        if sym.startswith("local ") or is_project(sym):
            continue
        ext_r += n
        if sym not in resolvable:
            ext_d += n
            ext_by_root[module_root(sym)] += n
    print("\n-- OUTSIDE THE PROJECT (stdlib / third-party, dangling is expected) --")
    print(f"  references          {ext_r:>10,}")
    print(f"  without definition  {ext_d:>10,}")
    for root, n in ext_by_root.most_common(8):
        print(f"    {root[:44]:<46} {n:>8,}")

    gen_occ = sum(n for p, n in s["per_file_occ"].items() if is_generated(p))
    gen_docs = sum(1 for p in s["per_file_occ"] if is_generated(p))
    if s["occ_total"]:
        print(f"\ngenerated code        {gen_docs:>10,} documents, "
              f"{gen_occ:,} occurrences ({100.0 * gen_occ / s['occ_total']:.1f} %)")

    thin = [(p_, n) for p_, n in s["per_file_occ"].items()
            if not is_generated(p_) and n < 5]
    print(f"files with <5 occ.    {len(thin):>10,}   (candidates for silent failure)")

    return dict(pct=(100.0 * tot_d / tot_r) if tot_r else 0.0, refs=tot_r,
                unresolved=tot_d, docs=s["docs"], occ=s["occ_total"])


def main():
    if len(sys.argv) < 2:
        print("usage: scipstat.py <index.scip> [<index.scip> ...]", file=sys.stderr)
        return 2
    results = {}
    for path in sys.argv[1:]:
        if not os.path.exists(path):
            print(f"\n!! missing {path} — indexing probably failed", file=sys.stderr)
            continue
        size = os.path.getsize(path)
        r = report(f"{path}   ({size / 1e6:.1f} MB raw)", parse(path))
        r["bytes"] = size
        results[path] = r

    if len(results) > 1:
        print(f"\n{'=' * 72}\nSUMMARY\n{'=' * 72}")
        print(f"{'index':<24}{'MB':>7}{'docs':>8}{'occurrences':>13}"
              f"{'project refs':>14}{'unresolved':>12}")
        for path, r in results.items():
            print(f"{os.path.basename(path):<24}{r['bytes'] / 1e6:>7.1f}{r['docs']:>8,}"
                  f"{r['occ']:>13,}{r['refs']:>14,}{r['pct']:>11.2f}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
