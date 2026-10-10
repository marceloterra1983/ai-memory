"""HTTP proofs for the reliable MCP ingress. Synthetic data only."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
import uuid
from pathlib import Path

from tests.fake_backend import Backend
from tests.http import file_mode, post_json, request, rpc, tool_text

ROOT = Path(__file__).resolve().parents[1]
INSTALLER = ROOT / "tests" / "fixtures" / "install-ai-memory-before-ready.sh"
SECRET_BODY = "SECRET-BODY-do-not-log"
SECRET_TOKEN = "Bearer SECRET-TOKEN-do-not-log"


def write_call(path, body, rpc_id=1, **arguments):
    args = {"path": path, "body": body, "workspace": "demo", "project": "demo"}
    args.update(arguments)
    return {
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": "tools/call",
        "params": {"name": "memory_write_page", "arguments": args},
    }


class IngressCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.outbox = Path(self.tmp.name) / "delivery"
        self.backend = Backend()
        self.backend.store.outbox = str(self.outbox)
        self.backend.store.known_scopes.add(("demo", "demo"))
        self.logs = []
        from reliable_mcp.relay import Relay

        self.relay = Relay(
            listen="127.0.0.1:0",
            backend=self.backend.url,
            outbox=self.outbox,
            auto_drain_seconds=0,
            log=self.logs.append,
        )
        self.relay.start()
        self.mcp = f"{self.relay.url}/mcp"

    def tearDown(self):
        self.relay.stop()
        self.backend.stop()
        self.tmp.cleanup()

    def assert_no_secrets(self, blob):
        text = blob if isinstance(blob, str) else json.dumps(blob)
        self.assertNotIn(SECRET_BODY, text)
        self.assertNotIn("SECRET-TOKEN-do-not-log", text)

    def test_offline_write_is_pending_without_fake_page_id(self):
        self.backend.stop()
        status, headers, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/offline.md",
                    "body": SECRET_BODY,
                    "workspace": "demo",
                    "project": "demo",
                },
            },
            rpc_id=7,
            headers={"Authorization": SECRET_TOKEN, "Origin": self.relay.url},
        )
        self.assertEqual(status, 200)
        self.assertIn("application/json", headers["content-type"])
        self.assertEqual(message["id"], 7)
        receipt = tool_text(message)
        self.assertEqual(receipt["status"], "PENDING")
        self.assertTrue(receipt["queued"])
        self.assertFalse(receipt["written"])
        self.assertIsNone(receipt["page_id"])
        self.assertNotIn("checkpoint", receipt)
        ops = list((self.outbox / "ops").iterdir())
        self.assertEqual(len(ops), 1)
        self.assertFalse(ops[0].is_symlink())
        self.assertEqual((ops[0] / "body.md").read_text(encoding="utf-8"), SECRET_BODY)
        intent = json.loads((ops[0] / "intent.json").read_text(encoding="utf-8"))
        self.assertEqual(intent["status"], "pending")
        self.assertEqual(intent["baseline"]["state"], "unknown")
        self.assertEqual(file_mode(self.outbox), 0o700)
        self.assertEqual(file_mode(ops[0] / "body.md"), 0o600)
        self.assertEqual(file_mode(ops[0] / "auth.secret"), 0o600)
        self.assert_no_secrets(self.logs)
        self.assert_no_secrets(intent)
        self.assertNotIn("SECRET-TOKEN", (ops[0] / "intent.json").read_text(encoding="utf-8"))

    def test_reload_then_backend_return_delivers_exact_body(self):
        self.backend.stop()
        body = "# Offline\n\nexact body\n"
        _, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/offline.md",
                    "body": body,
                    "workspace": "demo",
                    "project": "demo",
                    "tags": ["alpha"],
                },
            },
        )
        operation_id = tool_text(message)["operation_id"]
        self.relay.stop()
        self.backend = Backend(self.backend.store, port=self.backend.port)
        from reliable_mcp.relay import Relay

        restarted = Relay(
            listen="127.0.0.1:0",
            backend=self.backend.url,
            outbox=self.outbox,
            auto_drain_seconds=0,
        )
        restarted.start()
        self.relay = restarted
        delivered = restarted.drain()
        self.assertEqual(delivered, 1)
        intent = json.loads((self.outbox / "ops" / operation_id / "intent.json").read_text())
        self.assertEqual(intent["status"], "delivered")
        page = self.backend.store.pages[("demo", "demo", "notes/offline.md")]
        self.assertEqual(page["body"], body)
        self.assertEqual(page["tags"], ["alpha"])
        self.assertEqual(page["tier"], "semantic")
        self.assertFalse(page["pinned"])
        self.assertEqual(intent["page_id"], page["page_id"])
        self.assertEqual(self.backend.store.write_count, 1)
        writes = [item for item in self.backend.store.captured if _is_write(item)]
        arguments = json.loads(writes[-1]["body"].decode())["params"]["arguments"]
        self.assertTrue(arguments.get("create_only"))
        self.assertNotIn("expected_page_id", arguments)

    def test_lost_response_does_not_create_another_version(self):
        self.backend.store.drop_write_response = True
        self.backend.store.fail_reads_after_write = 1
        status, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/lost.md",
                    "body": "# Lost\n\nv1",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
        )
        self.assertEqual(status, 200)
        receipt = tool_text(message)
        self.assertEqual(receipt["status"], "PENDING")
        self.assertIsNone(receipt["page_id"])
        self.assertEqual(self.backend.store.write_count, 1)
        self.assertEqual(self.relay.drain(), 1)
        self.assertEqual(self.backend.store.write_count, 1)
        intent_path = next((self.outbox / "ops").iterdir()) / "intent.json"
        intent = json.loads(intent_path.read_text())
        page = self.backend.store.pages[("demo", "demo", "notes/lost.md")]
        self.assertEqual(intent["status"], "delivered")
        self.assertEqual(intent["page_id"], page["page_id"])

    def _restart_relay_while_backend_offline(self):
        from reliable_mcp.relay import Relay

        self.relay.stop()
        self.relay = Relay(
            listen="127.0.0.1:0",
            backend=self.backend.url,
            outbox=self.outbox,
            auto_drain_seconds=0,
        )
        self.relay.start()
        self.mcp = f"{self.relay.url}/mcp"

    def test_offline_expected_update_survives_restart_and_writes_once(self):
        path = "notes/offline-update.md"
        old = self.backend.store.force_put("demo", "demo", path, "# Old\n\nold")
        self.backend.stop()
        body = "# Updated\n\nexact update\n"
        metadata = {
            "title": "Offline update",
            "tier": "procedural",
            "pinned": True,
            "tags": ["offline", "cas"],
            "expires_at": "2027-01-01T00:00:00Z",
        }
        receipt = _write(self, body, path=path, expected_page_id=old["page_id"], **metadata)
        self.assertEqual(receipt["status"], "PENDING")
        self.assertIsNone(receipt["page_id"])
        intent_path = self.outbox / "ops" / receipt["operation_id"] / "intent.json"
        intent = json.loads(intent_path.read_text())
        guard = {"mode": "expected_page_id", "expected_page_id": old["page_id"]}
        self.assertEqual(intent["baseline"], {"state": "unknown"})
        self.assertEqual(intent["cas"], guard)
        self.assertEqual(intent["client_cas"], guard)
        self.assertFalse(intent.get("write_started"))
        self._restart_relay_while_backend_offline()
        self.backend.restart()

        self.assertEqual(self.relay.drain(), 1)
        intent = json.loads(intent_path.read_text())
        self.assertEqual(intent["status"], "delivered")
        self.assertTrue(intent["write_started"])
        page = self.backend.store.pages[("demo", "demo", path)]
        self.assertEqual(page["body"], body)
        for key, value in metadata.items():
            self.assertEqual(page[key], value)
        self.assertEqual(str(uuid.UUID(intent["page_id"])), page["page_id"])
        self.assertNotEqual(page["page_id"], old["page_id"])
        self.assertEqual(intent["checkpoint"], "synth")
        self.assertEqual(self.backend.store.write_count, 1)
        writes = [item for item in self.backend.store.captured if _is_write(item)]
        self.assertEqual(len(writes), 1)
        arguments = json.loads(writes[0]["body"])["params"]["arguments"]
        self.assertEqual(arguments["expected_page_id"], old["page_id"])
        self.assertNotIn("create_only", arguments)
        self.assertEqual(self.relay.drain(), 0)
        self.assertEqual(self.backend.store.write_count, 1)
        confirmed = _write(self, body, path=path, expected_page_id=old["page_id"], **metadata)
        self.assertEqual(confirmed["status"], "DELIVERED")
        self.assertEqual(confirmed["page_id"], page["page_id"])
        self.assertEqual(self.backend.store.write_count, 1)

    def test_offline_restart_refuses_unguarded_create_only_stale_and_missing_heads(self):
        body = "# Queued\n\nours"
        cases = [
            ("auto", {}, "# Other\n\ntheirs"),
            ("null", {"expected_page_id": None}, "# Other\n\ntheirs"),
            ("create", {"create_only": True}, body),
            ("stale", {"expected_page_id": str(uuid.uuid4())}, body),
            ("missing", {"expected_page_id": str(uuid.uuid4())}, None),
        ]
        self.backend.stop()
        operations = []
        for name, guard, current_body in cases:
            path = f"notes/offline-{name}.md"
            if name == "stale":
                old = self.backend.store.force_put("demo", "demo", path, "# Old\n\nold")
                guard = {"expected_page_id": old["page_id"]}
            receipt = _write(self, body, path=path, **guard)
            self.assertEqual(receipt["status"], "PENDING")
            current = None
            if current_body is not None:
                current = self.backend.store.force_put("demo", "demo", path, current_body)
            operations.append((name, path, receipt["operation_id"], current))
        self._restart_relay_while_backend_offline()
        self.backend.restart()

        self.assertEqual(self.relay.drain(), 0)
        for name, path, operation_id, current in operations:
            with self.subTest(guard=name):
                intent = json.loads((self.outbox / "ops" / operation_id / "intent.json").read_text())
                self.assertEqual(intent["status"], "conflict")
                self.assertEqual(intent["reason"], "divergent_head")
                self.assertIsNone(intent["page_id"])
                self.assertFalse(intent.get("write_started"))
                self.assertEqual(intent["current_head_page_id"], current["page_id"] if current else None)
                self.assertEqual(self.backend.store.pages.get(("demo", "demo", path)), current)
        self.assertEqual(self.backend.store.write_count, 0)
        self.assertFalse(any(_is_write(item) for item in self.backend.store.captured))
        self.assertEqual(self.relay.drain(), 0)

    def test_offline_expected_update_keeps_atomic_cas_after_restart(self):
        path = "notes/offline-cas-race.md"
        old = self.backend.store.force_put("demo", "demo", path, "# Old\n\nold")
        self.backend.stop()
        receipt = _write(self, "# New\n\nnew", path=path, expected_page_id=old["page_id"])
        self.assertEqual(receipt["status"], "PENDING")
        self._restart_relay_while_backend_offline()
        self.backend.restart()
        self.backend.store.allow_write.clear()
        result = {}

        def drain():
            result["delivered"] = self.relay.drain()

        thread = threading.Thread(target=drain)
        thread.start()
        try:
            self.assertTrue(self.backend.store.write_entered.wait(5))
            moved = self.backend.store.force_put("demo", "demo", path, "# Moved\n\nmoved")
        finally:
            self.backend.store.allow_write.set()
            thread.join(6)
        self.assertFalse(thread.is_alive())
        self.assertEqual(result["delivered"], 0)
        intent = json.loads((self.outbox / "ops" / receipt["operation_id"] / "intent.json").read_text())
        self.assertEqual(intent["status"], "conflict")
        self.assertTrue(intent["write_started"])
        self.assertEqual(intent["current_head_page_id"], moved["page_id"])
        self.assertEqual(self.backend.store.pages[("demo", "demo", path)], moved)
        self.assertEqual(self.backend.store.write_count, 0)

    def test_unknown_baseline_does_not_overwrite_divergent_head(self):
        self.backend.stop()
        _, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/race.md",
                    "body": "# Queued\n\nours",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
        )
        self.assertEqual(tool_text(message)["status"], "PENDING")
        self.backend = Backend(self.backend.store, port=self.backend.port)
        self.relay.backend_origin = self.backend.url
        from reliable_mcp.backend import Client

        self.relay.client = Client(self.backend.url, timeout=2)
        other = self.backend.store.force_put(
            "demo", "demo", "notes/race.md", "# Other\n\ntheirs", title="Other"
        )
        self.assertEqual(self.relay.drain(), 0)
        intent = json.loads((self.outbox / "ops" / tool_text(message)["operation_id"] / "intent.json").read_text())
        self.assertEqual(intent["status"], "conflict")
        self.assertIsNone(intent["page_id"])
        self.assertEqual(intent["current_head_page_id"], other["page_id"])
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/race.md")]["body"], "# Other\n\ntheirs")
        self.assertEqual(self.backend.store.write_count, 0)
        self.assertEqual(self.relay.drain(), 0)
        self.assertEqual(self.backend.store.write_count, 0)

    def test_online_cas_blocks_a_moved_head(self):
        original = self.backend.store.force_put("demo", "demo", "notes/live.md", "# Old\n\nold")
        self.backend.store.allow_write.clear()
        result = {}

        def call():
            result["value"] = rpc(
                self.mcp,
                "tools/call",
                {
                    "name": "memory_write_page",
                    "arguments": {
                        "path": "notes/live.md",
                        "body": "# New\n\nnew",
                        "workspace": "demo",
                        "project": "demo",
                    },
                },
                timeout=6,
            )

        thread = threading.Thread(target=call)
        thread.start()
        self.assertTrue(self.backend.store.write_entered.wait(5))
        moved = self.backend.store.force_put("demo", "demo", "notes/live.md", "# Moved\n\nmoved")
        self.backend.store.allow_write.set()
        thread.join(6)
        self.assertFalse(thread.is_alive())
        receipt = tool_text(result["value"][2])
        self.assertEqual(receipt["status"], "CONFLICT")
        self.assertIsNone(receipt["page_id"])
        self.assertEqual(receipt["current_head_page_id"], moved["page_id"])
        self.assertNotEqual(moved["page_id"], original["page_id"])
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/live.md")]["body"], "# Moved\n\nmoved")

    def test_metadata_changes_are_not_deduped_and_identical_state_is(self):
        body = "# Same\n\nbody"
        first = _write(self, body, tags=["a"])
        uuid.UUID(first["page_id"])
        second = _write(self, body, tags=["b"])
        self.assertNotEqual(first["page_id"], second["page_id"])
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/meta.md")]["tags"], ["b"])
        third = _write(self, body, tags=["b"])
        self.assertEqual(third["page_id"], second["page_id"])
        self.assertEqual(self.backend.store.write_count, 2)
        pinned = _write(self, body, tags=["b"], pinned=True)
        self.assertNotEqual(pinned["page_id"], second["page_id"])
        self.assertTrue(self.backend.store.pages[("demo", "demo", "notes/meta.md")]["pinned"])
        episodic = _write(self, body, tags=["b"], pinned=True, tier="episodic")
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/meta.md")]["tier"], "episodic")
        self.assertNotEqual(episodic["page_id"], pinned["page_id"])
        expiring = _write(
            self,
            body,
            tags=["b"],
            pinned=True,
            tier="episodic",
            expires_at="2026-12-01T00:00:00Z",
        )
        self.assertEqual(
            self.backend.store.pages[("demo", "demo", "notes/meta.md")]["expires_at"],
            "2026-12-01T00:00:00Z",
        )
        self.assertNotEqual(expiring["page_id"], episodic["page_id"])
        self.assertFalse(self.backend.store.seen_bad_cas)

    def test_missing_scope_is_refused_and_scopes_stay_isolated(self):
        status, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {"path": "notes/x.md", "body": "no scope"},
            },
        )
        self.assertEqual(status, 200)
        self.assertIn("error", message)
        self.assertEqual(message["error"]["code"], -32602)
        self.assertIn("scope", message["error"]["message"])
        self.assertEqual(self.backend.store.write_count, 0)
        self.assertEqual(list((self.outbox / "ops").iterdir()), [])
        partial, _, partial_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {"path": "notes/x.md", "body": "half", "project": "demo"},
            },
        )
        self.assertEqual(partial, 200)
        self.assertEqual(partial_message["error"]["code"], -32602)
        global_status, _, global_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/g.md",
                    "body": "# Global\n\npref",
                    "scope": "global",
                },
            },
        )
        self.assertEqual(global_status, 200)
        global_receipt = tool_text(global_message)
        uuid.UUID(global_receipt["page_id"])
        self.assertIn(("default", "_global", "notes/g.md"), self.backend.store.pages)
        self.assertNotIn(("demo", "demo", "notes/g.md"), self.backend.store.pages)
        bad_global, _, bad_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/g.md",
                    "body": "nope",
                    "scope": "global",
                    "project": "demo",
                },
            },
        )
        self.assertEqual(bad_global, 200)
        self.assertEqual(bad_message["error"]["code"], -32602)
        self.backend.store.known_scopes.add(("other", "other"))
        _write(self, "project body", path="notes/isolated.md", workspace="other", project="other")
        self.assertIn(("other", "other", "notes/isolated.md"), self.backend.store.pages)
        self.assertNotIn(("demo", "demo", "notes/isolated.md"), self.backend.store.pages)

    def test_client_cas_is_honored(self):
        self.backend.store.force_put("demo", "demo", "notes/exists.md", "# Exists\n\nold")
        status, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/exists.md",
                    "body": "# Exists\n\nnew",
                    "workspace": "demo",
                    "project": "demo",
                    "create_only": True,
                },
            },
        )
        self.assertEqual(status, 200)
        self.assertEqual(tool_text(message)["status"], "CONFLICT")
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/exists.md")]["body"], "# Exists\n\nold")
        both, _, both_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/both.md",
                    "body": "x",
                    "workspace": "demo",
                    "project": "demo",
                    "create_only": True,
                    "expected_page_id": str(uuid.uuid4()),
                },
            },
        )
        self.assertEqual(both, 200)
        self.assertEqual(both_message["error"]["code"], -32602)
        stale, _, stale_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/exists.md",
                    "body": "# Exists\n\nstale",
                    "workspace": "demo",
                    "project": "demo",
                    "expected_page_id": str(uuid.uuid4()),
                },
            },
        )
        self.assertEqual(tool_text(stale_message)["status"], "CONFLICT")
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/exists.md")]["body"], "# Exists\n\nold")
        created, _, created_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/fresh.md",
                    "body": "# Fresh\n\nyes",
                    "workspace": "demo",
                    "project": "demo",
                    "create_only": True,
                },
            },
        )
        uuid.UUID(tool_text(created_message)["page_id"])
        self.assertIn(("demo", "demo", "notes/fresh.md"), self.backend.store.pages)

    def test_forward_preserves_identity_headers_and_other_routes(self):
        status, headers, body = request(
            self.relay.url,
            "POST",
            "/hook?event=stop&agent=test",
            body=b'{"event":"hook-marker"}',
            headers={
                "Content-Type": "application/json",
                "Authorization": "Bearer hook-token",
                "Origin": self.relay.url,
                "X-Memory-Actor-Session-Id": "session-123",
                "Accept": "application/json, text/event-stream",
            },
        )
        self.assertEqual(status, 202)
        self.assertIn(b"hook-ok", body)
        captured = self.backend.store.captured[-1]
        self.assertEqual(captured["headers"]["authorization"], "Bearer hook-token")
        self.assertEqual(captured["headers"]["origin"], self.relay.url)
        self.assertEqual(captured["headers"]["x-memory-actor-session-id"], "session-123")
        self.assertIn(str(self.relay.port), captured["headers"]["host"])
        self.assertNotIn(str(self.backend.port), captured["headers"]["host"])
        self.assertEqual(captured["headers"]["accept"], "application/json, text/event-stream")
        self.assertEqual(captured["body"], b'{"event":"hook-marker"}')
        query_status, _, query = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_query", "arguments": {"query": "marker"}},
            rpc_id="abc",
        )
        self.assertEqual(query_status, 200)
        self.assertEqual(query["id"], "abc")
        self.assertEqual(tool_text(query)["hits"], ["QUERY-HIT"])
        self.assert_no_secrets(self.logs)

    def test_off_loopback_redirect_is_not_followed(self):
        self.backend.store.redirects["/go"] = "http://203.0.113.9/steal"
        status, headers, body = request(self.relay.url, "GET", "/go")
        self.assertEqual(status, 502)
        self.assertNotIn("203.0.113.9", body.decode())
        self.assertNotIn("203.0.113.9", json.dumps(headers))
        self.assertNotIn("location", headers)
        paths = [item["path"] for item in self.backend.store.captured]
        self.assertNotIn("/steal", paths)
        self.backend.store.redirects["/login"] = f"http://127.0.0.1:{self.backend.port}/web/login"
        status, headers, _body = request(self.relay.url, "GET", "/login")
        self.assertEqual(status, 302)
        self.assertEqual(headers["location"], f"http://127.0.0.1:{self.relay.port}/web/login")

    def test_auth_survives_process_restart_and_is_not_downgraded(self):
        self.backend.stop()
        _, _, first = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/auth.md",
                    "body": "# Auth\n\none",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
            headers={"Authorization": SECRET_TOKEN},
        )
        _, _, second = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/anon.md",
                    "body": "# Anon\n\ntwo",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
        )
        held = tool_text(second)["operation_id"]
        secret = self.outbox / "ops" / held / "auth.secret"
        self.assertFalse(secret.exists())
        authed = tool_text(first)["operation_id"]
        self.relay.stop()
        self.backend = Backend(self.backend.store, port=self.backend.port)
        env = os.environ.copy()
        env["AI_MEMORY_AUTH_TOKEN"] = "root-token-must-not-be-used"
        completed = subprocess.run(
            [
                "python3",
                str(ROOT / "reliable-mcp"),
                "worker",
                "--once",
                "--backend",
                self.backend.url,
                "--outbox",
                str(self.outbox),
            ],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assert_no_secrets(completed.stdout + completed.stderr)
        self.assertEqual(self.backend.store.write_count, 2)
        auths = [
            item["headers"].get("authorization")
            for item in self.backend.store.captured
            if _is_write(item)
        ]
        self.assertIn(SECRET_TOKEN, auths)
        self.assertIn(None, auths)
        self.assertNotIn("Bearer root-token-must-not-be-used", auths)
        os.remove(self.outbox / "ops" / authed / "auth.secret")
        intent = json.loads((self.outbox / "ops" / authed / "intent.json").read_text())
        intent["status"] = "pending"
        intent["page_id"] = None
        (self.outbox / "ops" / authed / "intent.json").write_text(json.dumps(intent))
        before = self.backend.store.write_count
        again = subprocess.run(
            [
                "python3",
                str(ROOT / "reliable-mcp"),
                "worker",
                "--once",
                "--backend",
                self.backend.url,
                "--outbox",
                str(self.outbox),
            ],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(self.backend.store.write_count, before)
        held_intent = json.loads((self.outbox / "ops" / authed / "intent.json").read_text())
        self.assertEqual(held_intent["status"], "pending")
        self.assertTrue(held_intent["reauthorization_required"])
        self.assert_no_secrets(again.stdout + again.stderr)

    def test_fsync_and_permission_failures_are_not_acked_as_queued(self):
        import reliable_mcp.outbox as outbox

        original = outbox.os.fsync

        def fail_fsync(_fd):
            raise OSError(28, "No space left on device")

        outbox.os.fsync = fail_fsync
        try:
            status, _, data = post_json(self.mcp, write_call("notes/disk.md", "body"))
        finally:
            outbox.os.fsync = original
        self.assertNotEqual(status, 200)
        text = data.decode()
        self.assertIn("not queued", text)
        self.assertNotIn("PENDING", text)
        self.assertNotIn("page_id", text)
        self.assertEqual(list((self.outbox / "ops").iterdir()), [])
        os.chmod(self.outbox / "ops", 0o555)
        try:
            status, _, data = post_json(self.mcp, write_call("notes/perm.md", "body"))
        finally:
            os.chmod(self.outbox / "ops", 0o700)
        self.assertNotEqual(status, 200)
        self.assertIn("not queued", data.decode())
        self.assertNotIn("PENDING", data.decode())

    def test_limits_body_concurrency_and_timeout(self):
        self.relay.stop()
        from reliable_mcp.relay import Relay

        self.relay = Relay(
            listen="127.0.0.1:0",
            backend=self.backend.url,
            outbox=self.outbox,
            auto_drain_seconds=0,
            max_body=512,
            max_inflight=1,
            acquire_timeout=0.2,
            forward_timeout=0.3,
        )
        self.relay.start()
        self.mcp = f"{self.relay.url}/mcp"
        status, _, data = post_json(self.mcp, write_call("notes/big.md", "x" * 600))
        self.assertEqual(status, 413)
        self.assertNotIn("PENDING", data.decode())
        self.backend.store.sleep_paths["/slow"] = 1.0
        outcomes = []
        entered = self.relay.handlers_entered

        def hit():
            outcomes.append(request(self.relay.url, "GET", "/slow", timeout=3))

        threads = [threading.Thread(target=hit) for _ in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(4)
        codes = sorted(item[0] for item in outcomes)
        self.assertIn(503, codes)
        self.assertEqual(self.relay.handlers_entered, entered + 1)
        self.backend.store.sleep_paths["/slow"] = 2.0
        started = time.monotonic()
        status, _, _data = request(self.relay.url, "GET", "/slow", timeout=3)
        self.assertLess(time.monotonic() - started, 1.2)
        self.assertEqual(status, 504)

    def test_cached_schema_works_during_outage_without_fake_tool_success(self):
        init_status, _, init = rpc(
            self.mcp,
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}},
            rpc_id="init-1",
        )
        self.assertEqual(init_status, 200)
        self.assertEqual(init["result"]["serverInfo"]["name"], "fake-memory")
        list_status, _, tools = rpc(self.mcp, "tools/list", {}, rpc_id="list-1")
        self.assertEqual(list_status, 200)
        self.assertIn("CAS-MARKER", json.dumps(tools))
        self.backend.stop()
        cached_status, _, cached = rpc(
            self.mcp,
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}},
            rpc_id="init-2",
        )
        self.assertEqual(cached_status, 200)
        self.assertEqual(cached["id"], "init-2")
        self.assertEqual(cached["result"]["serverInfo"]["name"], "fake-memory")
        _, _, cached_tools = rpc(self.mcp, "tools/list", {}, rpc_id="list-2")
        self.assertEqual(cached_tools["id"], "list-2")
        self.assertIn("CAS-MARKER", json.dumps(cached_tools))
        down_status, _, down = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_query", "arguments": {"query": "x"}},
            rpc_id="q-down",
        )
        self.assertEqual(down_status, 200)
        self.assertIn("error", down)
        self.assertNotIn("result", down)
        self.assertNotIn("QUERY-HIT", json.dumps(down))
        note_status, note_headers, note_body = post_json(
            self.mcp, {"jsonrpc": "2.0", "method": "notifications/initialized"}
        )
        self.assertEqual(note_status, 202)
        self.relay.stop()
        from reliable_mcp.relay import Relay

        restarted = Relay(
            listen="127.0.0.1:0",
            backend="http://127.0.0.1:9",
            outbox=self.outbox,
            auto_drain_seconds=0,
        )
        restarted.start()
        self.relay = restarted
        _, _, again = rpc(
            f"{restarted.url}/mcp",
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}},
            rpc_id="init-3",
        )
        self.assertEqual(again["result"]["protocolVersion"], "2024-11-05")
        self.assertEqual(again["result"]["serverInfo"]["version"], "0")

    def test_ready_requires_real_backend_mcp(self):
        from reliable_mcp.ready import probe

        ok, reason = probe(self.backend.url, "demo", "demo")
        self.assertTrue(ok, reason)
        self.backend.store.status_mode = "rpc_error"
        ok, reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.assertIn("memory_status", reason)
        self.backend.store.status_mode = "is_error"
        ok, _reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.backend.store.status_mode = "http_500"
        ok, _reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.backend.store.status_mode = "http_405"
        ok, _reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.backend.store.status_mode = "malformed"
        ok, _reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.backend.store.status_mode = "ok"
        self.backend.store.status_lie = ("other", "project")
        ok, reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.assertIn("scope", reason)
        self.backend.store.status_lie = None
        self.backend.store.cas_advertised = False
        ok, reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        self.assertIn("create_only", reason)
        self.backend.store.cas_advertised = True
        self.backend.store.health_only = True
        ok, _reason = probe(self.backend.url, "demo", "demo")
        self.assertFalse(ok)
        health, _, health_body = request(self.backend.url, "GET", "/healthz")
        self.assertEqual(health, 200)
        self.assertIn(b"ok", health_body)
        self.backend.store.health_only = False
        self.backend.stop()
        ok, _reason = probe(self.backend.url, "demo", "demo", per_call=0.4, budget=1.5)
        self.assertFalse(ok)
        self.backend = Backend(self.backend.store, port=self.backend.port)
        ok, reason = probe(self.backend.url, "demo", "demo")
        self.assertTrue(ok, reason)
        rpc(
            self.mcp,
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}},
        )
        rpc(self.mcp, "tools/list", {})
        self.backend.stop()
        ok, reason = probe(self.relay.url, "demo", "demo", per_call=0.4, budget=2)
        self.assertFalse(ok, reason)

    def test_new_outbox_directory_entries_are_synced(self):
        import reliable_mcp.outbox as outbox

        box = Path(self.tmp.name) / "new-outbox"
        synced = []
        original = outbox.os.fsync

        def track(fd):
            synced.append(os.fstat(fd).st_ino)
            original(fd)

        outbox.os.fsync = track
        try:
            outbox.ensure_outbox(box)
        finally:
            outbox.os.fsync = original
        self.assertIn(box.parent.stat().st_ino, synced)
        self.assertIn(box.stat().st_ino, synced)

    def test_symlink_outbox_and_root_are_refused(self):
        from reliable_mcp.outbox import OutboxError, ensure_outbox
        from reliable_mcp.relay import Relay

        outside = Path(self.tmp.name) / "outside"
        outside.mkdir()
        link = Path(self.tmp.name) / "linked-outbox"
        link.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(OutboxError):
            ensure_outbox(link)
        core = Path(self.tmp.name) / "ai-memory"
        nested = core / "queue"
        nested.parent.mkdir()
        with self.assertRaises(OutboxError):
            ensure_outbox(nested, data_dir=core)
        import reliable_mcp.safe as safe

        original = safe.os.geteuid
        safe.os.geteuid = lambda: 0
        try:
            with self.assertRaises(PermissionError):
                Relay(
                    listen="127.0.0.1:0",
                    backend=self.backend.url,
                    outbox=Path(self.tmp.name) / "root-box",
                    auto_drain_seconds=0,
                )
        finally:
            safe.os.geteuid = original

    def test_same_listen_and_non_loopback_are_rejected(self):
        from reliable_mcp.relay import Relay

        with self.assertRaises(ValueError):
            Relay(
                listen=f"127.0.0.1:{self.backend.port}",
                backend=self.backend.url,
                outbox=Path(self.tmp.name) / "loop",
                auto_drain_seconds=0,
            )
        with self.assertRaises(ValueError):
            Relay(
                listen="0.0.0.0:9",
                backend=self.backend.url,
                outbox=Path(self.tmp.name) / "wild",
                auto_drain_seconds=0,
            )
        with self.assertRaises(ValueError):
            Relay(
                listen="127.0.0.1:9",
                backend="http://203.0.113.8:9",
                outbox=Path(self.tmp.name) / "remote",
                auto_drain_seconds=0,
            )

    def test_replay_keeps_conflict_until_explicit_reconcile(self):
        self.backend.stop()
        _, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/recon.md",
                    "body": "# Queued\n\nours",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
        )
        operation_id = tool_text(message)["operation_id"]
        self.backend = Backend(self.backend.store, port=self.backend.port)
        from reliable_mcp.backend import Client

        self.relay.client = Client(self.backend.url, timeout=2)
        self.backend.store.force_put("demo", "demo", "notes/recon.md", "# Head\n\ntheirs", title="Head")
        self.relay.drain()
        completed = subprocess.run(
            [
                "python3",
                str(ROOT / "reliable-mcp"),
                "replay",
                "--outbox",
                str(self.outbox),
                "--backend",
                self.backend.url,
                "--operation",
                operation_id,
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("conflict", completed.stdout)
        self.assertNotIn("ours", completed.stdout)
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/recon.md")]["body"], "# Head\n\ntheirs")
        accepted = subprocess.run(
            [
                "python3",
                str(ROOT / "reliable-mcp"),
                "reconcile",
                "--outbox",
                str(self.outbox),
                "--backend",
                self.backend.url,
                "--operation",
                operation_id,
                "--accept-current-head",
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertNotIn("ours", accepted.stdout)
        self.assertEqual(self.relay.drain(), 1)
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/recon.md")]["body"], "# Queued\n\nours")

    def test_online_write_is_durable_before_backend_sees_it(self):
        _, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/order.md",
                    "body": "# Order\n\nbody",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
        )
        self.assertEqual(tool_text(message)["status"], "DELIVERED")
        self.assertTrue(self.backend.store.durable_before_write)
        page_id = tool_text(message)["page_id"]
        uuid.UUID(page_id)

    def test_status_hides_bodies(self):
        self.backend.stop()
        rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/hidden.md",
                    "body": SECRET_BODY,
                    "workspace": "demo",
                    "project": "demo",
                },
            },
            headers={"Authorization": SECRET_TOKEN},
        )
        completed = subprocess.run(
            ["python3", str(ROOT / "reliable-mcp"), "status", "--outbox", str(self.outbox)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("pending", completed.stdout)
        self.assertIn("notes/hidden.md", completed.stdout)
        self.assert_no_secrets(completed.stdout + completed.stderr)

    def test_retry_limit_does_not_drop_the_operation(self):
        self.backend.stop()
        _, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/stay.md",
                    "body": "# Stay\n\nqueued",
                    "workspace": "demo",
                    "project": "demo",
                },
            },
        )
        operation_id = tool_text(message)["operation_id"]
        for _ in range(4):
            self.assertEqual(self.relay.drain(), 0)
        intent = json.loads((self.outbox / "ops" / operation_id / "intent.json").read_text())
        self.assertEqual(intent["status"], "pending")
        self.assertGreaterEqual(intent["attempts"], 4)
        self.assertTrue((self.outbox / "ops" / operation_id / "body.md").is_file())


def _write(case, body, path="notes/meta.md", **arguments):
    _, _, message = rpc(
        case.mcp,
        "tools/call",
        {
            "name": "memory_write_page",
            "arguments": {
                "path": path,
                "body": body,
                "workspace": arguments.pop("workspace", "demo"),
                "project": arguments.pop("project", "demo"),
                **arguments,
            },
        },
    )
    if "error" in message:
        raise AssertionError(message)
    return tool_text(message)


def _is_write(item):
    if not item["path"].startswith("/mcp"):
        return False
    try:
        message = json.loads(item["body"].decode())
    except json.JSONDecodeError:
        return False
    return message.get("method") == "tools/call" and (message.get("params") or {}).get("name") == "memory_write_page"


class InstallCase(unittest.TestCase):
    def test_install_stages_without_touching_units_or_the_installer(self):
        unit_dir = Path.home() / ".config" / "systemd" / "user"
        before = _snapshot(unit_dir)
        installer_hash = hashlib.sha256(INSTALLER.read_bytes()).hexdigest() if INSTALLER.is_file() else None
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "ai-memory.service"
            fixture.write_text(
                "\n".join(
                    [
                        "[Service]",
                        "ExecStart=%h/.local/bin/ai-memory serve --transport http --bind 127.0.0.1:49374 --enable-web",
                        "Environment=AI_MEMORY_AUTH_TOKEN=stage-token-do-not-copy",
                        "Restart=on-failure",
                        "",
                    ]
                )
            )
            stage = Path(tmp) / "stage"
            dry = subprocess.run(
                ["python3", str(ROOT / "reliable-mcp"), "install", "--dry-run", "--unit", str(fixture)],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(dry.returncode, 0, dry.stderr)
            self.assertIn("não instala", dry.stdout)
            self.assertFalse(stage.exists())
            staged = subprocess.run(
                [
                    "python3",
                    str(ROOT / "reliable-mcp"),
                    "install",
                    "--stage",
                    str(stage),
                    "--unit",
                    str(fixture),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(staged.returncode, 0, staged.stderr)
            dropin = (stage / "units" / "ai-memory.service.d" / "backend-bind.conf").read_text()
            self.assertIn("--bind 127.0.0.1:49376", dropin)
            self.assertIn("%h/.local/bin/ai-memory", dropin)
            self.assertIn("--enable-web", dropin)
            self.assertNotIn("stage-token-do-not-copy", dropin)
            self.assertNotIn("Environment=", dropin)
            ingress = (stage / "units" / "ai-memory-ingress.service").read_text()
            worker = (stage / "units" / "ai-memory-delivery.service").read_text()
            for text in (ingress, worker):
                self.assertNotIn("PartOf=", text)
                self.assertNotIn("Requires=", text)
                self.assertNotIn(".worktrees", text)
                self.assertIn(".agents/tools/ai-memory/reliable-mcp", text)
            self.assertTrue((stage / "source" / "reliable-mcp").is_file())
            self.assertTrue((stage / "patches" / "install-ai-memory-ready.patch").is_file())
            copied = Path(tmp) / "install-ai-memory.sh"
            shutil.copy(INSTALLER, copied)
            patch = (ROOT / "patches" / "install-ai-memory-ready.patch").read_text()
            applied = subprocess.run(
                ["patch", "-p1"],
                cwd=tmp,
                input=patch,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(applied.returncode, 0, applied.stderr)
            rewritten = copied.read_text()
            self.assertIn("ready --backend", rewritten)
            self.assertIn("AI_MEMORY_READY_WORKSPACE", rewritten)
            self.assertNotIn("/handoff", rewritten)
        self.assertEqual(_snapshot(unit_dir), before)
        if installer_hash is not None:
            self.assertEqual(hashlib.sha256(INSTALLER.read_bytes()).hexdigest(), installer_hash)


def _snapshot(path: Path):
    if not path.exists():
        return None
    return sorted(str(item.relative_to(path)) for item in path.rglob("*"))


if __name__ == "__main__":
    unittest.main()
