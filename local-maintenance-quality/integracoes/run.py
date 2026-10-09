#!/usr/bin/env python3
"""Assert lifecycle -> capture -> zero-LLM summary -> MCP recall in the lab."""

import json
import sys
import urllib.parse
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lab import mcp_call, request, wait_for

FIXTURES = Path(__file__).with_name("fixtures.json")


def hook(server_url, workspace, project, agent, event, payload, extra=None):
    query = {"workspace": workspace, "project": project, "agent": agent, "event": event}
    query.update(extra or {})
    status, _ = request(server_url, "/hook?" + urllib.parse.urlencode(query), payload)
    if status != 202:
        raise AssertionError(f"synthetic {event} returned HTTP {status}")


def session_state(connection, session_id, workspace, project):
    return connection.execute(
        "SELECT s.ended_at, s.summary_page_id, s.agent_kind, "
        "(SELECT count(*) FROM observations o WHERE o.session_id=s.id) "
        "FROM sessions s JOIN workspaces ws ON ws.id=s.workspace_id "
        "JOIN projects pr ON pr.id=s.project_id WHERE s.id=? AND ws.name=? AND pr.name=?",
        (uuid.UUID(session_id).bytes, workspace, project),
    ).fetchone()


def run(connection, server_url, workspace):
    report = []
    for row in json.loads(FIXTURES.read_text()):
        project = "formats"
        for event, field in [("session-start", "start"), ("user-prompt-submit", "prompt"), ("session-end", "end")]:
            hook(server_url, workspace, project, row["agent"], event, row[field], row["query"])
        state = wait_for(lambda: ended_state(connection, row["session_id"], workspace, project), row["shape"])
        assert state[1], f"{row['shape']}: substantive session has no summary link"
        assert state[2] == row["agent"], f"{row['shape']}: incorrect agent attribution"
        assert state[3] == 3, f"{row['shape']}: incorrect lifecycle observation count"
        page = connection.execute(
            "SELECT path, body FROM pages WHERE id=? AND is_latest=1", (state[1],),
        ).fetchone()
        assert page and row["fact"] in page[1], f"{row['shape']}: summary lost synthetic fact"
        result = mcp_call(server_url, "memory_query", {"workspace": workspace, "project": project, "query": row["token"], "limit": 10})
        assert any(hit["path"] == page[0] for hit in result.get("hits", [])), f"{row['shape']}: MCP summary recall failed"

        receiver = str(uuid.uuid4())
        query = {"workspace": workspace, "project": project, "agent": "claude-code", "cwd": row["cwd"], "session_id": receiver}
        status, context = request(server_url, "/handoff?" + urllib.parse.urlencode(query))
        assert status == 200 and row["fact"] in context, f"{row['shape']}: hook handoff did not carry the fact"
        _, replay = request(server_url, "/handoff?" + urllib.parse.urlencode(query))
        assert row["fact"] not in replay, f"{row['shape']}: hook handoff claimed twice"
        report.append({"agent": row["agent"], "shape": row["shape"], "observations": state[3], "summary_link": True, "recall": True, "handoff_once": True})
    report.append(lifecycle_only(connection, server_url, workspace))
    report.append(mcp_only(server_url, workspace))
    return report


def ended_state(connection, session_id, workspace, project):
    state = session_state(connection, session_id, workspace, project)
    return state if state and state[0] is not None else None


def lifecycle_only(connection, server_url, workspace):
    sid = str(uuid.uuid4())
    for event in ("session-start", "session-end"):
        hook(server_url, workspace, "lifecycle-only", "codex", event, {"session_id": sid, "cwd": "/synthetic-quality/lifecycle-only"})
    state = wait_for(lambda: ended_state(connection, sid, workspace, "lifecycle-only"), "boundary-only end")
    assert state[1] is None and state[3] == 2, "boundary-only session generated a summary or lost lifecycle"
    count = connection.execute("SELECT count(*) FROM handoffs WHERE from_session_id=?", (uuid.UUID(sid).bytes,)).fetchone()[0]
    assert count == 0, "boundary-only session generated an empty handoff"
    return {"case": "lifecycle-only", "observations": 2, "summary": False, "handoff": False}


def mcp_only(server_url, workspace):
    scope = {"workspace": workspace, "project": "muse-mcp-only"}
    created = mcp_call(server_url, "memory_handoff_begin", {**scope, "summary": "muse-synthetic-baton-47", "shared": True})
    handoff_id = created["handoff_id"]
    listed = mcp_call(server_url, "memory_handoff_list", scope)
    assert handoff_id in json.dumps(listed), "MCP-only handoff was not listed"
    # Another project must not consume this project's exact handoff id.
    foreign = {"workspace": workspace, "project": "muse-other-project", "summary": "control", "shared": True}
    mcp_call(server_url, "memory_handoff_begin", foreign)
    try:
        refused = mcp_call(server_url, "memory_handoff_accept", {"workspace": workspace, "project": foreign["project"], "handoff_id": handoff_id})
        assert refused.get("handoff") is None, "foreign-scope exact handoff id was consumed"
    except ValueError:
        pass
    accepted = mcp_call(server_url, "memory_handoff_accept", {**scope, "handoff_id": handoff_id})
    assert accepted["status"] == "claimed" and "muse-synthetic-baton-47" in json.dumps(accepted), "MCP-only exact claim failed"
    replay = mcp_call(server_url, "memory_handoff_accept", {**scope, "handoff_id": handoff_id})
    assert replay.get("handoff") is None, "MCP-only handoff claimed twice"
    recent = mcp_call(server_url, "memory_recent", scope)
    assert isinstance(recent, dict), "MCP-only recent unavailable"
    return {"case": "muse-mcp-only", "list": True, "exact_claim_once": True, "foreign_project_refused": True, "recent": True}


def discover(connection, server_url, workspace, repo):
    # The legitimate root can be a future cwd-match key. Its child project
    # must never register the enclosing root and swallow sibling checkouts.
    for cwd, project in [(repo, "discovery-root"), (repo / "backend", "backend")]:
        sid = str(uuid.uuid4())
        extra = {"cwd": str(cwd)}
        for event in ("session-start", "session-end"):
            query = {"workspace": workspace, "agent": "codex", "event": event}
            if project == "backend":
                # An operator explicitly naming a child project outranks the
                # already-registered root's legitimate cwd-prefix routing.
                query["project"] = project
            status, _ = request(server_url, "/hook?" + urllib.parse.urlencode(query), {"session_id": sid, **extra})
            assert status == 202
        wait_for(lambda: ended_state(connection, sid, workspace, project), "repository discovery")
    paths = dict(connection.execute(
        "SELECT pr.name, pr.repo_path FROM projects pr JOIN workspaces ws ON ws.id=pr.workspace_id "
        "WHERE ws.name=? AND pr.name IN ('discovery-root', 'backend')", (workspace,),
    ))
    assert paths == {"discovery-root": str(repo), "backend": None}, "discover registered a child as an enclosing-root catch-all"
    return {"case": "discover", "root_registered": True, "child_isolated": True}
