"""Lossless native-hook delivery proofs using synthetic files and real SQLite."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import sqlite3
import stat
import subprocess
import sys
import tempfile
import tracemalloc
import unittest
import time
from contextlib import closing
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlencode, urlsplit

from reliable_mcp.backend import Exchange

ORIGIN = "http://127.0.0.1:49374"
SECRET_BODY = "SYNTHETIC-HOOK-BODY-do-not-log"
SECRET_TOKEN = "SYNTHETIC-HOOK-TOKEN-do-not-log"
WORKSPACE_A = bytes.fromhex("11" * 16)
WORKSPACE_B = bytes.fromhex("22" * 16)
PROJECT_A = bytes.fromhex("33" * 16)
PROJECT_B = bytes.fromhex("44" * 16)
COMPLETED_AT = 1_791_642_000_123


class CheckpointClient:
    """Replace the socket only; persistence and scoped receipt lookup stay real."""

    def __init__(self, case, *, complete=True, status=200, callback=None,
                 single_complete=False, single_status=202, single_claim=False):
        self.case = case
        self.complete = complete
        self.status = status
        self.callback = callback
        self.single_complete = single_complete
        self.single_status = single_status
        self.single_claim = single_claim
        self.sent = []
        self.paths = []

    def exchange(self, method, path, headers, body):
        self.case.assertEqual(method, "POST")
        self.paths.append(path)
        batched = path == "/hook/batch"
        if batched:
            items = json.loads(body)
            self.case.assertIsInstance(items, list)
        else:
            self.case.assertEqual(urlsplit(path).path, "/hook")
            items = [{"url": ORIGIN + path, "body": json.loads(body)}]
        self.case.assertGreater(len(items), 0)
        for item in items:
            self.case.assertEqual(set(item), {"url", "body"})
            self.case.assertIsInstance(item["body"], dict)
            self.case.assertEqual(urlsplit(item["url"]).netloc, "127.0.0.1:49374")
            self.case.assertEqual(urlsplit(item["url"]).path, "/hook")
            query = parse_qs(urlsplit(item["url"]).query)
            self.case.assertEqual(query["workspace"], ["workspace-a"])
            self.case.assertEqual(query["project"], ["demo"])
            self.sent.append((query["ingest_key"][0], dict(headers), item["body"]))
            if self.complete if batched else self.single_complete:
                self.case.checkpoint(query["ingest_key"][0])
            elif not batched and self.single_claim:
                self.case.checkpoint(query["ingest_key"][0], completed_at=None)
        if self.callback:
            callback, self.callback = self.callback, None
            callback()
        status = self.status if batched else self.single_status
        if status is None:
            return Exchange(None, [], b"", "connection_lost")
        response_headers = [("Location", "http://198.51.100.9/hook/batch")] if status == 302 else []
        response_body = json.dumps({"accepted": len(items)}).encode() if batched else b"queued"
        return Exchange(status, response_headers, response_body)


class HookDeliveryCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.spool = self.root / "hook-spool"
        self.spool.mkdir(mode=0o700)
        self.archive = self.root / "archive"
        self.database = self.root / "memory.sqlite"
        with closing(sqlite3.connect(self.database)) as database, database:
            database.executescript(
                "CREATE TABLE workspaces(id BLOB PRIMARY KEY, name TEXT NOT NULL UNIQUE);"
                "CREATE TABLE projects(id BLOB PRIMARY KEY, workspace_id BLOB NOT NULL, name TEXT NOT NULL);"
                "CREATE TABLE ingest_keys(project_id BLOB NOT NULL, key TEXT NOT NULL,"
                " seen_at INTEGER NOT NULL, completed_at INTEGER, PRIMARY KEY(project_id, key));"
            )
            database.executemany("INSERT INTO workspaces VALUES(?, ?)", [(WORKSPACE_A, "workspace-a"), (WORKSPACE_B, "workspace-b")])
            database.executemany("INSERT INTO projects VALUES(?, ?, ?)", [(PROJECT_A, WORKSPACE_A, "demo"), (PROJECT_B, WORKSPACE_B, "demo")])
        self.drainers = []
        self.sequence = 0

    def tearDown(self):
        for drainer in self.drainers:
            drainer.close()
        self.tmp.cleanup()

    def drainer(self, client=None):
        from reliable_mcp.hook_delivery import HookDelivery

        drainer = HookDelivery(self.spool, self.database, self.archive, ORIGIN, client=client or CheckpointClient(self))
        self.drainers.append(drainer)
        return drainer

    def entry(self, key="synthetic-key", **changes):
        event = changes.pop("event", "post-tool-use")
        query = {"event": event, "agent": "codex", "workspace": "workspace-a", "project": "demo", "ingest_key": key}
        entry = {
            "url": ORIGIN + "/hook?" + urlencode(query),
            "body": json.dumps({"session_id": "synthetic-session", "tool_name": "Read", "tool_response": SECRET_BODY}),
            "created_ms": 1_700_000_000_000,
            "auth_mode": "none",
            "attempts": 0,
        }
        entry.update(changes)
        return entry

    def write_entry(self, key="synthetic-key", **changes):
        self.sequence += 1
        entry = self.entry(key, **changes)
        path = self.spool / f"{entry['created_ms']:013}-42-{self.sequence:016x}.json"
        # Whitespace is intentional: successful archiving must retain raw bytes.
        raw = (json.dumps(entry, indent=2) + "\n").encode()
        path.write_bytes(raw)
        path.chmod(0o600)
        return path, raw

    def checkpoint(self, key, project=PROJECT_A, completed_at=COMPLETED_AT):
        with closing(sqlite3.connect(self.database)) as database, database:
            database.execute(
                "INSERT INTO ingest_keys VALUES(?, ?, ?, ?) ON CONFLICT(project_id,key) DO UPDATE SET completed_at=excluded.completed_at",
                (project, key, COMPLETED_AT - 1, completed_at),
            )

    def receipt_path(self, entry_path):
        return self.archive / (entry_path.name + ".receipt.json")

    def assert_private(self, path, mode):
        self.assertFalse(path.is_symlink())
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), mode)

    def assert_content_free(self, result):
        self.assertEqual(set(result), {"delivered", "pending", "unconfirmed", "errors"})
        for value in result.values():
            self.assertIsInstance(value, int)
            self.assertGreaterEqual(value, 0)
        rendered = json.dumps(result)
        self.assertNotIn(SECRET_BODY, rendered)
        self.assertNotIn(SECRET_TOKEN, rendered)

    def test_completed_sql_receipt_archives_raw_privately_before_removing_spool(self):
        path, raw = self.write_entry()
        drainer = self.drainer()
        original_unlink = Path.unlink
        removed = []

        def check_archive_before_unlink(candidate, *args, **kwargs):
            if candidate == path:
                self.assertEqual((self.archive / path.name).read_bytes(), raw)
                receipt = json.loads(self.receipt_path(path).read_text())
                self.assertEqual(receipt["sha256"], hashlib.sha256(raw).hexdigest())
                self.assertEqual(receipt["completed_at"], COMPLETED_AT)
                removed.append(candidate)
            return original_unlink(candidate, *args, **kwargs)

        with patch.object(Path, "unlink", check_archive_before_unlink):
            result = drainer.drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(result["pending"], 0)
        self.assertEqual(removed, [path])
        self.assertFalse(path.exists())
        self.assert_private(self.archive, 0o700)
        self.assert_private(self.archive / path.name, 0o600)
        self.assert_private(self.receipt_path(path), 0o600)
        self.assertNotIn(SECRET_BODY, self.receipt_path(path).read_text())
        self.assertNotIn(SECRET_TOKEN, self.receipt_path(path).read_text())
        self.assert_content_free(result)

    def test_failed_or_ack_only_delivery_never_changes_attempts_age_or_original_bytes(self):
        cases = [(None, False), (202, False), (200, False), (302, False), (429, False), (500, False)]
        for status, complete in cases:
            with self.subTest(status=status):
                path, raw = self.write_entry(f"hold-{status}", attempts=8, created_ms=1_700_000_000_000)
                before = path.stat()
                drainer = self.drainer(CheckpointClient(self, complete=complete, status=status))
                result = drainer.drain_once()
                self.assertEqual(result["delivered"], 0)
                self.assertEqual(path.read_bytes(), raw)
                after = path.stat()
                self.assertEqual(after.st_mtime_ns, before.st_mtime_ns)
                self.assertEqual(stat.S_IMODE(after.st_mode), 0o600)
                self.assertFalse(self.receipt_path(path).exists())
                self.assert_content_free(result)
                drainer.close()

    def test_seen_without_completed_checkpoint_is_not_delivery(self):
        path, raw = self.write_entry()
        self.checkpoint("synthetic-key", completed_at=None)
        result = self.drainer(CheckpointClient(self, complete=False)).drain_once()
        self.assertEqual(result["delivered"], 0)
        self.assertEqual(result["unconfirmed"], 1)
        self.assertEqual(path.read_bytes(), raw)

    def test_preclaimed_incomplete_event_does_not_block_new_events(self):
        pending, pending_raw = self.write_entry("claimed-in-progress")
        fresh, fresh_raw = self.write_entry("new-event")
        completed, _ = self.write_entry("completed-control")
        self.checkpoint("claimed-in-progress", completed_at=None)
        self.checkpoint("completed-control")
        client = CheckpointClient(self)
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 2)
        self.assertEqual(result["pending"], 1)
        self.assertEqual(result["unconfirmed"], 1)
        self.assertEqual(pending.read_bytes(), pending_raw)
        self.assertFalse(fresh.exists())
        self.assertFalse(completed.exists())
        self.assertEqual((self.archive / fresh.name).read_bytes(), fresh_raw)
        self.assertEqual([key for key, _, _ in client.sent], ["new-event"])

    def test_foreign_workspace_receipt_cannot_confirm_same_key_with_legitimate_control(self):
        foreign, raw = self.write_entry("foreign-only")
        good, _ = self.write_entry("legitimate")
        self.checkpoint("foreign-only", project=PROJECT_B)
        self.checkpoint("legitimate")
        result = self.drainer(CheckpointClient(self, complete=False)).drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(result["pending"], 1)
        self.assertEqual(foreign.read_bytes(), raw)
        self.assertFalse(good.exists())
        self.assertFalse(self.receipt_path(foreign).exists())

    def test_foreign_project_receipt_cannot_confirm_key_with_legitimate_control(self):
        project_c = bytes.fromhex("55" * 16)
        with closing(sqlite3.connect(self.database)) as database, database:
            database.execute("INSERT INTO projects VALUES(?, ?, ?)", (project_c, WORKSPACE_A, "other-project"))
        foreign, raw = self.write_entry("foreign-project")
        good, _ = self.write_entry("legitimate")
        self.checkpoint("foreign-project", project=project_c)
        self.checkpoint("legitimate")
        result = self.drainer(CheckpointClient(self, complete=False)).drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(foreign.read_bytes(), raw)
        self.assertFalse(good.exists())

    def test_completed_checkpoint_is_idempotent_without_reposting_event(self):
        path, raw = self.write_entry()
        self.checkpoint("synthetic-key")
        client = CheckpointClient(self)
        drainer = self.drainer(client)
        first = drainer.drain_once()
        second = drainer.drain_once()
        self.assertEqual(first["delivered"], 1)
        self.assertEqual(second["delivered"], 0)
        self.assertEqual(client.sent, [])
        self.assertEqual((self.archive / path.name).read_bytes(), raw)
        with closing(sqlite3.connect(self.database)) as database, database:
            self.assertEqual(database.execute("SELECT COUNT(*) FROM ingest_keys").fetchone()[0], 1)

    def test_lost_http_response_with_real_completed_receipt_does_not_repost(self):
        path, _ = self.write_entry()
        client = CheckpointClient(self, status=None)
        drainer = self.drainer(client)
        self.assertEqual(drainer.drain_once()["delivered"], 1)
        self.assertFalse(path.exists())
        self.assertEqual(drainer.drain_once()["delivered"], 0)
        self.assertEqual(len(client.sent), 1)

    def test_archive_collision_preserves_both_original_and_existing_file(self):
        path, raw = self.write_entry()
        self.archive.mkdir(mode=0o700)
        collided = self.archive / path.name
        collided.write_bytes(b"different-original-event\n")
        collided.chmod(0o600)
        result = self.drainer().drain_once()
        self.assertEqual(result["delivered"], 0)
        self.assertEqual(path.read_bytes(), raw)
        self.assertEqual(collided.read_bytes(), b"different-original-event\n")
        self.assertGreater(result["errors"], 0)

    def test_receipt_collision_is_not_overwritten_or_used_to_delete_original(self):
        path, raw = self.write_entry()
        self.archive.mkdir(mode=0o700)
        receipt_path = self.receipt_path(path)
        wrong = b'{"sha256":"wrong","completed_at":123}\n'
        receipt_path.write_bytes(wrong)
        receipt_path.chmod(0o600)
        result = self.drainer().drain_once()
        self.assertEqual(result["delivered"], 0)
        self.assertEqual(path.read_bytes(), raw)
        self.assertEqual(receipt_path.read_bytes(), wrong)
        self.assertGreater(result["errors"], 0)

    def test_fsync_failure_keeps_source_even_after_backend_checkpoint(self):
        path, raw = self.write_entry()
        drainer = self.drainer()
        with patch("os.fsync", side_effect=OSError("synthetic sync failure")):
            result = drainer.drain_once()
        self.assertEqual(result["delivered"], 0)
        self.assertGreater(result["errors"], 0)
        self.assertEqual(path.read_bytes(), raw)
        with closing(sqlite3.connect(self.database)) as database, database:
            self.assertEqual(database.execute("SELECT completed_at FROM ingest_keys WHERE key='synthetic-key'").fetchone()[0], COMPLETED_AT)

    def test_spool_symlink_is_refused_with_legitimate_control(self):
        target = self.root / "outside.json"
        target_raw = json.dumps(self.entry("outside")).encode()
        target.write_bytes(target_raw)
        link = self.spool / "1700000000000-42-0000000000000042.json"
        link.symlink_to(target)
        good, _ = self.write_entry("legitimate")
        client = CheckpointClient(self)
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertTrue(link.is_symlink())
        self.assertEqual(target.read_bytes(), target_raw)
        self.assertFalse(good.exists())
        self.assertEqual([key for key, _, _ in client.sent], ["legitimate"])

    def test_invalid_envelope_types_are_preserved_with_legitimate_control(self):
        entries = [[], 123, self.entry("bad-number", url=123),
                   self.entry("bad-object", url={}), self.entry("bad-list", url=[])]
        invalid = []
        for index, entry in enumerate(entries):
            path = self.spool / f"0000000000000-42-{index:016x}.json"
            raw = json.dumps(entry).encode()
            path.write_bytes(raw)
            path.chmod(0o600)
            invalid.append((path, raw))
        good, _ = self.write_entry("legitimate")
        client = CheckpointClient(self)
        try:
            result = self.drainer(client).drain_once()
        except (AttributeError, TypeError) as error:
            self.fail(f"invalid envelope blocked the valid control: {type(error).__name__}")
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(result["errors"], len(invalid))
        self.assertFalse(good.exists())
        for path, raw in invalid:
            self.assertEqual(path.read_bytes(), raw)
            self.assertFalse(self.receipt_path(path).exists())
        self.assertEqual([key for key, _, _ in client.sent], ["legitimate"])
        self.assert_content_free(result)

    def test_fifo_source_is_refused_without_blocking_legitimate_control(self):
        fifo = self.spool / "0000000000000-42-0000000000000042.json"
        os.mkfifo(fifo, 0o600)
        good, raw = self.write_entry("legitimate")
        self.checkpoint("legitimate")
        command = [sys.executable, "-B", "-m", "reliable_mcp.hook_delivery",
                   "--spool", str(self.spool), "--database", str(self.database),
                   "--archive", str(self.archive), "--origin", ORIGIN, "--once"]
        try:
            process = subprocess.run(command, capture_output=True, timeout=2)
        except subprocess.TimeoutExpired:
            self.fail("FIFO blocked the valid control")
        self.assertEqual(process.returncode, 0, process.stderr.decode())
        result = json.loads(process.stdout)
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(result["errors"], 1)
        self.assertTrue(stat.S_ISFIFO(fifo.stat().st_mode))
        self.assertFalse(good.exists())
        self.assertEqual((self.archive / good.name).read_bytes(), raw)
        self.assertEqual(process.stderr, b"")
        self.assert_content_free(result)

    def test_archive_symlink_cannot_overwrite_foreign_target(self):
        path, raw = self.write_entry()
        self.archive.mkdir(mode=0o700)
        target = self.root / "foreign-target"
        target.write_bytes(b"must stay untouched")
        (self.archive / path.name).symlink_to(target)
        result = self.drainer().drain_once()
        self.assertEqual(result["delivered"], 0)
        self.assertEqual(path.read_bytes(), raw)
        self.assertEqual(target.read_bytes(), b"must stay untouched")
        self.assertTrue((self.archive / path.name).is_symlink())

    def test_bound_origin_hook_path_and_complete_scope_are_required_with_legitimate_control(self):
        bad_urls = [
            "http://198.51.100.9:49374/hook?workspace=workspace-a&project=demo&ingest_key=bad",
            "http://127.0.0.1:49377/hook?workspace=workspace-a&project=demo&ingest_key=bad",
            ORIGIN + "/mcp?workspace=workspace-a&project=demo&ingest_key=bad",
            ORIGIN + "/hook?project=demo&ingest_key=bad",
            ORIGIN + "/hook?workspace=workspace-a&ingest_key=bad",
            ORIGIN + "/hook?workspace=workspace-a&project=demo",
            ORIGIN + "/hook?workspace=workspace-a&workspace=workspace-b&project=demo&ingest_key=bad",
        ]
        invalid = [self.write_entry(f"bad-{index}", url=url) for index, url in enumerate(bad_urls)]
        good, _ = self.write_entry("legitimate")
        client = CheckpointClient(self)
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 1)
        for path, raw in invalid:
            self.assertEqual(path.read_bytes(), raw)
        self.assertFalse(good.exists())
        self.assertEqual([key for key, _, _ in client.sent], ["legitimate"])
        self.assert_content_free(result)

    def test_profile_and_oidc_are_refused_without_default_auth_fallback(self):
        oidc, oidc_raw = self.write_entry("oidc", auth_mode="oidc")
        profile, profile_raw = self.write_entry("profile", profile="remote")
        good, _ = self.write_entry("legitimate")
        client = CheckpointClient(self)
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(oidc.read_bytes(), oidc_raw)
        self.assertEqual(profile.read_bytes(), profile_raw)
        self.assertFalse(good.exists())
        self.assertEqual([key for key, _, _ in client.sent], ["legitimate"])

    def test_static_auth_stays_bound_to_its_event_and_out_of_receipt_and_counts(self):
        anonymous, _ = self.write_entry("anonymous")
        authenticated, _ = self.write_entry("authenticated", auth_mode="static", token=SECRET_TOKEN)
        client = CheckpointClient(self)
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 2)
        sent = {key: {name.lower(): value for name, value in headers.items()} for key, headers, _ in client.sent}
        self.assertNotIn("authorization", sent["anonymous"])
        self.assertEqual(sent["authenticated"]["authorization"], "Bearer " + SECRET_TOKEN)
        for path in (anonymous, authenticated):
            self.assertNotIn(SECRET_TOKEN, self.receipt_path(path).read_text())
            self.assertNotIn(SECRET_BODY, self.receipt_path(path).read_text())
        self.assert_content_free(result)

    def test_native_drain_lock_is_held_for_drainer_lifetime_and_released_on_close(self):
        drainer = self.drainer()
        with (self.spool / ".drain.lock").open("a+b") as other:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            drainer.drain_once()
            with self.assertRaises(BlockingIOError):
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            drainer.close()
            fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(other, fcntl.LOCK_UN)

    def test_new_arrivals_are_delivered_on_next_pass_without_losing_initial_events(self):
        first, raw = self.write_entry("initial")
        arrived = []

        def enqueue_while_posting():
            arrived.append(self.write_entry("arrived-during-pass"))

        client = CheckpointClient(self, callback=enqueue_while_posting)
        drainer = self.drainer(client)
        result = drainer.drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertEqual((self.archive / first.name).read_bytes(), raw)
        self.assertFalse(first.exists())
        second, second_raw = arrived[0]
        self.assertEqual(second.read_bytes(), second_raw)
        self.assertEqual(drainer.drain_once()["delivered"], 1)
        self.assertFalse(second.exists())
        self.assertEqual((self.archive / second.name).read_bytes(), second_raw)
        self.assertEqual([key for key, _, _ in client.sent], ["initial", "arrived-during-pass"])

    def assert_slow_lifecycle_event(self, event):
        key = f"slow-{event}"
        slow, slow_raw = self.write_entry(key, event=event)
        fresh, _ = self.write_entry(f"fresh-{event}")
        client = CheckpointClient(self, single_claim=True)
        drainer = self.drainer(client)
        result = drainer.drain_once()
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(result["pending"], 1)
        self.assertEqual(result["unconfirmed"], 1)
        self.assertEqual(slow.read_bytes(), slow_raw)
        self.assertFalse(fresh.exists())
        self.assertFalse(self.receipt_path(slow).exists())
        slow_url = urlsplit(json.loads(slow_raw)["url"])
        original_path = slow_url.path + "?" + slow_url.query
        self.assertEqual(client.paths, [original_path, "/hook/batch"])

        newer, _ = self.write_entry(f"newer-{event}")
        next_result = drainer.drain_once()
        self.assertEqual(next_result["delivered"], 1)
        self.assertEqual(next_result["pending"], 1)
        self.assertEqual(slow.read_bytes(), slow_raw)
        self.assertFalse(newer.exists())
        self.assertEqual(client.paths.count(original_path), 1)
        self.assertEqual([sent_key for sent_key, _, _ in client.sent], [key, f"fresh-{event}", f"newer-{event}"])

        self.checkpoint(key)
        self.assertEqual(drainer.drain_once()["delivered"], 1)
        self.assertFalse(slow.exists())
        self.assertEqual((self.archive / slow.name).read_bytes(), slow_raw)
        self.assertEqual(client.paths.count(original_path), 1)

    def test_pre_compact_uses_original_single_path_and_waits_for_receipt(self):
        self.assert_slow_lifecycle_event("pre-compact")

    def test_session_end_uses_original_single_path_and_waits_for_receipt(self):
        self.assert_slow_lifecycle_event("session-end")

    def test_host_down_pass_has_at_most_four_batches_and_preserves_every_event(self):
        originals = [self.write_entry(f"budget-{index:03}") for index in range(161)]
        client = CheckpointClient(self, complete=False, status=None)
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 0)
        self.assertEqual(result["pending"], 161)
        self.assertGreater(client.paths.count("/hook/batch"), 0)
        self.assertLessEqual(client.paths.count("/hook/batch"), 4)
        self.assertLessEqual(len(client.sent), 128)
        self.assertGreaterEqual(len(originals) - len(client.sent), 33)
        for path, raw in originals:
            self.assertEqual(path.read_bytes(), raw)
            self.assertFalse(self.receipt_path(path).exists())
        self.assert_content_free(result)

    def test_large_backlog_uses_memory_for_one_batch_instead_of_every_payload(self):
        calls = []

        class DisconnectedClient:
            def exchange(self, *_):
                calls.append(1)
                return Exchange(None, [], b"", "synthetic_down")

        body = json.dumps({"session_id": "synthetic-session", "tool_response": "x" * (128 * 1024)})
        originals = [self.write_entry(f"large-{index:03}", body=body)[0] for index in range(100)]
        drainer = self.drainer(DisconnectedClient())
        tracemalloc.start()
        try:
            result = drainer.drain_once()
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 24 * 1024 * 1024,
                        "pending backlog bodies must not all remain resident before sending")
        self.assertEqual(result["delivered"], 0)
        self.assertEqual(result["pending"], 100)
        self.assertGreater(len(calls), 0)
        self.assertLessEqual(len(calls), 4)
        for path in originals:
            self.assertEqual(json.loads(path.read_bytes())["body"], body)
            self.assertFalse(self.receipt_path(path).exists())

    def test_claim_created_after_scan_is_not_reposted_into_its_active_tail(self):
        first, _ = self.write_entry("first-control")
        claimed, raw = self.write_entry("claimed-during-pass", event="pre-compact")
        fresh, _ = self.write_entry("fresh-control")
        client = CheckpointClient(self, callback=lambda: self.checkpoint("claimed-during-pass", completed_at=None))
        result = self.drainer(client).drain_once()
        self.assertEqual(result["delivered"], 2)
        self.assertEqual(result["unconfirmed"], 1)
        self.assertEqual(claimed.read_bytes(), raw)
        self.assertFalse(first.exists())
        self.assertFalse(fresh.exists())
        self.assertEqual([key for key, _, _ in client.sent], ["first-control", "fresh-control"])

    def test_unconfirmed_prefix_cools_down_so_new_arrivals_are_not_starved(self):
        originals = [self.write_entry(f"unconfirmed-{index:03}") for index in range(128)]
        client = CheckpointClient(self, complete=False, status=200)
        drainer = self.drainer(client)
        drainer.drain_once()
        fresh, _ = self.write_entry("fresh-control")
        client.complete = True
        second = drainer.drain_once()
        self.assertFalse(fresh.exists())
        self.assertEqual(second["delivered"], 1)
        self.assertEqual(len(client.sent), 129)
        for path, raw in originals:
            self.assertEqual(path.read_bytes(), raw)

    def test_cli_waits_for_existing_guard_then_archives_confirmed_event(self):
        path, raw = self.write_entry("guard-handoff")
        self.checkpoint("guard-handoff")
        guard = self.drainer()
        command = [sys.executable, "-B", "-m", "reliable_mcp.hook_delivery",
                   "--spool", str(self.spool), "--database", str(self.database),
                   "--archive", str(self.archive), "--origin", ORIGIN, "--once"]
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            time.sleep(0.15)
            self.assertIsNone(process.poll())
            self.assertEqual(path.read_bytes(), raw)
            guard.close()
            stdout, stderr = process.communicate(timeout=3)
            self.assertEqual(process.returncode, 0, stderr.decode())
            self.assertEqual(json.loads(stdout)["delivered"], 1)
            self.assertEqual(stderr, b"")
            self.assertEqual((self.archive / path.name).read_bytes(), raw)
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate()


if __name__ == "__main__":
    unittest.main()
