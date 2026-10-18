"""Run only release quality gates, with no version, commit or tag operations."""

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[2]


class ReleaseGates(unittest.TestCase):
    def run_gates(self, deny_status=0, audit_status=0, deny_installed=True):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / "bin"
            scripts.mkdir()
            stubs = root / "stubs"
            stubs.mkdir()
            # Stop before every release mutation while executing the real gates.
            source = (REPO / "bin/release").read_text()
            gates = source.split("# ── update Cargo.toml version", 1)[0]
            script = scripts / "release"
            script.write_text(gates + '\nprintf "GATES_COMPLETED\\n"\n')
            for name in ("dirname", "date"):
                (stubs / name).symlink_to(shutil.which(name))
            cargo = stubs / "cargo"
            cargo.write_text(
                '#!/bin/bash\ncase "$1" in\n'
                f"deny) exit {deny_status} ;;\n"
                f"audit) exit {audit_status} ;;\n"
                "fmt|clippy|test) exit 0 ;;\n"
                "*) exit 99 ;;\nesac\n"
            )
            cargo.chmod(0o755)
            git = stubs / "git"
            git.write_text(
                '#!/bin/bash\nif [[ "$*" == "diff --check" ]]; then exit 0; fi\n'
                "exit 99\n"
            )
            git.chmod(0o755)
            for name in (("cargo-deny", "cargo-audit") if deny_installed else ("cargo-audit",)):
                (stubs / name).symlink_to(cargo)
            return subprocess.run(
                ["/bin/bash", str(script), "0.0.0"],
                cwd=root,
                env={"PATH": str(stubs), "TMPDIR": str(root)},
                text=True,
                capture_output=True,
                check=False,
            )

    def test_deny_failure_stops_before_release_mutations(self):
        result = self.run_gates(deny_status=42)
        self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
        self.assertNotIn("GATES_COMPLETED", result.stdout)

    def test_successful_gates_complete(self):
        result = self.run_gates()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("GATES_COMPLETED", result.stdout)

    def test_audit_remains_an_explicit_operator_warning(self):
        result = self.run_gates(audit_status=43)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("[warn] cargo audit", result.stdout)
        self.assertIn("GATES_COMPLETED", result.stdout)

    def test_missing_optional_deny_reports_skip(self):
        result = self.run_gates(deny_installed=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cargo-deny not installed", result.stdout)


if __name__ == "__main__":
    unittest.main()
