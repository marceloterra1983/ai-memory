"""JSON-RPC envelope and readiness contract. Synthetic data only.

rmcp accepts a request only when jsonrpc is the string "2.0" and id is a
string or an i64 (NumberOrString). Bool, null, float, and out-of-range
integers are not ids. Duplicate object keys are not last-key-wins.
"""

from __future__ import annotations

import json
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from reliable_mcp.protocol import cas_advertised, parse_message
from reliable_mcp.ready import probe
from reliable_mcp.relay import Relay
from tests.fake_backend import Backend

_I64_MAX = (1 << 63) - 1
_I64_MIN = -1 << 63
_WRITE_ARGUMENTS = {
    "path": "notes/protocol-negative.md",
    "body": "# Protocol negative\n",
    "workspace": "default",
    "project": "dev",
}


def _write_body(**envelope):
    message = {
        "method": "tools/call",
        "params": {"name": "memory_write_page", "arguments": dict(_WRITE_ARGUMENTS)},
    }
    message.update(envelope)
    return json.dumps(message).encode("utf-8")


def _ops(outbox: Path):
    ops = outbox / "ops"
    if not ops.is_dir():
        return []
    return [child for child in ops.iterdir() if child.is_dir()]


class _RelayCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.outbox = Path(self.tmp.name) / "delivery"
        self.backend = None
        self.relay = Relay(
            listen="127.0.0.1:0",
            backend="http://127.0.0.1:9",
            outbox=self.outbox,
            auto_drain_seconds=0,
            forward_timeout=0.4,
        )

    def tearDown(self):
        if self.backend is not None:
            self.backend.stop()
        self.tmp.cleanup()

    def _use_backend(self):
        self.backend = Backend()
        self.relay.backend_origin = self.backend.url
        self.backend.store.known_scopes.add(("default", "dev"))
        self.backend.store.known_scopes.add(("demo", "demo"))

    def _handle(self, raw: bytes):
        return self.relay.handle(
            "POST",
            "/mcp",
            [("Content-Type", "application/json")],
            raw,
        )

    def _assert_refused(self, raw: bytes, echoed_id=None):
        before = self.backend.store.write_count if self.backend is not None else 0
        captured = len(self.backend.store.captured) if self.backend is not None else 0
        status, _, payload = self._handle(raw)
        message = json.loads(payload.decode("utf-8"))
        self.assertEqual(status, 200)
        self.assertEqual(message["jsonrpc"], "2.0")
        self.assertEqual(message["error"]["code"], -32600)
        self.assertEqual(message["id"], echoed_id)
        self.assertNotIn("result", message)
        self.assertEqual(_ops(self.outbox), [])
        if self.backend is not None:
            self.assertEqual(self.backend.store.write_count, before)
            self.assertEqual(len(self.backend.store.captured), captured)
            self.assertEqual(self.backend.store.pages, {})


class EnvelopeRejection(_RelayCase):
    def test_proof_missing_version_and_id_does_not_enqueue(self):
        raw = json.dumps(
            {
                "method": "tools/call",
                "params": {"name": "memory_write_page", "arguments": dict(_WRITE_ARGUMENTS)},
            }
        ).encode("utf-8")
        self._assert_refused(raw, echoed_id=None)

    def test_missing_version_and_id_does_not_reach_a_live_backend(self):
        self._use_backend()
        self._assert_refused(_write_body(), echoed_id=None)
        self.assertEqual(self.backend.store.captured, [])

    def test_malformed_write_envelopes_do_not_enqueue(self):
        cases = [
            ("version-missing", _write_body(id=7), 7),
            ("version-wrong", _write_body(**{"jsonrpc": "1.0", "id": "sdk"}), "sdk"),
            ("version-number", _write_body(**{"jsonrpc": 2.0, "id": 1}), 1),
            ("id-missing", _write_body(**{"jsonrpc": "2.0"}), None),
            ("id-null", _write_body(**{"jsonrpc": "2.0", "id": None}), None),
            ("id-bool", _write_body(**{"jsonrpc": "2.0", "id": True}), None),
            ("id-float", _write_body(**{"jsonrpc": "2.0", "id": 1.5}), None),
            ("id-float-whole", b'{"jsonrpc":"2.0","id":1.0,"method":"tools/call","params":{"name":"memory_write_page","arguments":{"path":"notes/protocol-negative.md","body":"# Protocol negative\\n","workspace":"default","project":"dev"}}}', None),
            ("id-above-i64", _write_body(**{"jsonrpc": "2.0", "id": _I64_MAX + 1}), None),
            ("id-below-i64", _write_body(**{"jsonrpc": "2.0", "id": _I64_MIN - 1}), None),
            ("id-object", _write_body(**{"jsonrpc": "2.0", "id": {"n": 1}}), None),
            ("id-array", _write_body(**{"jsonrpc": "2.0", "id": [1]}), None),
        ]
        for name, raw, echoed in cases:
            with self.subTest(name=name):
                self._assert_refused(raw, echoed_id=echoed)

    def test_duplicate_keys_do_not_enqueue_or_rewrite(self):
        raw = (
            b'{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_write_page",'
            b'"arguments":{"path":"notes/a.md","path":"notes/protocol-negative.md",'
            b'"body":"# Protocol negative\\n","workspace":"default","project":"dev"}}}'
        )
        self.assertIsNone(parse_message(raw))
        self._assert_refused(raw, echoed_id=None)
        self._use_backend()
        self._assert_refused(raw, echoed_id=None)
        self.assertFalse(any(b"notes/protocol-negative.md" in item["body"] and item["body"] != raw for item in self.backend.store.captured))

    def test_duplicate_non_write_is_forwarded_unchanged(self):
        self._use_backend()
        raw = b'{"jsonrpc":"2.0","jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}'
        self.assertIsNone(parse_message(raw))
        status, _, payload = self._handle(raw)
        self.assertEqual(_ops(self.outbox), [])
        self.assertEqual(self.backend.store.write_count, 0)
        self.assertEqual(self.backend.store.captured[-1]["body"], raw)
        self.assertNotEqual(json.loads(payload.decode()), {"jsonrpc": "2.0", "id": 3, "method": "tools/list"})

    def test_valid_ids_still_queue_offline(self):
        for rpc_id in (0, -1, _I64_MIN, _I64_MAX, 2**53, "sdk-1", ""):
            with self.subTest(rpc_id=rpc_id):
                raw = _write_body(**{"jsonrpc": "2.0", "id": rpc_id})
                status, _, payload = self._handle(raw)
                message = json.loads(payload.decode())
                self.assertEqual(status, 200, message)
                self.assertNotIn("error", message)
                self.assertEqual(message["id"], rpc_id)
                self.assertEqual(message["jsonrpc"], "2.0")
                receipt = json.loads(message["result"]["content"][0]["text"])
                self.assertEqual(receipt["status"], "PENDING")
        # The id is not part of the intent hash, so these calls share one op.
        self.assertEqual(len(_ops(self.outbox)), 1)

    def test_live_sdk_request_keeps_its_id(self):
        self._use_backend()
        raw = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 0,
                "method": "tools/call",
                "params": {
                    "name": "memory_write_page",
                    "arguments": {
                        "path": "notes/sdk.md",
                        "body": "# Sdk\n\nbody",
                        "workspace": "demo",
                        "project": "demo",
                    },
                },
            }
        ).encode()
        status, _, payload = self._handle(raw)
        message = json.loads(payload.decode())
        self.assertEqual(status, 200)
        self.assertEqual(message["jsonrpc"], "2.0")
        self.assertEqual(message["id"], 0)
        self.assertNotIn("error", message)

    def test_malformed_notification_and_list_are_not_rewritten(self):
        self._use_backend()
        init = {
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}},
        }
        self._handle(json.dumps(init).encode())
        listed = {"jsonrpc": "2.0", "id": "list", "method": "tools/list", "params": {}}
        self._handle(json.dumps(listed).encode())
        note = b'{"method":"notifications/initialized"}'
        status, _, payload = self._handle(note)
        self.assertEqual(self.backend.store.captured[-1]["body"], note)
        self.assertNotIn(b'"id"', self.backend.store.captured[-1]["body"])
        self.assertEqual(_ops(self.outbox), [])
        bad_list = b'{"jsonrpc":"2.0","id":true,"method":"tools/list","params":{}}'
        self._handle(bad_list)
        self.assertEqual(self.backend.store.captured[-1]["body"], bad_list)
        self.backend.stop()
        self.backend = None
        cached_status, _, cached_payload = self._handle(json.dumps(listed).encode())
        cached = json.loads(cached_payload.decode())
        self.assertEqual(cached_status, 200)
        self.assertEqual(cached["id"], "list")
        self.assertIn("tools", cached["result"])
        down_status, _, down_payload = self._handle(bad_list)
        self.assertNotEqual(down_status, 200)
        self.assertNotIn(b"CAS-MARKER", down_payload)
        note_status, _, note_payload = self._handle(note)
        self.assertNotEqual(note_status, 202)
        self.assertNotIn(b"jsonrpc", note_payload)
        self.assertEqual(_ops(self.outbox), [])


class ReadinessEnvelope(unittest.TestCase):
    def setUp(self):
        self.httpd = None

    def tearDown(self):
        if self.httpd is not None:
            self.httpd.shutdown()
            self.httpd.server_close()

    def _serve(self, reply):
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), _ReadyHandler)
        httpd.reply = reply
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        self.httpd = httpd
        return f"http://127.0.0.1:{httpd.server_address[1]}"

    def test_wrong_id_or_version_is_not_ready(self):
        cases = ("init-id", "init-version", "tools-id", "tools-version", "status-id", "status-version")
        for mode in cases:
            with self.subTest(mode=mode):
                url = self._serve(lambda message, mode=mode: _ready_reply(message, mode))
                ok, reason = probe(url, "demo", "demo", per_call=1.0, budget=4.0)
                self.assertFalse(ok, reason)
                self.httpd.shutdown()
                self.httpd.server_close()
                self.httpd = None

    def test_error_is_error_and_bad_scope_stay_failed(self):
        for mode in ("status-error", "status-is-error", "bad-scope"):
            with self.subTest(mode=mode):
                url = self._serve(lambda message, mode=mode: _ready_reply(message, mode))
                ok, _reason = probe(url, "demo", "demo", per_call=1.0, budget=4.0)
                self.assertFalse(ok)
                self.httpd.shutdown()
                self.httpd.server_close()
                self.httpd = None

    def test_typed_backend_schema_is_ready_and_presence_only_is_not(self):
        url = self._serve(lambda message: _ready_reply(message, "ok"))
        ok, reason = probe(url, "demo", "demo", per_call=1.0, budget=4.0)
        self.assertTrue(ok, reason)
        self.httpd.shutdown()
        self.httpd.server_close()
        self.httpd = None
        url = self._serve(lambda message: _ready_reply(message, "presence"))
        ok, reason = probe(url, "demo", "demo", per_call=1.0, budget=4.0)
        self.assertFalse(ok)
        self.assertIn("create_only", reason)

    def test_fake_backend_stays_ready(self):
        backend = Backend()
        try:
            ok, reason = probe(backend.url, "demo", "demo")
            self.assertTrue(ok, reason)
            backend.store.status_mode = "rpc_error"
            ok, reason = probe(backend.url, "demo", "demo")
            self.assertFalse(ok)
            self.assertIn("memory_status", reason)
        finally:
            backend.stop()


class CasSchema(unittest.TestCase):
    def test_typed_cas_fields_count_and_presence_does_not(self):
        self.assertFalse(_advertised({"create_only": {}, "expected_page_id": {}}))
        self.assertFalse(_advertised({"create_only": {"description": "bool"}, "expected_page_id": {"type": "string"}}))
        self.assertFalse(_advertised({"create_only": {"type": "boolean"}, "expected_page_id": {"type": "integer"}}))
        self.assertFalse(_advertised({"create_only": {"type": "string"}, "expected_page_id": {"type": "string"}}))
        self.assertTrue(_advertised({"create_only": {"type": "boolean"}, "expected_page_id": {"type": "string"}}))
        self.assertTrue(
            _advertised(
                {
                    "create_only": {"type": "boolean", "description": "create only"},
                    "expected_page_id": {"type": ["string", "null"], "description": "UUID"},
                }
            )
        )
        self.assertTrue(
            _advertised(
                {
                    "create_only": {"type": "boolean"},
                    "expected_page_id": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                }
            )
        )
        self.assertTrue(
            _advertised(
                {
                    "create_only": {"type": "boolean"},
                    "expected_page_id": {"type": "string", "nullable": True},
                }
            )
        )
        self.assertFalse(
            _advertised(
                {
                    "create_only": {"type": "boolean"},
                    "expected_page_id": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
                }
            )
        )


class ParseMessage(unittest.TestCase):
    def test_duplicate_keys_are_rejected_at_every_object(self):
        self.assertIsNone(parse_message(b'{"a":1,"a":2}'))
        self.assertIsNone(parse_message(b'{"params":{"path":"a","path":"b"}}'))
        self.assertEqual(parse_message(b'{"jsonrpc":"2.0","id":0}')["id"], 0)


def _advertised(properties):
    return cas_advertised(
        [{"name": "memory_write_page", "inputSchema": {"type": "object", "properties": properties}}]
    )


class _ReadyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        return

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or "0")
        message = json.loads(self.rfile.read(length).decode("utf-8"))
        body = json.dumps(self.server.reply(message)).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


def _ready_reply(message, mode):
    rpc_id = message.get("id")
    method = message.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "contract", "version": "0"},
        }
        if mode == "init-id":
            return _rpc("other", result)
        if mode == "init-version":
            return _rpc(rpc_id, result, version="1.0")
        return _rpc(rpc_id, result)
    if method == "tools/list":
        result = {"tools": _tools(mode == "presence")}
        if mode == "tools-id":
            return _rpc("other", result)
        if mode == "tools-version":
            return _rpc(rpc_id, result, version="1.0")
        return _rpc(rpc_id, result)
    if mode == "status-error":
        return {"jsonrpc": "2.0", "id": rpc_id, "error": {"code": -32602, "message": "nope"}}
    payload = {
        "counts": {"pages_latest": 1},
        "scope": {"workspace": "demo", "project": "demo", "resolved_by": "explicit"},
    }
    if mode == "bad-scope":
        payload["scope"] = {"workspace": "other", "project": "demo", "resolved_by": "default"}
    result = {
        "content": [{"type": "text", "text": json.dumps(payload)}],
        "isError": mode == "status-is-error",
    }
    if mode == "status-id":
        return _rpc("other", result)
    if mode == "status-version":
        return _rpc(rpc_id, result, version="1.0")
    return _rpc(rpc_id, result)


def _tools(presence_only):
    if presence_only:
        write_props = {"create_only": {}, "expected_page_id": {"description": "present"}}
    else:
        write_props = {
            "create_only": {"type": "boolean"},
            "expected_page_id": {"type": ["string", "null"]},
        }
    return [
        {"name": "memory_write_page", "inputSchema": {"type": "object", "properties": write_props}},
        {"name": "memory_status", "inputSchema": {"type": "object", "properties": {}}},
    ]


def _rpc(rpc_id, result, version="2.0"):
    return {"jsonrpc": version, "id": rpc_id, "result": result}
