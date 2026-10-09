#!/usr/bin/env python3
"""Create a fresh, owned synthetic lab and run the quality evidence gates."""

import argparse
import hashlib
import importlib.util
import json
import socket
import subprocess
import sys
import urllib.error
import urllib.parse
from pathlib import Path

from lab import LAB_KIND, SYNTHETIC_TOKEN, open_lab, request, validate_url, wait_for

HERE = Path(__file__).resolve().parent


def module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def environment(root):
    # No inherited config, provider keys, agent stores or user git settings.
    return {"PATH": "/usr/bin:/bin", "HOME": str(root / "home"),
            "USERPROFILE": str(root / "home"), "AI_MEMORY_HOME": str(root / "home"),
            "AI_MEMORY_AUTH_TOKEN": SYNTHETIC_TOKEN,
            "XDG_CONFIG_HOME": str(root / "home/.config"), "XDG_DATA_HOME": str(root / "home/.local/share"),
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": str(root / "empty-gitconfig"), "RUST_LOG": "warn"}


def write_page(server_url, workspace, page):
    status, raw = request(server_url, "/admin/write-page", {"workspace": workspace, "tier": "semantic", **page})
    assert status == 200, f"synthetic write-page refused with HTTP {status}"
    result = json.loads(raw)
    assert result.get("page_id") and result.get("checkpoint"), "synthetic page write lacks version/checkpoint"
    return result


def seed(connection, server_url, workspace):
    fixtures = json.loads((HERE / "baseline/fixtures.json").read_text())
    receipts = [write_page(server_url, workspace, page) for page in fixtures["historical_pages"] + fixtures["pages"]]
    # Same page identity in another workspace must not improve this benchmark.
    decoy = {"project": "regente", "path": "concepts/cobalt-source.md", "title": "Synthetic decoy", "body": "# Synthetic decoy\ncobaltpolicy uses decoy-999, never cobalt-source-17."}
    write_page(server_url, workspace + "-decoy", decoy)
    for project, path in [("regente", "decisions/cobalt-route.md"), ("meusagentes", "decisions/amber-route.md")]:
        counts = connection.execute(
            "SELECT count(*), sum(p.is_latest) FROM pages p JOIN projects pr ON pr.id=p.project_id "
            "JOIN workspaces ws ON ws.id=p.workspace_id WHERE ws.name=? AND pr.name=? AND p.path=?",
            (workspace, project, path),
        ).fetchone()
        assert counts == (2, 1), "temporal fixture did not preserve its previous version"
    write_page(server_url, workspace, {"project": "homeassistant-mcp-only", "path": "concepts/homeassistant-synthetic.md", "body": "# Synthetic Home Assistant\nhomeassistantsynthetic manual page contains homeassistant-page-only-53."})
    return receipts


def curation_noop(connection, server_url, workspace):
    row = connection.execute(
        "SELECT p.id, p.body FROM pages p JOIN projects pr ON pr.id=p.project_id "
        "JOIN workspaces ws ON ws.id=p.workspace_id WHERE ws.name=? AND pr.name='regente' "
        "AND p.path='concepts/cobalt-source.md' AND p.is_latest=1", (workspace,),
    ).fetchone()
    import uuid
    payload = {"workspace": workspace, "project": "regente", "path": "concepts/cobalt-source.md",
               "expected_head_id": str(uuid.UUID(bytes=row[0])),
               "expected_body_sha256": hashlib.sha256(row[1].encode()).hexdigest()}
    status, _ = request(server_url, "/admin/curate-relations", payload, authorized=False)
    assert status == 401, "metadata endpoint admitted an anonymous caller"
    status, raw = request(server_url, "/admin/curate-relations", payload)
    assert status == 200, f"synthetic metadata dryrun refused with HTTP {status}"
    result = json.loads(raw)
    assert not result["applied"] and result["removed"] == 0, "clean synthetic relation dryrun mutated a page"
    stale = {**payload, "expected_head_id": str(uuid.uuid4())}
    status, _ = request(server_url, "/admin/curate-relations", stale)
    assert status == 409, "stale metadata head did not fail closed"
    status, _ = request(server_url, "/admin/curate-relations", {**payload, "frontmatter": {"title": "must-not-be-ignored"}})
    assert status in (400, 422), "unknown metadata DTO field was silently ignored"
    return {"case": "curation-http", "dryrun_default": True, "clean_noop": True, "stale_refused": True, "unknown_field_refused": True, "anonymous_refused": True}


def readiness(server_url, server):
    if server.poll() is not None:
        raise ValueError("owned laboratory server exited before readiness")
    try:
        return request(server_url, "/admin/status")[0] == 200
    except urllib.error.URLError:
        return False


def execute(binary, root, server_url, workspace, baseline_only=False):
    url = validate_url(server_url)
    if not binary.is_file() or not binary.is_absolute():
        raise ValueError("--binary must identify an existing absolute binary path")
    if root.exists():
        raise ValueError("--lab-dir must be new; existing laboratories are never overwritten")
    if workspace in ("default", "_global") or not workspace:
        raise ValueError("use a distinct non-default synthetic workspace")
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        probe.bind(("127.0.0.1", urllib.parse.urlsplit(url).port))
    root.mkdir(parents=True, mode=0o700)
    for child in ("home", "data", "discovery-root/backend"):
        (root / child).mkdir(parents=True)
    (root / "empty-gitconfig").write_text("")
    config = root / "config.toml"
    config.write_text('backfill_on_start = false\nconsolidate_on_session_end = false\nrun_autowire = false\nlog_level = "warn"\n[maintenance]\nenabled = false\n[auto_improve]\non_session_end = false\n[auto_improve.scheduler]\nenabled = false\n')
    env = environment(root)
    subprocess.run(["git", "init", "--quiet", str(root / "discovery-root")], env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    db = root / "data/db/memory.sqlite"
    (root / "lab.json").write_text(json.dumps({"kind": LAB_KIND, "server_url": url, "db": str(db), "workspace": workspace}, indent=2) + "\n")
    command = [str(binary), "--data-dir", str(root / "data"), "--config", str(config), "serve", "--transport", "http", "--bind", f"127.0.0.1:{urllib.parse.urlsplit(url).port}", "--workspace", workspace, "--project", "scratch", "--no-watcher"]
    with (root / "server.log").open("w") as log:
        server = subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
        connection = None
        try:
            wait_for(lambda: readiness(url, server), "server readiness", seconds=30)
            connection = open_lab(root, url, db, workspace)
            receipts = seed(connection, url, workspace)
            formats = []
            if not baseline_only:
                integration = module(HERE / "integracoes/run.py", "quality_integrations")
                formats = integration.run(connection, url, workspace)
                formats.append(integration.discover(connection, url, workspace, root / "discovery-root"))
                formats.append(curation_noop(connection, url, workspace))
            query = {"workspace": workspace, "project": "homeassistant-mcp-only", "query": "homeassistantsynthetic"}
            from lab import mcp_call
            hits = mcp_call(url, "memory_query", query)
            assert hits.get("hits"), "MCP-only Home Assistant page recall failed"
            count = connection.execute("SELECT count(*) FROM observations o JOIN projects pr ON pr.id=o.project_id JOIN workspaces ws ON ws.id=o.workspace_id WHERE ws.name=? AND pr.name='homeassistant-mcp-only'", (workspace,)).fetchone()[0]
            assert count == 0, "MCP-only fixture unexpectedly captured observations"
            formats.append({"case": "homeassistant-mcp-only", "page_recall": True, "observations": 0, "valid": True})
            benchmark = module(HERE / "baseline/bench.py", "quality_benchmark")
            # Prove the DB helper enforces the full coordinate, using real DB rows.
            bodies = benchmark.corpos_da_pagina(connection, {"path": "concepts/cobalt-source.md"}, workspace, ["regente"])
            assert len(bodies) == 1 and "decoy-999" not in bodies[0], "benchmark merged foreign-workspace bodies"
            result = benchmark.run(root, url, db, workspace, root / "bench.jsonl")
            for field in ("projeto", "linha_de_base"):
                assert result[field]["recall@3_k"] == result["perguntas"] and not result[field]["erros"], f"synthetic {field} retrieval gate failed"
            forbidden = connection.execute("SELECT count(*) FROM projects WHERE name='forbidden-injection-project'").fetchone()[0]
            assert forbidden == 0, "retrieved injection caused an unauthorized action"
            evidence = {"kind": LAB_KIND, "formats": formats, "synthetic_seed_checkpoints": len(receipts), "baseline": result, "provider": "none", "injection_executed": False}
            (root / "evidence.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
            return evidence
        finally:
            if connection is not None:
                connection.close()
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "lab-dir", "server-url", "workspace"):
        parser.add_argument("--" + flag, required=True)
    parser.add_argument("--baseline-only", action="store_true", help="compare retrieval with an older binary without requiring the new curation endpoint")
    args = parser.parse_args()
    try:
        result = execute(Path(args.binary), Path(args.lab_dir).resolve(), args.server_url, args.workspace, args.baseline_only)
    except (ValueError, OSError, AssertionError, KeyError, json.JSONDecodeError) as error:
        # Never echo request/response bodies or a server log to diagnose failure.
        parser.error(f"synthetic laboratory failed: {type(error).__name__}: {error}")
    print(json.dumps({"formats_and_controls": len(result["formats"]), "recall_at_3": result["baseline"]["projeto"]["recall@3"], "evidence": str(Path(args.lab_dir).resolve() / "evidence.json")}, indent=2))


if __name__ == "__main__":
    main()
