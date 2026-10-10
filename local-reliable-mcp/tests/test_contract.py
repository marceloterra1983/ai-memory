"""Failing contracts for typed arguments, guards, HTTP, host, and credentials."""

from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import threading
import unittest
import uuid
from pathlib import Path

from tests import test_ingress
from tests.http import request, rpc, tool_text

ROOT = Path(__file__).resolve().parents[1]


def _base_args(**extra):
    args = {
        "path": "notes/typed.md",
        "body": "# Typed\n\nbody",
        "workspace": "demo",
        "project": "demo",
    }
    args.update(extra)
    return args


class _Harness:
    """Runs the ingress fixture without inheriting its tests."""

    def __init__(self):
        self.case = test_ingress.IngressCase("test_offline_write_is_pending_without_fake_page_id")
        self.case.setUp()

    def close(self):
        self.case.tearDown()


class TypedArguments(unittest.TestCase):
    def setUp(self):
        self.harness = _Harness()
        self.relay = self.harness.case.relay
        self.backend = self.harness.case.backend
        self.outbox = self.harness.case.outbox
        self.mcp = self.harness.case.mcp

    def tearDown(self):
        self.harness.case.relay = self.relay
        self.harness.case.backend = self.backend
        self.harness.close()

    def test_each_invalid_type_is_refused_before_enqueue(self):
        from reliable_mcp.protocol import normalize_write

        cases = [
            ("create_only", {"create_only": "invalid"}),
            ("create_only", {"create_only": None}),
            ("create_only", {"create_only": 1}),
            ("expected_page_id", {"expected_page_id": 123}),
            ("expected_page_id", {"expected_page_id": ""}),
            ("expected_page_id", {"expected_page_id": "   "}),
            ("expected_page_id", {"expected_page_id": "not-a-uuid"}),
            ("tier", {"tier": ""}),
            ("tier", {"tier": " semantic"}),
            ("tier", {"tier": "semantic "}),
            ("pinned", {"pinned": None}),
            ("pinned", {"pinned": "true"}),
            ("tags", {"tags": None}),
            ("tags", {"tags": [1]}),
            ("title", {"title": 1}),
            ("expires_at", {"expires_at": 1}),
        ]
        for needle, extra in cases:
            with self.subTest(extra=extra):
                normalized, error = normalize_write(_base_args(**extra))
                self.assertIsNone(normalized)
                self.assertIn(needle, error)
        omitted, error = normalize_write(_base_args())
        self.assertIsNone(error)
        self.assertEqual(omitted["cas"]["mode"], "auto")
        self.assertEqual(omitted["tier"], "semantic")
        self.assertIs(omitted["pinned"], False)
        self.assertEqual(omitted["tags"], [])
        null_expected, error = normalize_write(_base_args(expected_page_id=None))
        self.assertIsNone(error)
        self.assertEqual(null_expected["cas"]["mode"], "auto")
        blank_title, error = normalize_write(_base_args(title=""))
        self.assertIsNone(error)
        self.assertEqual(blank_title["title"], "")
        blank_expires, error = normalize_write(_base_args(expires_at="  \t"))
        self.assertIsNone(error)
        self.assertIsNone(blank_expires["expires_at"])
        self.backend.stop()
        before = list((self.outbox / "ops").iterdir()) if (self.outbox / "ops").exists() else []
        status, _, message = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_write_page", "arguments": _base_args(create_only="invalid")},
        )
        self.assertEqual(status, 200)
        self.assertEqual(message["error"]["code"], -32602)
        self.assertEqual(list((self.outbox / "ops").iterdir()) if (self.outbox / "ops").exists() else [], before)
        self.assertEqual(self.backend.store.write_count, 0)

    def test_explicit_guards_beat_identical_content_until_a_write_starts(self):
        body = "# Same\n\nexact"
        page = self.backend.store.force_put("demo", "demo", "notes/same.md", body)
        created, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": _base_args(path="notes/same.md", body=body, create_only=True),
            },
        )
        self.assertEqual(created, 200)
        receipt = tool_text(message)
        self.assertEqual(receipt["status"], "CONFLICT")
        self.assertFalse(receipt["written"])
        self.assertEqual(self.backend.store.write_count, 0)
        stale, _, stale_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": _base_args(
                    path="notes/same.md",
                    body=body,
                    expected_page_id=str(uuid.uuid4()),
                ),
            },
        )
        self.assertEqual(stale, 200)
        self.assertEqual(tool_text(stale_message)["status"], "CONFLICT")
        self.assertEqual(self.backend.store.write_count, 0)
        self.assertEqual(self.backend.store.pages[("demo", "demo", "notes/same.md")]["page_id"], page["page_id"])

        from reliable_mcp.backend import Client

        original = Client.write_page
        self_outbox = self.outbox / "ops"
        calls = {}

        def crash_after_marker(client, arguments, authorization=None, extra=None):
            calls["arguments"] = arguments
            for child in self_outbox.iterdir():
                stored = json.loads((child / "intent.json").read_text(encoding="utf-8"))
                if stored.get("path") == "notes/fresh.md":
                    calls["write_started"] = stored.get("write_started")
            raise TimeoutError("lost after marker")

        Client.write_page = crash_after_marker
        try:
            status, _, lost = rpc(
                self.mcp,
                "tools/call",
                {
                    "name": "memory_write_page",
                    "arguments": _base_args(path="notes/fresh.md", body="# Fresh\n\nonce", create_only=True),
                },
            )
        finally:
            Client.write_page = original
        self.assertEqual(status, 200)
        self.assertEqual(tool_text(lost)["status"], "PENDING")
        self.assertTrue(calls["write_started"])
        self.assertEqual(self.backend.store.write_count, 0)
        operation_id = tool_text(lost)["operation_id"]
        self.backend.store.force_put("demo", "demo", "notes/fresh.md", "# Fresh\n\nonce")
        self.assertEqual(self.relay.drain(), 1)
        intent = json.loads((self.outbox / "ops" / operation_id / "intent.json").read_text())
        self.assertEqual(intent["status"], "delivered")
        self.assertEqual(self.backend.store.write_count, 0)


class RawHttp(unittest.TestCase):
    def setUp(self):
        self.harness = _Harness()
        self.relay = self.harness.case.relay
        self.backend = self.harness.case.backend
        self.outbox = self.harness.case.outbox
        self.mcp = self.harness.case.mcp

    def tearDown(self):
        self.harness.case.relay = self.relay
        self.harness.case.backend = self.backend
        self.harness.close()

    def _raw(self, payload: bytes, timeout=2):
        sock = socket.create_connection(("127.0.0.1", self.relay.port), timeout)
        sock.settimeout(timeout)
        sock.sendall(payload)
        try:
            sock.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        chunks = []
        while True:
            try:
                data = sock.recv(4096)
            except TimeoutError:
                break
            if not data:
                break
            chunks.append(data)
        sock.close()
        return b"".join(chunks)

    def test_bodyless_get_head_options_do_not_require_content_length(self):
        for method in (b"GET", b"HEAD", b"OPTIONS"):
            with self.subTest(method=method.decode()):
                raw = self._raw(method + b" /handoff HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nConnection: close\r\n\r\n" % self.relay.port)
                status = int(raw.split(b" ", 2)[1])
                self.assertNotEqual(status, 411)
                self.assertNotIn(b"PENDING", raw)
        self.assertFalse((self.outbox / "ops").exists() and any((self.outbox / "ops").iterdir()))

    def test_smuggling_and_limits_are_rejected_on_the_socket(self):
        host = f"127.0.0.1:{self.relay.port}".encode()
        duplicate = self._raw(
            b"POST /mcp HTTP/1.1\r\nHost: "
            + host
            + b"\r\nContent-Length: 1\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"
        )
        self.assertTrue(duplicate.startswith(b"HTTP/1.1 400"))
        encoded = self._raw(
            b"POST /mcp HTTP/1.1\r\nHost: "
            + host
            + b"\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        self.assertTrue(encoded.startswith(b"HTTP/1.1 411"))
        invalid = self._raw(
            b"POST /mcp HTTP/1.1\r\nHost: " + host + b"\r\nContent-Length: 12abc\r\nConnection: close\r\n\r\n"
        )
        self.assertTrue(invalid.startswith(b"HTTP/1.1 400"))
        short = self._raw(
            b"POST /mcp HTTP/1.1\r\nHost: "
            + host
            + b"\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort"
        )
        self.assertTrue(short.startswith(b"HTTP/1.1 400"))
        self.relay.stop()
        from reliable_mcp.relay import Relay

        self.relay = Relay(
            listen="127.0.0.1:0",
            backend=self.backend.url,
            outbox=self.outbox,
            auto_drain_seconds=0,
            read_timeout=0.3,
            max_header=128,
            max_inflight=1,
        )
        self.relay.start()
        self.harness.case.relay = self.relay
        host = f"127.0.0.1:{self.relay.port}".encode()
        huge = b"A" * 400
        too_big = self._raw(b"GET /handoff HTTP/1.1\r\nHost: " + host + b"\r\nX-Big: " + huge + b"\r\nConnection: close\r\n\r\n")
        self.assertIn(too_big.split(b" ", 2)[1], {b"431", b"400", b"408"})
        started = threading.Event()

        def stall():
            sock = socket.create_connection(("127.0.0.1", self.relay.port), 2)
            sock.settimeout(2)
            started.set()
            try:
                sock.recv(64)
            except OSError:
                pass
            sock.close()

        thread = threading.Thread(target=stall)
        thread.start()
        self.assertTrue(started.wait(1))
        thread.join(1.5)
        self.assertFalse(thread.is_alive())
        bound = self.relay._httpd.socket.getsockname()
        self.assertEqual(bound[0], "127.0.0.1")


class HostOriginAndCredential(unittest.TestCase):
    def setUp(self):
        self.harness = _Harness()
        self.relay = self.harness.case.relay
        self.backend = self.harness.case.backend
        self.outbox = self.harness.case.outbox
        self.mcp = self.harness.case.mcp

    def tearDown(self):
        self.harness.case.relay = self.relay
        self.harness.case.backend = self.backend
        self.harness.close()

    def test_evil_host_and_origin_do_not_queue_when_backend_is_offline(self):
        self.backend.stop()
        status, _, body = request(
            self.relay.url,
            "POST",
            "/mcp",
            body=json.dumps(test_ingress.write_call("notes/host.md", "# Host\n\nbody")).encode(),
            headers={
                "Host": "evil.example",
                "Origin": "https://evil.example",
                "Content-Type": "application/json",
                "Authorization": test_ingress.SECRET_TOKEN,
            },
        )
        self.assertEqual(status, 403)
        self.assertNotIn(b"PENDING", body)
        self.assertFalse((self.outbox / "ops").exists() and any((self.outbox / "ops").iterdir()))
        allowed, _, allowed_message = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_write_page", "arguments": _base_args(path="notes/local.md")},
            headers={"Origin": self.relay.url},
        )
        self.assertEqual(allowed, 200)
        self.assertEqual(tool_text(allowed_message)["status"], "PENDING")

    def test_duplicate_authorization_is_not_downgraded_and_probe_uses_the_operation(self):
        dup = self._duplicate_authorization()
        self.assertEqual(dup[0], 400)
        self.assertFalse((self.outbox / "ops").exists() and any((self.outbox / "ops").iterdir()))
        self.backend.store.require_authorization = True
        self.backend.store.accepted_authorization = "Bearer op-token"
        env = os.environ.get("AI_MEMORY_AUTH_TOKEN")
        os.environ["AI_MEMORY_AUTH_TOKEN"] = "Bearer root-token-must-not-be-used"
        try:
            status, _, message = rpc(
                self.mcp,
                "tools/call",
                {"name": "memory_write_page", "arguments": _base_args(path="notes/authz.md", body="# Authz\n\nsecret")},
                headers={
                    "Authorization": "Bearer op-token",
                    "Mcp-Session-Id": "session-op",
                    "Origin": self.relay.url,
                },
            )
        finally:
            if env is None:
                os.environ.pop("AI_MEMORY_AUTH_TOKEN", None)
            else:
                os.environ["AI_MEMORY_AUTH_TOKEN"] = env
        self.assertEqual(status, 200, message)
        self.assertEqual(tool_text(message)["status"], "DELIVERED")
        listed = [
            item
            for item in self.backend.store.captured
            if item["path"].startswith("/mcp") and b"tools/list" in item["body"]
        ]
        self.assertTrue(listed)
        for item in listed:
            self.assertEqual(item["headers"].get("authorization"), "Bearer op-token")
            self.assertEqual(item["headers"].get("mcp-session-id"), "session-op")
            self.assertNotIn("root-token", item["headers"].get("authorization", ""))
        writes = [item for item in self.backend.store.captured if b"memory_write_page" in item["body"]]
        self.assertEqual(writes[-1]["headers"].get("authorization"), "Bearer op-token")

    def test_write_mime_is_rejected_before_enqueue_and_other_routes_stay_open(self):
        status, _, body = request(
            self.relay.url,
            "POST",
            "/mcp",
            body=json.dumps(test_ingress.write_call("notes/mime.md", "# Mime\n\nno")).encode(),
            headers={"Content-Type": "text/plain"},
        )
        self.assertEqual(status, 415)
        self.assertFalse((self.outbox / "ops").exists() and any((self.outbox / "ops").iterdir()))
        hook, _, hook_body = request(
            self.relay.url,
            "POST",
            "/hook",
            body=b"plain",
            headers={"Content-Type": "text/plain"},
        )
        self.assertEqual(hook, 202)
        self.assertIn(b"hook-ok", hook_body)
        typed, _, message = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_write_page", "arguments": _base_args(path="notes/charset.md", body="# Charset\n\nok")},
            headers={"Content-Type": "application/json; charset=utf-8"},
        )
        self.assertEqual(typed, 200)
        self.assertEqual(tool_text(message)["status"], "DELIVERED")

    def _duplicate_authorization(self):
        host = f"127.0.0.1:{self.relay.port}"
        body = json.dumps(test_ingress.write_call("notes/dup.md", "x")).encode()
        payload = (
            f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nHost: evil.example\r\n"
            f"Authorization: Bearer one\r\nAuthorization: Bearer two\r\n"
            f"Content-Type: application/json\r\nContent-Length: {len(body)}\r\n"
            f"Connection: close\r\n\r\n"
        ).encode() + body
        raw = RawHttp._raw(self, payload)
        status = int(raw.split(b" ", 2)[1])
        return status, raw

    def test_scope_absence_allows_only_create_and_text_is_not_absence(self):
        self.backend.store.known_scopes.discard(("fresh", "proj"))
        status, _, message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/new.md",
                    "body": "# New\n\nscope",
                    "workspace": "fresh",
                    "project": "proj",
                },
            },
        )
        self.assertEqual(status, 200)
        self.assertEqual(tool_text(message)["status"], "DELIVERED")
        sent = [
            json.loads(item["body"].decode())["params"]["arguments"]
            for item in self.backend.store.captured
            if b"memory_write_page" in item["body"] and b"notes/new.md" in item["body"]
        ]
        self.assertTrue(sent[-1].get("create_only"))
        self.assertNotIn("expected_page_id", sent[-1])
        before = self.backend.store.write_count
        self.backend.store.known_scopes.discard(("missing", "proj"))
        refused, _, refused_message = rpc(
            self.mcp,
            "tools/call",
            {
                "name": "memory_write_page",
                "arguments": {
                    "path": "notes/stale.md",
                    "body": "# Stale\n\nno",
                    "workspace": "missing",
                    "project": "proj",
                    "expected_page_id": str(uuid.uuid4()),
                },
            },
        )
        self.assertEqual(refused, 200)
        self.assertEqual(tool_text(refused_message)["status"], "CONFLICT")
        self.assertEqual(self.backend.store.write_count, before)
        self.assertNotIn(("missing", "proj", "notes/stale.md"), self.backend.store.pages)
        self.backend.store.read_failure = (
            -32603,
            "temporary io: directory not found",
            None,
        )
        io_status, _, io_message = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_write_page", "arguments": _base_args(path="notes/io.md", body="# Io\n\nno")},
        )
        self.assertEqual(io_status, 200)
        self.assertEqual(tool_text(io_message)["status"], "PENDING")
        self.assertEqual(self.backend.store.write_count, before)
        self.backend.store.read_failure = (
            -32602,
            "project 'nope' not found in workspace 'nope'",
            {"code": "not_a_page"},
        )
        weird, _, weird_message = rpc(
            self.mcp,
            "tools/call",
            {"name": "memory_write_page", "arguments": _base_args(path="notes/weird.md")},
        )
        self.assertEqual(tool_text(weird_message)["status"], "PENDING")
        self.assertNotIn(("demo", "demo", "notes/weird.md"), self.backend.store.pages)


class InstallerReadiness(unittest.TestCase):
    def test_patched_readiness_requires_active_ingress_worker_and_entry_mcp(self):
        fixture = ROOT / "tests" / "fixtures" / "install-ai-memory-before-ready.sh"
        patch = (ROOT / "patches" / "install-ai-memory-ready.patch").read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            copied = root / "install-ai-memory.sh"
            copied.write_bytes(fixture.read_bytes())
            dry_run = subprocess.run(
                ["patch", "--fuzz=0", "--dry-run", "-p1"], cwd=root, input=patch,
                text=True, capture_output=True, check=False,
            )
            self.assertEqual(dry_run.returncode, 0, dry_run.stdout + dry_run.stderr)
            applied = subprocess.run(
                ["patch", "--fuzz=0", "-p1"], cwd=root, input=patch,
                text=True, capture_output=True, check=False,
            )
            self.assertEqual(applied.returncode, 0, applied.stdout + applied.stderr)
            syntax = subprocess.run(["bash", "-n", str(copied)], text=True, capture_output=True, check=False)
            self.assertEqual(syntax.returncode, 0, syntax.stderr)
            patched = copied.read_text(encoding="utf-8")
            start = patched.index("\nready=0\n") + 1
            last = '\nok "servidor em $SERVER_ORIGIN"\n'
            end = patched.index(last, start) + len(last)
            runner = (
                "set -euo pipefail\n"
                "die() { printf '%s\\n' \"$*\" >&2; exit 1; }\n"
                "ok() { printf '%s\\n' \"$*\"; }\n"
                + patched[start:end]
            )
            manifest = root / "manifest.json"
            stubs = root / "bin"
            stubs.mkdir()
            systemctl = stubs / "systemctl"
            systemctl.write_text(
                '#!/bin/bash\nset -euo pipefail\nprintf "systemctl %s\\n" "$*" >> "$CALLS"\n'
                '[[ "$1" == --user && $# -eq 4 ]] || exit 2\n'
                'if [[ "$2" == is-active && "$3" == --quiet ]]; then\n'
                '  [[ "$4" != "$INACTIVE_UNIT" ]]\n'
                'elif [[ "$2" == status && "$3" == --no-pager ]]; then\n'
                '  exit 0\nelse\n  exit 2\nfi\n',
                encoding="utf-8",
            )
            reliable = stubs / "reliable-mcp"
            reliable.write_text(
                '#!/usr/bin/env python3\nimport os, sys\n'
                'from reliable_mcp.safe import parse_origin\n'
                'args = sys.argv[1:]\n'
                'with open(os.environ["CALLS"], "a") as log:\n'
                '    log.write("reliable-mcp " + " ".join(args) + "\\n")\n'
                'if (len(args) != 7 or args[:2] != ["ready", "--backend"] or\n'
                '    args[3:] != ["--workspace", os.environ["EXPECTED_WS"], "--project", os.environ["EXPECTED_PR"]]):\n'
                '    sys.exit(2)\n'
                'try:\n'
                '    parse_origin(args[2])\n'
                'except ValueError as exc:\n'
                '    sys.exit(str(exc))\n'
                'if args[2] == os.environ["EXPECTED_BACKEND"]:\n'
                '    print("ready")\n'
                '    sys.exit(0)\n'
                'if args[2] == "http://127.0.0.1:49374":\n'
                '    if os.environ["ENTRY_EXIT"] == "0":\n'
                '        print("ready")\n'
                '    sys.exit(int(os.environ["ENTRY_EXIT"]))\n'
                'sys.exit(2)\n',
                encoding="utf-8",
            )
            sleep = stubs / "sleep"
            sleep.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            sleep.chmod(0o700)
            systemctl.chmod(0o700)
            reliable.chmod(0o700)
            calls = root / "calls.log"
            cases = (
                {"name": "inactive ingress", "inactive": "ai-memory-ingress.service", "error": "ai-memory-ingress.service"},
                {"name": "inactive delivery", "inactive": "ai-memory-delivery.service", "error": "ai-memory-delivery.service"},
                {"name": "entry not ready", "entry_exit": "1", "error": "entrada"},
                {"name": "manifest 49377"},
                {"name": "legacy manifest", "legacy": True, "backend": "http://127.0.0.1:49376"},
                {"name": "backend override", "overrides": {"AI_MEMORY_BACKEND_URL": "http://127.0.0.1:49378"},
                 "backend": "http://127.0.0.1:49378"},
                {"name": "complete override without manifest", "missing_manifest": True,
                 "overrides": {"AI_MEMORY_BACKEND_URL": "http://127.0.0.1:49378",
                               "AI_MEMORY_READY_WORKSPACE": "override-ws", "AI_MEMORY_READY_PROJECT": "override-pr"},
                 "backend": "http://127.0.0.1:49378", "workspace": "override-ws", "project": "override-pr"},
                {"name": "malformed bind", "bind": "127.0.0.1:not-a-port", "invalid": True, "error": "servidor não respondeu"},
                {"name": "non-loopback bind", "bind": "192.0.2.1:49377", "invalid": True, "error": "servidor não respondeu"},
                {"name": "null bind", "bind": None, "before_probe": True, "error": "backend_bind"},
                {"name": "numeric bind", "bind": 49377, "before_probe": True, "error": "backend_bind"},
                {"name": "empty bind", "bind": "", "before_probe": True, "error": "backend_bind"},
            )
            for case in cases:
                with self.subTest(case=case["name"]):
                    service = {"readiness_scope": {"workspace": "default", "project": "dev"}}
                    if not case.get("legacy"):
                        service["backend_bind"] = case.get("bind", "127.0.0.1:49377")
                    manifest.write_text(json.dumps({"service": service}), encoding="utf-8")
                    if case.get("missing_manifest"):
                        manifest.unlink()
                    calls.write_text("", encoding="utf-8")
                    backend = case.get("backend", "http://127.0.0.1:49377")
                    workspace = case.get("workspace", "default")
                    project = case.get("project", "dev")
                    env = {
                        "PATH": str(stubs) + os.pathsep + os.defpath,
                        "HOME": str(root),
                        "PYTHONPATH": str(ROOT),
                        "PYTHONDONTWRITEBYTECODE": "1",
                        "MANIFEST": str(manifest),
                        "RELIABLE_MCP": str(reliable),
                        "SERVER_ORIGIN": "http://127.0.0.1:49374",
                        "CALLS": str(calls),
                        "EXPECTED_BACKEND": backend,
                        "EXPECTED_WS": workspace,
                        "EXPECTED_PR": project,
                        "INACTIVE_UNIT": case.get("inactive", ""),
                        "ENTRY_EXIT": case.get("entry_exit", "0"),
                    }
                    env.update(case.get("overrides", {}))
                    result = subprocess.run(
                        ["bash", "-c", runner], cwd=root, env=env, timeout=5,
                        text=True, capture_output=True, check=False,
                    )
                    invoked = calls.read_text(encoding="utf-8").splitlines()
                    error = case.get("error")
                    if case.get("before_probe"):
                        self.assertEqual(invoked, [])
                    else:
                        chosen = "http://" + case["bind"] if case.get("invalid") else backend
                        self.assertEqual(
                            invoked[0],
                            f"reliable-mcp ready --backend {chosen} --workspace {workspace} --project {project}",
                        )
                    if error is not None:
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertNotIn("servidor em", result.stdout + result.stderr)
                        self.assertIn(error, result.stderr)
                        if case.get("invalid") or case.get("before_probe"):
                            self.assertNotIn("ready", result.stdout)
                    else:
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("servidor em http://127.0.0.1:49374", result.stdout)
                        self.assertEqual(invoked[1:], [
                            "systemctl --user is-active --quiet ai-memory-ingress.service",
                            "systemctl --user is-active --quiet ai-memory-delivery.service",
                            f"reliable-mcp ready --backend http://127.0.0.1:49374 --workspace {workspace} --project {project}",
                        ])

    def test_patch_reads_manifest_scope_and_check_stays_readonly(self):
        import hashlib
        import shutil

        installer = ROOT / "tests" / "fixtures" / "install-ai-memory-before-ready.sh"
        self.assertTrue(installer.is_file(), "fixed unpatched installer fixture is absent")
        before = hashlib.sha256(installer.read_bytes()).hexdigest()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            copied = root / "install-ai-memory.sh"
            shutil.copy(installer, copied)
            copied.chmod(0o700)
            patch = (ROOT / "patches" / "install-ai-memory-ready.patch").read_text(encoding="utf-8")
            applied = subprocess.run(["patch", "--fuzz=0", "-p1"], cwd=root, input=patch, text=True, capture_output=True, check=False)
            self.assertEqual(applied.returncode, 0, applied.stderr)
            text = copied.read_text(encoding="utf-8")
            self.assertIn("readiness_scope", text)
            self.assertIn("AI_MEMORY_READY_WORKSPACE", text)
            self.assertIn("8003", text)
            self.assertIn("embedding-query.conf", text)
            self.assertNotIn("/handoff", text)
            syntax = subprocess.run(["bash", "-n", str(copied)], capture_output=True, text=True, check=False)
            self.assertEqual(syntax.returncode, 0, syntax.stderr)
            home = root / "home"
            home.mkdir()
            manifest = root / "manifest.json"
            manifest.write_text(
                json.dumps({"service": {"readiness_scope": {"workspace": "default", "project": "dev"}}}),
                encoding="utf-8",
            )
            env = os.environ.copy()
            env.update(
                {
                    "HOME": str(home),
                    "AI_MEMORY_MANIFEST": str(manifest),
                    "AI_MEMORY_BIN": str(root / "missing-bin"),
                    "AI_MEMORY_CONFIG": str(root / "missing.toml"),
                    "AI_MEMORY_UNIT": str(root / "missing.service"),
                    "AI_MEMORY_DATA_DIR": str(root / "missing-data"),
                    "AI_MEMORY_SYSTEM_BIN": str(root / "missing-system"),
                }
            )
            snapshot = sorted(p.relative_to(root).as_posix() for p in root.rglob("*"))
            checked = subprocess.run([str(copied), "--check"], cwd=root, env=env, text=True, capture_output=True, check=False)
            after = sorted(p.relative_to(root).as_posix() for p in root.rglob("*"))
            self.assertEqual(after, snapshot)
            self.assertNotEqual(checked.returncode, 0)
            self.assertIn("somente leitura", checked.stdout + checked.stderr)
            self.assertIn("override de prontidão incompleto", text)
            start = text.index('if [[ -n "${AI_MEMORY_READY_WORKSPACE+x}"')
            end = text.index("for _ in $(seq 1 20); do", start)
            snippet = text[start:end]
            runner = "set -euo pipefail\ndie() { printf 'erro: %s\\n' \"$*\" >&2; exit 1; }\nMANIFEST=\"$1\"\n" + snippet + "printf '%s\\n' \"$READY_WS\" \"$READY_PR\"\n"
            clean = os.environ.copy()
            clean.pop("AI_MEMORY_READY_WORKSPACE", None)
            clean.pop("AI_MEMORY_READY_PROJECT", None)
            selected = subprocess.run(
                ["bash", "-c", runner, "select", str(manifest)],
                env=clean,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(selected.returncode, 0, selected.stderr)
            self.assertEqual(selected.stdout.splitlines(), ["default", "dev"])
            incomplete = os.environ.copy()
            incomplete["AI_MEMORY_READY_WORKSPACE"] = "default"
            incomplete.pop("AI_MEMORY_READY_PROJECT", None)
            half = subprocess.run(
                ["bash", "-c", runner, "select", str(manifest)],
                env=incomplete,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(half.returncode, 0)
            self.assertIn("incompleto", half.stderr)
            both = os.environ.copy()
            both["AI_MEMORY_READY_WORKSPACE"] = "override-ws"
            both["AI_MEMORY_READY_PROJECT"] = "override-pr"
            overridden = subprocess.run(
                ["bash", "-c", runner, "select", str(root / "missing-manifest.json")],
                env=both,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(overridden.returncode, 0, overridden.stderr)
            self.assertEqual(overridden.stdout.splitlines(), ["override-ws", "override-pr"])
        self.assertEqual(hashlib.sha256(installer.read_bytes()).hexdigest(), before)

    def test_config_allowlist_rejects_a_missing_file_without_creating_an_outbox(self):
        from reliable_mcp.safe import ConfigError, load_public_allowlist

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / "config.toml"
            config.write_text(
                "\n".join(
                    [
                        'allowed_hosts = ["dev.example.ts.net"]',
                        'cors_allow_origins = ["https://dev.example.ts.net"]',
                        'token_pepper = "do-not-print"',
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            hosts, origins = load_public_allowlist(config)
            shown = repr((hosts, origins))
            self.assertIn("127.0.0.1", hosts)
            self.assertIn("localhost", hosts)
            self.assertIn("::1", hosts)
            self.assertIn("dev.example.ts.net", hosts)
            self.assertEqual(origins, ["https://dev.example.ts.net"])
            self.assertNotIn("do-not-print", shown)
            missing = root / "missing.toml"
            with self.assertRaises(ConfigError):
                load_public_allowlist(missing)
            outbox = root / "outbox"
            failed = subprocess.run(
                [
                    "python3",
                    str(ROOT / "reliable-mcp"),
                    "serve",
                    "--listen",
                    "127.0.0.1:0",
                    "--backend",
                    "http://127.0.0.1:9",
                    "--outbox",
                    str(outbox),
                    "--config",
                    str(missing),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
                timeout=5,
            )
            self.assertEqual(failed.returncode, 1, failed.stderr)
            self.assertIn("config not found", failed.stderr)
            self.assertFalse(outbox.exists())
            self.assertNotIn("do-not-print", failed.stderr)


if __name__ == "__main__":
    import unittest

    unittest.main()
