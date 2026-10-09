"""Safety and fixture contracts of the isolated synthetic quality lab."""

import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from lab import validate_url


class LabContract(unittest.TestCase):
    def test_lab_url_refuses_production_remote_credentials_and_redirect_shapes(self):
        # Removing the loopback, port or credential guards would allow a lab
        # runner to address a real/global server before seeding synthetic data.
        for url in ["", "http://127.0.0.1:49374", "http://localhost:49474", "https://127.0.0.1:49474", "http://example.com:49474", "http://secret@127.0.0.1:49474", "http://127.0.0.1:49474/mcp", "http://127.0.0.1:49474?token=synthetic"]:
            with self.subTest(url=url):
                with self.assertRaises(ValueError):
                    validate_url(url)
        self.assertEqual(validate_url("http://127.0.0.1:49474/"), "http://127.0.0.1:49474")

    def test_bench_requires_explicit_coordinates_before_any_read(self):
        # An inherited/global URL or DB default would bypass this refusal.
        result = subprocess.run(
            [sys.executable, str(ROOT / "baseline/bench.py")],
            capture_output=True, text=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--server-url", result.stderr)
        self.assertIn("--db", result.stderr)
        self.assertIn("--workspace", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_eval_has_executable_discriminating_facts_for_both_projects(self):
        fixture = ROOT / "baseline/fixtures.json"
        self.assertTrue(fixture.exists(), "executable synthetic corpus is missing")
        data = json.loads(fixture.read_text())
        queries = data["queries"]
        self.assertEqual({q["project"] for q in queries}, {"regente", "meusagentes"})
        self.assertTrue({"source", "exception", "temporal", "invalid-reference", "injection", "distinct-topics"} <= {q["cat"] for q in queries})
        for q in queries:
            bodies = [p["body"] for p in data["pages"] if p["project"] == q["project"] and p["path"] == q["expected_path"]]
            self.assertEqual(len(bodies), 1)
            self.assertTrue(all(term in bodies[0] for term in q["must"]))
            self.assertTrue(q["must"])
            self.assertTrue(all("placeholder" not in term.lower() for term in q["must"]))

    def test_long_exception_fact_occurs_after_six_kilobytes(self):
        data = json.loads((ROOT / "baseline/fixtures.json").read_text())
        body = next(p["body"] for p in data["pages"] if p["path"] == "concepts/cobalt-exception.md")
        self.assertGreater(body.encode().find(b"cobalt-exception-9"), 6000)

    def test_format_matrix_has_real_bodies_and_kimi_nested_casing_controls(self):
        rows = json.loads((ROOT / "integracoes/fixtures.json").read_text())
        self.assertEqual(len(rows), 22)
        by_shape = {row["shape"]: row for row in rows}
        self.assertEqual(by_shape["kimi-content-blocks"]["prompt"]["prompt"][0]["type"], "text")
        self.assertTrue(by_shape["cursor-workspace-roots"]["start"]["workspace_roots"])
        self.assertIn("conversationId", by_shape["antigravity-conversationId"]["start"])
        self.assertIn("session_id", by_shape["devin-query-bridge"]["query"])
        for row in rows:
            self.assertIn(row["fact"], json.dumps(row["prompt"]))
            self.assertNotEqual(row["start"], row["prompt"])


if __name__ == "__main__":
    unittest.main()
