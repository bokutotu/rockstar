#!/usr/bin/env python3
"""Deterministic stdio LSP peer for integration tests; never used by the app.

The real transport, document sync, native widgets and key routing remain in use.
Configuration and logs are confined to the temporary project supplied by tests.
"""
import json
import re
import sys
import threading
import time
from pathlib import Path

root = Path.cwd()
lock = threading.Lock()
documents = {}


def options():
    path = root / "mock-options.json"
    return json.loads(path.read_text()) if path.exists() else {}


def send(value):
    body = json.dumps({"jsonrpc": "2.0", **value}, ensure_ascii=False).encode()
    with lock:
        sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        sys.stdout.buffer.flush()


def position(text, offset):
    prefix = text[:offset]
    return {"line": prefix.count("\n"), "character": len(prefix.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2}


def offset(text, pos):
    lines = text.splitlines(keepends=True)
    start = sum(map(len, lines[:pos["line"]]))
    column = pos["character"]
    line = "".join(lines[pos["line"]:])
    used = 0
    for index, char in enumerate(line):
        if used == column:
            return start + index
        used += len(char.encode("utf-16-le")) // 2
    return len(text)


def span(text, start, end):
    return {"start": position(text, start), "end": position(text, end)}


def location(name="definition.rs"):
    return {"uri": (root / name).as_uri(), "range": span("fn found() {}", 3, 8)}


def item(name, uri):
    return {"name": name, "kind": 12, "uri": uri, "range": span("fn found() {}", 0, 13), "selectionRange": span("fn found() {}", 3, 8), "data": {"name": name}}


def diagnostics(uri):
    text, version = documents[uri]
    match = re.search("BROKEN", text)
    values = [{"range": span(text, *match.span()), "severity": 1, "message": "mock diagnostic", "source": "mock"}] if match else []
    send({"method": "textDocument/publishDiagnostics", "params": {"uri": uri, "version": version, "diagnostics": values}})


def handle(message):
    method = message.get("method")
    if method == "test/crash":
        sys.stderr.write("intentional mock server crash\n")
        sys.exit(9)
    if method == "test/error":
        send({"id": message["id"], "error": {"code": -32602, "message": "intentional error"}})
        return
    params = message.get("params") or {}
    opts = options()
    uri = params.get("textDocument", {}).get("uri", "")
    text = documents.get(uri, ("", 0))[0]
    result = None
    if method == "initialize":
        caps = {"textDocumentSync": {"openClose": True, "change": opts.get("sync", 2), "save": {"includeText": True}}, "positionEncoding": "utf-16", "definitionProvider": True, "hoverProvider": True, "completionProvider": {"triggerCharacters": ["."]}, "documentHighlightProvider": True, "callHierarchyProvider": True, "inlayHintProvider": True, "codeLensProvider": {"resolveProvider": False}, "executeCommandProvider": {"commands": ["mock.references"]}, "semanticTokensProvider": {"legend": {"tokenTypes": ["type"], "tokenModifiers": []}, "full": True}}
        for name in opts.get("unsupported", []):
            caps.pop(name, None)
        time.sleep(opts.get("initialize_delay", 0))
        result = {"capabilities": caps}
    elif method == "textDocument/didOpen":
        doc = params["textDocument"]
        documents[uri] = (doc["text"], doc["version"])
        diagnostics(uri)
    elif method == "textDocument/didChange":
        for change in params["contentChanges"]:
            if "range" not in change:
                text = change["text"]
            else:
                r = change["range"]
                text = text[:offset(text, r["start"])] + change["text"] + text[offset(text, r["end"]):]
        documents[uri] = (text, params["textDocument"]["version"])
        diagnostics(uri)
    elif method == "textDocument/definition":
        result = [location()]
        if opts.get("multiple_definitions"):
            result.append(location("second.rs"))
    elif method == "textDocument/hover":
        result = {"contents": {"kind": "markdown", "value": "**LSP hover**\n\nA documented symbol."}}
    elif method == "textDocument/completion":
        end = offset(text, params["position"])
        match = re.search(r"[\w]+$", text[:end])
        start = match.start() if match else end
        completion = {"label": "completed", "textEdit": {"range": span(text, start, end), "newText": "completed"}}
        if opts.get("additional_edit"):
            completion["additionalTextEdits"] = [{"range": span(text, 0, 0), "newText": "// imported\n"}]
        result = [completion]
    elif method == "textDocument/documentHighlight":
        result = [{"range": span(text, *m.span()), "kind": 2} for m in re.finditer(r"\banswer\b", text)]
    elif method == "textDocument/prepareCallHierarchy":
        result = [item("root", uri)]
    elif method == "callHierarchy/incomingCalls":
        name = params["item"]["name"]
        result = [] if name != "root" else [{"from": item("caller", location()["uri"]), "fromRanges": [location()["range"]]}]
    elif method == "callHierarchy/outgoingCalls":
        name = params["item"]["name"]
        result = [] if name != "root" else [{"to": item("callee", location()["uri"]), "fromRanges": [location()["range"]]}]
    elif method == "textDocument/inlayHint":
        result = [{"position": position(text, m.end()), "label": ": i32", "paddingRight": True} for m in re.finditer(r"let answer", text)] if opts.get("annotations") else []
    elif method == "textDocument/codeLens":
        result = [{"range": span(text, 0, 0), "command": {"title": "1 reference", "command": "mock.references"}}] if text and opts.get("annotations") else []
    elif method.startswith("textDocument/semanticTokens"):
        result = {"data": []}
        if opts.get("annotations"):
            previous_line = previous_col = 0
            for match in re.finditer(r"\banswer\b", text):
                pos = position(text, match.start())
                result["data"].extend([pos["line"] - previous_line, pos["character"] - previous_col if pos["line"] == previous_line else pos["character"], 6, 0, 0])
                previous_line, previous_col = pos["line"], pos["character"]
    elif method == "workspace/executeCommand":
        result = [location()]
    elif method == "test/reverse":
        result = params["value"]
    if "id" in message:
        delay = opts.get("delay_definition", 0) if method == "textDocument/definition" else params.get("delay", 0)
        if delay:
            def later():
                time.sleep(delay)
                send({"id": message["id"], "result": result})
            threading.Thread(target=later, daemon=True).start()
        else:
            send({"id": message["id"], "result": result})


while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    with (root / "lsp-log.jsonl").open("a") as log:
        log.write(json.dumps(message, ensure_ascii=False) + "\n")
    if message.get("method") == "exit":
        sys.exit(0)
    handle(message)
    if message.get("method") == "initialize" and options().get("stall_after_initialize"):
        time.sleep(30)
