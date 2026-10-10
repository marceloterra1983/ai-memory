"""The installed binary has no CAS yet. A scoped write through the relay must not land."""

from __future__ import annotations

import os
import signal
import socket
import subprocess
import tempfile
import time
import unittest
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROD_DATA = Path.home() / ".local" / "share" / "ai-memory"
MARKER = "RELAY-SYNTH-9f3c1a-do-not-land"


def _binary():
    override = os.environ.get("AI_MEMORY_BIN")
    if override and Path(override).is_file() and os.access(override, os.X_OK):
        return Path(override)
    for candidate in (Path.home() / ".local" / "bin" / "ai-memory", Path("/usr/bin/ai-memory")):
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    return None


def _free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def _scrub(text, secret):
    if secret:
        return text.replace(secret, "[redacted]")
    return text


class RealBinaryCase(unittest.TestCase):
    def test_current_binary_is_rejected_and_a_scoped_write_does_not_land(self):
        binary = _binary()
        if binary is None:
            self.skipTest("ai-memory binary is not installed")
        port = _free_port()
        self.assertNotEqual(port, 49374)
        self.assertNotEqual(port, 49376)
        secret = os.environ.get("AI_MEMORY_AUTH_TOKEN")
        with tempfile.TemporaryDirectory(prefix="reliable-mcp-bin-") as tmp:
            root = Path(tmp)
            data = root / "data"
            outbox = root / "delivery"
            data.mkdir()
            config = root / "config.toml"
            config.write_text("# synthetic ingress proof\n", encoding="utf-8")
            env = os.environ.copy()
            for key in list(env):
                if key in {"AI_MEMORY_AUTH_TOKEN", "AI_MEMORY_SERVER_URL"} or key.startswith("AI_MEMORY_LLM"):
                    env.pop(key, None)
            env["AI_MEMORY_DATA_DIR"] = str(data)
            stdout_path = root / "stdout.txt"
            stderr_path = root / "stderr.txt"
            stdout_file = stdout_path.open("wb")
            stderr_file = stderr_path.open("wb")
            proc = subprocess.Popen(
                [
                    str(binary),
                    "--data-dir",
                    str(data),
                    "--config",
                    str(config),
                    "serve",
                    "--transport",
                    "http",
                    "--bind",
                    f"127.0.0.1:{port}",
                    "--workspace",
                    "relay-synth",
                    "--project",
                    "relay-synth",
                    "--no-watcher",
                ],
                cwd=root,
                env=env,
                stdout=stdout_file,
                stderr=stderr_file,
                start_new_session=True,
            )
            try:
                self._assert_isolated(proc.pid, binary, data)
                self._wait_health(proc, port, stderr_path, secret)
                self._assert_isolated(proc.pid, binary, data)
                from reliable_mcp.ready import probe
                from reliable_mcp.relay import Relay
                from tests.http import rpc, tool_text

                ok, reason = probe(f"http://127.0.0.1:{port}", "relay-synth", "relay-synth", per_call=2.5, budget=8)
                self.assertFalse(ok)
                self.assertIn("create_only", reason)
                relay = Relay(
                    listen="127.0.0.1:0",
                    backend=f"http://127.0.0.1:{port}",
                    outbox=outbox,
                    auto_drain_seconds=0,
                )
                relay.start()
                try:
                    status, _, message = rpc(
                        f"{relay.url}/mcp",
                        "tools/call",
                        {
                            "name": "memory_write_page",
                            "arguments": {
                                "path": "notes/synth.md",
                                "body": f"# Synth\n\n{MARKER}\n",
                                "workspace": "relay-synth",
                                "project": "relay-synth",
                            },
                        },
                        timeout=8,
                    )
                    self.assertEqual(status, 200)
                    if "error" not in message:
                        receipt = tool_text(message)
                        self.assertIsNone(receipt["page_id"])
                        self.assertNotEqual(receipt.get("status"), "DELIVERED")
                    self.assertIsNone(self._marker_in(data))
                    self.assertIsNotNone(self._marker_in(outbox))
                finally:
                    relay.stop()
            finally:
                self._stop(proc, binary)
                stdout_file.close()
                stderr_file.close()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass

    def _wait_health(self, proc, port, stderr_path, secret):
        deadline = time.monotonic() + 30
        last = ""
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                err = _scrub(stderr_path.read_text(errors="replace")[-2000:], secret)
                self.fail(f"binary exited {proc.returncode}: {err}")
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=0.5) as response:
                    if response.status == 200:
                        return
            except OSError as exc:
                last = type(exc).__name__
            time.sleep(0.2)
        self.fail(f"healthz did not answer ({last})")

    def _assert_isolated(self, pid, binary, data):
        exe = os.readlink(f"/proc/{pid}/exe")
        self.assertEqual(os.path.realpath(exe), os.path.realpath(binary))
        cmdline = Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\x00", b" ").decode()
        self.assertIn(str(data), cmdline)
        self.assertNotIn("49374", cmdline)
        for fd in Path(f"/proc/{pid}/fd").iterdir():
            try:
                target = os.readlink(fd)
            except OSError:
                continue
            self.assertNotIn(str(PROD_DATA), target)

    def _marker_in(self, root: Path):
        if not root.exists():
            return None
        for dirpath, _dirs, filenames in os.walk(root):
            for name in filenames:
                path = Path(dirpath) / name
                try:
                    if MARKER.encode() in path.read_bytes():
                        return path
                except OSError:
                    continue
        return None

    def _stop(self, proc, binary):
        if proc.poll() is not None:
            return
        exe = os.readlink(f"/proc/{proc.pid}/exe")
        if os.path.realpath(exe) != os.path.realpath(binary):
            raise RuntimeError(f"refuse to signal {exe}")
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            exe = os.readlink(f"/proc/{proc.pid}/exe")
            if os.path.realpath(exe) != os.path.realpath(binary):
                raise RuntimeError(f"refuse to signal {exe}")
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
