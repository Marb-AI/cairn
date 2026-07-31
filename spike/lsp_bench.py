#!/usr/bin/env python3
"""Measure the LSP hot path: how long a warm language server takes to answer about
one changed file.

Architecture section 4.2 assumes 10-100 ms for this, and the whole dirty-overlay design
leans on it. That number was never measured - phase 0 timed the batch indexers, not the
servers - so this settles it before anything is built on top.

What is measured, in order, because each stage answers a different question:

  initialize + warm-up   one-off cost the daemon pays at startup, not per query
  documentSymbol         cheapest useful request: "what is in this file"
  references             the request the tool actually needs, and the expensive one
  didChange -> repeat    the real hot path: file edited, ask again

Timings are wall clock around the request/response pair, so they include the server's
re-analysis. That is the number that matters; anything else flatters the result.
"""
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path


class LspClient:
    def __init__(self, cmd, cwd, env=None):
        self.proc = subprocess.Popen(
            cmd,
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env={**os.environ, **(env or {})},
        )
        self.next_id = 1
        self.responses = {}
        self.lock = threading.Lock()
        self.event = threading.Condition(self.lock)
        self.notifications = []
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    def _read_loop(self):
        stream = self.proc.stdout
        while True:
            headers = {}
            while True:
                line = stream.readline()
                if not line:
                    return
                line = line.decode("utf-8", "replace").strip()
                if not line:
                    break
                if ":" in line:
                    k, v = line.split(":", 1)
                    headers[k.strip().lower()] = v.strip()
            n = int(headers.get("content-length", 0))
            if not n:
                continue
            body = stream.read(n)
            try:
                msg = json.loads(body)
            except Exception:
                continue
            # A server-initiated request must be answered or the server blocks.
            # pyright asks for workspace/configuration during start-up and will not
            # serve a single documentSymbol until it gets a reply - ignoring these is
            # why the first version of this harness "measured" a 180 s timeout.
            if "method" in msg and "id" in msg:
                self._answer_server_request(msg)
                continue
            with self.event:
                if "id" in msg and ("result" in msg or "error" in msg):
                    self.responses[msg["id"]] = msg
                else:
                    self.notifications.append(msg)
                self.event.notify_all()

    def _answer_server_request(self, msg):
        method = msg.get("method", "")
        if method == "workspace/configuration":
            # One settings object per requested item; empty means "defaults".
            items = (msg.get("params") or {}).get("items") or [{}]
            result = [{} for _ in items]
        elif method == "workspace/workspaceFolders":
            result = None
        else:
            # registerCapability, workDoneProgress/create and friends: acknowledging is
            # all that is required.
            result = None
        self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

    def _send(self, payload):
        body = json.dumps(payload).encode("utf-8")
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.proc.stdin.flush()

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout=180):
        with self.lock:
            rid = self.next_id
            self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.time() + timeout
        with self.event:
            while rid not in self.responses:
                if not self.event.wait(timeout=max(0.05, deadline - time.time())):
                    if time.time() > deadline:
                        raise TimeoutError(f"{method} timed out after {timeout}s")
            return self.responses.pop(rid)

    def close(self):
        try:
            self.request("shutdown", None, timeout=10)
            self.notify("exit", None)
        except Exception:
            pass
        self.proc.terminate()


def uri(path):
    return "file://" + str(path)


def initialize(client, root):
    return client.request(
        "initialize",
        {
            "processId": os.getpid(),
            "rootUri": uri(root),
            "workspaceFolders": [{"uri": uri(root), "name": "w"}],
            "capabilities": {
                "textDocument": {
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
                    "references": {},
                    "synchronization": {"didSave": True, "dynamicRegistration": False},
                },
                "workspace": {"workspaceFolders": True, "configuration": True},
                "window": {"workDoneProgress": True},
            },
        },
    )


def timed(label, fn):
    t0 = time.perf_counter()
    try:
        result = fn()
    except Exception as e:
        print(f"    {label:<34} FAILED  {type(e).__name__}: {e}")
        return None, None
    ms = (time.perf_counter() - t0) * 1000
    return result, ms


def count(result):
    if not result:
        return 0
    r = result.get("result")
    if isinstance(r, list):
        return len(r)
    return 0 if r is None else 1


def bench(name, cmd, root, rel_file, language_id, warm_wait):
    print(f"\n{'=' * 70}\n{name}\n{'=' * 70}")
    path = Path(root) / rel_file
    if not path.exists():
        print(f"  missing {path}")
        return
    text = path.read_text()
    client = LspClient(cmd, cwd=root)

    _, ms = timed("initialize", lambda: initialize(client, root))
    print(f"    {'initialize':<34} {ms:8.0f} ms")
    client.notify("initialized", {})

    client.notify(
        "textDocument/didOpen",
        {
            "textDocument": {
                "uri": uri(path),
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        },
    )
    # The server indexes the workspace in the background; measuring before it settles
    # would measure the warm-up, not the hot path.
    print(f"    {'warm-up wait':<34} {warm_wait * 1000:8.0f} ms (fixed)")
    time.sleep(warm_wait)

    doc = {"textDocument": {"uri": uri(path)}}
    res, ms = timed("documentSymbol (cold)", lambda: client.request("textDocument/documentSymbol", doc))
    if ms is not None:
        print(f"    {'documentSymbol (first)':<34} {ms:8.1f} ms   {count(res)} symbols")

    # Repeat: the second call is the steady state a daemon would see.
    for i in range(3):
        res, ms = timed("documentSymbol", lambda: client.request("textDocument/documentSymbol", doc))
        if ms is not None:
            print(f"    {'documentSymbol (warm)':<34} {ms:8.1f} ms")

    # Find a symbol with a body to ask references about.
    target = None
    for s in (res or {}).get("result", []) or []:
        rng = s.get("selectionRange") or s.get("range") or s.get("location", {}).get("range")
        if rng:
            target = rng["start"]
            break
    if target:
        ref_params = {
            **doc,
            "position": target,
            "context": {"includeDeclaration": False},
        }
        for _ in range(2):
            res, ms = timed("references", lambda: client.request("textDocument/references", ref_params))
            if ms is not None:
                print(f"    {'references':<34} {ms:8.1f} ms   {count(res)} refs")

    # The actual hot path: edit the file, then ask again.
    changed = text + "\n# cairn benchmark edit\n"
    for round_no in (1, 2):
        t0 = time.perf_counter()
        client.notify(
            "textDocument/didChange",
            {
                "textDocument": {"uri": uri(path), "version": round_no + 1},
                "contentChanges": [{"text": changed + f"# {round_no}\n"}],
            },
        )
        res = client.request("textDocument/documentSymbol", doc)
        ms = (time.perf_counter() - t0) * 1000
        print(f"    {'didChange + documentSymbol':<34} {ms:8.1f} ms   {count(res)} symbols")
        if target:
            t0 = time.perf_counter()
            client.request("textDocument/references", ref_params)
            ms = (time.perf_counter() - t0) * 1000
            print(f"    {'didChange + references':<34} {ms:8.1f} ms")

    client.close()


if __name__ == "__main__":
    repo = sys.argv[1] if len(sys.argv) > 1 else "/work"
    bench(
        "pyright-langserver  (Python, 1169 files)",
        ["pyright-langserver", "--stdio"],
        f"{repo}/srcpy",
        "domains/orders/grpc/handlers/order.py",
        "python",
        warm_wait=float(os.environ.get("PY_WARM", "45")),
    )
    bench(
        "gopls  (Go, 99 packages)",
        ["gopls", "-mode=stdio"],
        f"{repo}/srcgo",
        "domains/orders/cmd/grpcserver/server.go",
        "go",
        warm_wait=float(os.environ.get("GO_WARM", "45")),
    )
