"""Boundaries shared by the synthetic runner and its read-only benchmark."""

import json
import sqlite3
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

LAB_KIND = "ai-memory-quality-synthetic-v1"
SYNTHETIC_TOKEN = "synthetic-quality-lab-root-token"


def validate_url(server_url):
    parsed = urllib.parse.urlsplit(server_url)
    if (parsed.scheme != "http" or parsed.hostname != "127.0.0.1"
            or parsed.username or parsed.password or not parsed.port
            or parsed.port == 49374 or parsed.path not in ("", "/")
            or parsed.query or parsed.fragment):
        raise ValueError("laboratory URL must be explicit http://127.0.0.1:PORT (PORT != 49374)")
    return server_url.rstrip("/")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("laboratory HTTP redirects are refused")


def request(server_url, endpoint, payload=None, authorized=True):
    url = validate_url(server_url) + endpoint
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
    if authorized:
        headers["Authorization"] = "Bearer " + SYNTHETIC_TOKEN
    raw = None if payload is None else json.dumps(payload).encode()
    try:
        with urllib.request.build_opener(NoRedirect).open(
                urllib.request.Request(url, data=raw, headers=headers), timeout=10) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        # The caller can assert refusal without printing captured content.
        return error.code, ""


def mcp_call(server_url, name, arguments):
    status, raw = request(server_url, "/mcp", {
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
    if status != 200:
        raise ValueError(f"laboratory MCP {name} returned HTTP {status}")
    if raw.startswith("event:") or raw.startswith("data:"):
        raw = next(line[5:].strip() for line in raw.splitlines() if line.startswith("data:"))
    result = json.loads(raw)
    if "error" in result or result.get("result", {}).get("isError"):
        raise ValueError(f"laboratory MCP {name} refused the request")
    return json.loads(result["result"]["content"][0]["text"])


def open_lab(lab_dir, server_url, db, workspace):
    root = Path(lab_dir).resolve()
    database = Path(db).resolve()
    url = validate_url(server_url)
    manifest = json.loads((root / "lab.json").read_text())
    expected = {"kind": LAB_KIND, "server_url": url, "db": str(database), "workspace": workspace}
    if manifest != expected or database != root / "data/db/memory.sqlite":
        raise ValueError("URL, DB and workspace must match the synthetic laboratory manifest")
    status, raw = request(url, "/admin/status")
    if status != 200 or Path(json.loads(raw)["db_path"]).resolve() != database:
        raise ValueError("server status does not identify the explicit laboratory DB")
    connection = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)
    connection.execute("PRAGMA query_only=ON")
    return connection


def wait_for(predicate, label, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f"synthetic laboratory timed out: {label}")
