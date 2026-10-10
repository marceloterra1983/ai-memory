"""Loopback MCP double that speaks the frozen backend interface."""

from __future__ import annotations

import json
import os
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def _tool(rpc_id, payload, is_error=False):
    return {
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "content": [{"type": "text", "text": json.dumps(payload)}],
            "isError": is_error,
        },
    }


def _rpc_error(rpc_id, code, message):
    return {"jsonrpc": "2.0", "id": rpc_id, "error": {"code": code, "message": message}}


class Store:
    def __init__(self):
        self.lock = threading.Lock()
        self.pages = {}
        self.write_count = 0
        self.cas_advertised = True
        self.health_only = False
        self.mcp_status = 200
        self.mcp_mode = "ok"
        self.status_mode = "ok"
        self.status_lie = None
        self.drop_write_response = False
        self.fail_reads_after_write = 0
        self.allow_write = threading.Event()
        self.allow_write.set()
        self.write_entered = threading.Event()
        self.sleep_paths = {}
        self.known_scopes = {("demo", "demo")}
        self.captured = []
        self.outbox = None
        self.durable_before_write = False
        self.seen_bad_cas = False
        self.redirects = {}

    def force_put(self, workspace, project, path, body, **meta):
        title = meta.get("title") or _title(body, path)
        page = {
            "page_id": str(uuid.uuid4()),
            "body": body,
            "title": title,
            "tier": meta.get("tier", "semantic"),
            "pinned": meta.get("pinned", False),
            "tags": list(meta.get("tags", [])),
            "expires_at": meta.get("expires_at"),
        }
        with self.lock:
            self.pages[(workspace, project, path)] = page
            self.known_scopes.add((workspace, project))
        return page


def _title(body, path):
    for line in body.splitlines():
        stripped = line.strip()
        if stripped.startswith("# "):
            return stripped[2:].strip()
    stem = path.rsplit("/", 1)[-1]
    return stem[:-3] if stem.endswith(".md") else stem


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        return

    @property
    def store(self) -> Store:
        return self.server.store

    def do_GET(self):
        self._handle()

    def do_POST(self):
        self._handle()

    def do_PUT(self):
        self._handle()

    def do_DELETE(self):
        self._handle()

    def do_OPTIONS(self):
        self._handle()

    def _read(self):
        raw = self.headers.get("Content-Length")
        if raw is None:
            return b""
        return self.rfile.read(int(raw))

    def _send(self, status, body, content_type="application/json", extra=None):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        for key, value in extra or []:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def _drop(self):
        self.close_connection = True
        try:
            self.connection.shutdown(__import__("socket").SHUT_RDWR)
        except OSError:
            pass

    def _capture(self, body):
        headers = {k.lower(): v for k, v in self.headers.items()}
        self.store.captured.append(
            {
                "method": self.command,
                "path": self.path,
                "headers": headers,
                "body": body,
            }
        )

    def _handle(self):
        path = self.path.split("?", 1)[0]
        delay = self.store.sleep_paths.get(path, 0)
        if delay:
            __import__("time").sleep(delay)
        body = self._read()
        self._capture(body)
        if path in self.store.redirects:
            location = self.store.redirects[path]
            self._send(302, b"", extra=[("Location", location)])
            return
        if path == "/healthz":
            self._send(200, json.dumps({"status": "ok"}))
            return
        if path == "/hook":
            self._send(202, json.dumps({"accepted": True, "marker": "hook-ok"}))
            return
        if path == "/slow":
            self._send(200, json.dumps({"slow": True}))
            return
        if path != "/mcp":
            self._send(404, json.dumps({"error": "no such route"}))
            return
        if self.store.health_only or self.store.mcp_status != 200:
            self._send(self.store.mcp_status if self.store.mcp_status != 200 else 500, b"nope")
            return
        if self.store.mcp_mode == "http_500":
            self._send(500, b"down")
            return
        if self.store.mcp_mode == "http_405":
            self._send(405, b"nope")
            return
        if self.store.mcp_mode == "malformed":
            self._send(200, b'{"jsonrpc":')
            return
        try:
            message = json.loads(body.decode() or "null")
        except json.JSONDecodeError:
            self._send(400, json.dumps(_rpc_error(None, -32700, "parse error")))
            return
        if not isinstance(message, dict):
            self._send(400, json.dumps(_rpc_error(None, -32600, "batch unsupported")))
            return
        method = message.get("method")
        rpc_id = message.get("id", None)
        if isinstance(method, str) and method.startswith("notifications/"):
            self._send(202, b"", content_type="text/plain")
            return
        if method == "initialize":
            self._send(
                200,
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "fake-memory", "version": "0"},
                        },
                    }
                ),
            )
            return
        if method == "tools/list":
            self._send(200, json.dumps({"jsonrpc": "2.0", "id": rpc_id, "result": {"tools": self._tools()}}))
            return
        if method != "tools/call":
            self._send(200, json.dumps(_rpc_error(rpc_id, -32601, "unknown method")))
            return
        params = message.get("params") or {}
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "memory_status":
            self._status(rpc_id, args)
            return
        if name == "memory_read_page":
            self._read_page(rpc_id, args)
            return
        if name == "memory_write_page":
            self._write_page(rpc_id, args)
            return
        if name == "memory_query":
            self._send(200, json.dumps(_tool(rpc_id, {"hits": ["QUERY-HIT"]})))
            return
        self._send(200, json.dumps(_rpc_error(rpc_id, -32602, "unknown tool")))

    def _tools(self):
        props = {
            "path": {"type": "string"},
            "body": {"type": "string"},
            "tier": {"type": "string"},
            "pinned": {"type": "boolean"},
            "tags": {"type": "array"},
            "title": {"type": "string"},
            "expires_at": {"type": "string"},
            "workspace": {"type": "string"},
            "project": {"type": "string"},
            "scope": {"type": "string"},
        }
        if self.store.cas_advertised:
            props["create_only"] = {"type": "boolean"}
            props["expected_page_id"] = {"type": "string"}
        return [
            {
                "name": "memory_write_page",
                "description": "CAS-MARKER write a page",
                "inputSchema": {"type": "object", "properties": props},
            },
            {
                "name": "memory_read_page",
                "description": "read a page",
                "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}},
            },
            {
                "name": "memory_status",
                "description": "status",
                "inputSchema": {"type": "object", "properties": {"workspace": {}, "project": {}}},
            },
            {
                "name": "memory_query",
                "description": "query",
                "inputSchema": {"type": "object", "properties": {"query": {}}},
            },
        ]

    def _status(self, rpc_id, args):
        mode = self.store.status_mode
        if mode == "rpc_error":
            self._send(200, json.dumps(_rpc_error(rpc_id, -32603, "status failed")))
            return
        if mode == "is_error":
            self._send(
                200,
                json.dumps(_tool(rpc_id, {"counts": {"pages_latest": 1}}, is_error=True)),
            )
            return
        if mode == "http_500":
            self._send(500, b"status down")
            return
        if mode == "http_405":
            self._send(405, b"nope")
            return
        if mode == "malformed":
            self._send(200, b'{"jsonrpc":')
            return
        workspace = args.get("workspace")
        project = args.get("project")
        if self.store.status_lie:
            workspace, project = self.store.status_lie
            resolved = "default"
        else:
            resolved = "explicit"
        if not self.store.status_lie and (workspace, project) not in self.store.known_scopes:
            self._send(
                200,
                json.dumps(
                    _rpc_error(
                        rpc_id,
                        -32602,
                        f"project {project!r} not found in workspace {workspace!r}",
                    )
                ),
            )
            return
        with self.store.lock:
            count = len(self.store.pages)
        payload = {
            "counts": {
                "pages_latest": count,
                "pages_all": count,
                "sessions": 0,
                "observations": 0,
                "evidence_rows": 0,
            },
            "scope": {
                "workspace": workspace,
                "project": project,
                "resolved_by": resolved,
            },
        }
        self._send(200, json.dumps(_tool(rpc_id, payload)))

    def _scope_key(self, args):
        if args.get("scope") == "global":
            return "default", "_global"
        return args.get("workspace"), args.get("project")

    def _read_page(self, rpc_id, args):
        with self.store.lock:
            pending_fail = self.store.write_count > 0 and self.store.fail_reads_after_write > 0
            if pending_fail:
                self.store.fail_reads_after_write -= 1
        if pending_fail:
            self._send(500, b"read failed")
            return
        workspace, project = self._scope_key(args)
        path = args.get("path")
        with self.store.lock:
            page = self.store.pages.get((workspace, project, path))
        if page is None:
            self._send(
                200,
                json.dumps(
                    _rpc_error(
                        rpc_id,
                        -32602,
                        f"page {path} not found in resolved scope {workspace}/{project}",
                    )
                ),
            )
            return
        payload = {
            "page_id": page["page_id"],
            "path": path,
            "title": page["title"],
            "body": page["body"],
            "frontmatter": {
                "title": page["title"],
                "tier": page["tier"],
                "pinned": page["pinned"],
                "tags": page["tags"],
                "expires_at": page["expires_at"],
            },
        }
        self._send(200, json.dumps(_tool(rpc_id, payload)))

    def _write_page(self, rpc_id, args):
        if self.store.outbox and not self.store.durable_before_write:
            self.store.durable_before_write = _body_is_durable(self.store.outbox, args.get("body", ""))
        if not self.store.allow_write.is_set():
            self.store.write_entered.set()
            if not self.store.allow_write.wait(5):
                self._send(200, json.dumps(_rpc_error(rpc_id, -32603, "stalled write timed out")))
                return
        create_only = args.get("create_only") is True
        expected = args.get("expected_page_id")
        if create_only and expected or (not create_only and not expected):
            self.store.seen_bad_cas = True
            self._send(200, json.dumps(_rpc_error(rpc_id, -32602, "cas fields invalid")))
            return
        workspace, project = self._scope_key(args)
        path = args.get("path")
        key = (workspace, project, path)
        with self.store.lock:
            current = self.store.pages.get(key)
            if create_only and current is not None:
                self._send(200, json.dumps(_rpc_error(rpc_id, -32602, "create_only conflict")))
                return
            if not create_only and (current is None or current["page_id"] != expected):
                self._send(200, json.dumps(_rpc_error(rpc_id, -32602, "expected_page_id mismatch")))
                return
            title = args.get("title") or _title(args.get("body", ""), path or "page.md")
            page = {
                "page_id": str(uuid.uuid4()),
                "body": args.get("body", ""),
                "title": title,
                "tier": args.get("tier") or "semantic",
                "pinned": bool(args.get("pinned", False)),
                "tags": list(args.get("tags") or []),
                "expires_at": args.get("expires_at"),
            }
            self.store.pages[key] = page
            self.store.known_scopes.add((workspace, project))
            self.store.write_count += 1
        if self.store.drop_write_response:
            self.store.drop_write_response = False
            self._drop()
            return
        self._send(
            200,
            json.dumps(
                _tool(
                    rpc_id,
                    {"page_id": page["page_id"], "path": path, "checkpoint": "synth"},
                )
            ),
        )


def _body_is_durable(outbox, body):
    ops = Path(outbox) / "ops"
    if not ops.is_dir():
        return False
    for child in ops.iterdir():
        if child.is_symlink() or not child.is_dir():
            continue
        body_path = child / "body.md"
        intent_path = child / "intent.json"
        if not body_path.is_file() or not intent_path.is_file():
            continue
        if body_path.read_text(encoding="utf-8") != body:
            continue
        intent = json.loads(intent_path.read_text(encoding="utf-8"))
        if intent.get("status") == "pending":
            return True
    return False


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address, store):
        self.store = store
        super().__init__(address, Handler)

    def handle_error(self, request, client_address):
        return


class Backend:
    def __init__(self, store=None, port=0):
        self.store = store or Store()
        self.httpd = Server(("127.0.0.1", port), self.store)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.port}"

    def stop(self):
        httpd = self.httpd
        if httpd is None:
            return
        self.httpd = None
        httpd.shutdown()
        httpd.server_close()
        self.thread.join(2)

    def restart(self):
        port = self.port
        self.stop()
        self.httpd = Server(("127.0.0.1", port), self.store)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()


def free_port():
    import socket

    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port
