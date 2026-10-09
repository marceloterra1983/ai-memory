import importlib.util
import pathlib
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import tempfile
import unittest

HELPER = pathlib.Path(__file__).resolve().parents[1] / "curadoria/helper.py"
spec = importlib.util.spec_from_file_location("quality_curation", HELPER)
curation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(curation)


class CurationTests(unittest.TestCase):
    def test_only_exact_sentinels_are_removed_and_origins_stay_pending(self):
        rows = [{"head_id": "synthetic-id", "path": "notes/a.md", "body_sha256": "ab" * 32,
                 "frontmatter": {"relations": {"contradicts": ["None", "none.md", "Free title", "notes/none.md"],
                                                "fixes": ["N/A", "FALHA.md"]},
                                 "origin": {"session_id": "synthetic-origin"}}}]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "dev")
        self.assertEqual(plan["candidates"][0]["removed"], 3)
        self.assertEqual([p["raw_target"] for p in plan["pending"]], ["Free title", "notes/none.md", "FALHA.md"])
        self.assertTrue(all(p["origin"] == {"session_id": "synthetic-origin"} for p in plan["pending"]))
        self.assertFalse(curation.is_sentinel("not-none.md"))
        self.assertTrue(curation.is_sentinel("  Nenhuma.md "))

    def test_consolidator_top_level_origin_is_reported_without_inference(self):
        fm = {"session_id": "synthetic-session", "agent": "grok", "relations": {"fixes": ["Free title"]}}
        rows = [{"head_id": "synthetic", "path": "notes/a.md", "body_sha256": "ab" * 32, "frontmatter": fm}]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "regente")
        self.assertEqual(plan["pending"][0]["origin"], {"session_id": "synthetic-session", "agent": "grok"})
        fm["origin"] = {"custom": "preserve-exactly"}
        fm["sources"] = ["import:synthetic-source"]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "regente")
        self.assertEqual(plan["pending"][0]["origin"]["declared_origin"], fm["origin"])
        self.assertEqual(plan["pending"][0]["origin"]["session_id"], fm["session_id"])
        self.assertEqual(plan["pending"][0]["origin"]["sources"], fm["sources"])
        del fm["session_id"], fm["agent"]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "regente")
        self.assertEqual(plan["pending"][0]["origin"]["sources"], fm["sources"])

    def test_stale_preflight_cancels_every_apply(self):
        rows = [{"head_id": f"synthetic-{i}", "path": f"notes/{i}.md", "body_sha256": "ab" * 32,
                 "frontmatter": {"relations": {"causes": ["None"]}}} for i in range(2)]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "dev")
        calls = []
        def request(payload):
            calls.append(payload)
            if len(calls) == 2:
                raise curation.CurationError("HTTP 409")
            return {"page_id": payload["expected_head_id"], "removed": 1, "applied": False}
        with self.assertRaises(curation.CurationError):
            curation.execute_plan(plan, request, apply=True)
        self.assertEqual(len(calls), 2)
        self.assertTrue(all(p["dry_run"] for p in calls))

    def test_apply_requires_all_previews_then_keeps_checkpoint_receipts(self):
        rows = [{"head_id": "synthetic", "path": "notes/a.md", "body_sha256": "ab" * 32,
                 "frontmatter": {"relations": {"causes": ["None"]}}}]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "farol")
        calls = []
        def request(payload):
            calls.append(payload)
            return {"page_id": "synthetic" if payload["dry_run"] else "new",
                    "removed": 1, "applied": not payload["dry_run"],
                    "before_checkpoint": "before", "checkpoint": "after"}
        result = curation.execute_plan(plan, request, apply=True)
        self.assertEqual([p["dry_run"] for p in calls], [True, False])
        self.assertEqual(result[0]["checkpoint"], "after")

    def test_malformed_relation_values_are_pending_and_never_cleanup_candidates(self):
        rows = [{"head_id": "synthetic", "path": "notes/a.md", "body_sha256": "ab" * 32,
                 "frontmatter": {"relations": {"causes": "None"}, "origin": {"session_id": "source"}}}]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "dev")
        self.assertEqual(plan["candidates"], [])
        self.assertEqual(plan["pending"][0]["raw_target"], "None")
        rows[0]["frontmatter"]["relations"] = "None"
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "dev")
        self.assertEqual(plan["candidates"], [])
        self.assertEqual(plan["pending"][0]["source_fact"], {"field": "relations", "value": "None"})
        self.assertEqual(plan["pending"][0]["origin"], {"session_id": "source"})

    def test_explicit_server_refuses_redirect_without_forwarding_bearer(self):
        received = []
        class Foreign(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass
            def do_GET(self):
                received.append(self.headers.get("Authorization"))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{}')
        foreign = ThreadingHTTPServer(("127.0.0.1", 0), Foreign)
        class Authorized(BaseHTTPRequestHandler):
            redirect = True
            def log_message(self, *_args):
                pass
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                if self.redirect:
                    self.send_response(302)
                    self.send_header("Location", f"http://127.0.0.1:{foreign.server_port}/foreign")
                    self.end_headers()
                else:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps({"applied": False}).encode())
        authorized = ThreadingHTTPServer(("127.0.0.1", 0), Authorized)
        servers = (foreign, authorized)
        threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in servers]
        for thread in threads:
            thread.start()
        try:
            request = curation.server_request(f"http://127.0.0.1:{authorized.server_port}", "synthetic-only")
            with self.assertRaises(curation.CurationError):
                request({"dry_run": True})
            self.assertEqual(received, [])
            Authorized.redirect = False
            self.assertEqual(request({"dry_run": True}), {"applied": False})
        finally:
            for server in servers:
                server.shutdown()
                server.server_close()
            for thread in threads:
                thread.join()

    def test_confirmed_receipts_are_recorded_before_a_later_stale_request(self):
        rows = [{"head_id": f"synthetic-{i}", "path": f"notes/{i}.md", "body_sha256": "ab" * 32,
                 "frontmatter": {"relations": {"causes": ["None"]}}} for i in range(2)]
        plan = curation.build_plan(rows, "http://127.0.0.1:12345", "default", "dev")
        calls, saved = [], []
        def request(payload):
            calls.append(payload)
            if len(calls) == 4:
                raise curation.CurationError("HTTP 409")
            return {"page_id": payload["expected_head_id"], "removed": 1,
                    "applied": not payload["dry_run"], "before_checkpoint": "before", "checkpoint": "after"}
        with self.assertRaises(curation.CurationError):
            curation.execute_plan(plan, request, apply=True, record_receipts=lambda value: saved.extend(value))
        self.assertEqual(len(saved), 1)
        self.assertEqual(saved[0]["checkpoint"], "after")

    def test_qlmed_cannot_write_and_private_artifacts_have_restricted_mode(self):
        with self.assertRaises(curation.CurationError):
            curation.ensure_write_scope("default", "QLMED")
        with self.assertRaises(curation.CurationError):
            curation.ensure_write_scope("other", "dev")
        with tempfile.TemporaryDirectory() as tmp:
            target = pathlib.Path(tmp) / "pending.json"
            curation.write_private(target, {"pending": []})
            self.assertEqual(target.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
