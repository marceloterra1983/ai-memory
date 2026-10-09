#!/usr/bin/env python3
"""Latest-only metadata inventory and exact-sentinel cleanup through root admin.

Requires explicit DB, server URL and scope. SQL is read-only; all mutations go
through CAS/admission/wiki/writer on the server. Reports are private by default.
"""
import argparse
import json
import os
import pathlib
import sqlite3
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import uuid

KEYS = ("causes", "fixes", "contradicts", "references")
SENTINELS = {"none", "nenhuma", "n/a", "none.md", "nenhuma.md", "n/a.md"}
WRITE_SCOPES = {("default", "dev"), ("default", "farol"), ("default", "regente")}


class CurationError(Exception):
    """A refusal that never includes response bodies or credentials."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """Keep authorization and curation requests on the explicit server."""
    def redirect_request(self, _req, _fp, _code, _msg, _headers, _newurl):
        return None


def is_sentinel(value):
    return isinstance(value, str) and value.strip().lower() in SENTINELS


def ensure_write_scope(workspace, project):
    if (workspace, project) not in WRITE_SCOPES:
        raise CurationError("scope is not authorized for relation cleanup")


def relation_origin(frontmatter):
    origin = {key: frontmatter[key] for key in ("session_id", "agent", "sources") if key in frontmatter}
    if origin:
        if "origin" in frontmatter:
            origin["declared_origin"] = frontmatter["origin"]
        return origin
    return frontmatter.get("origin")


def build_plan(rows, server_url, workspace, project, target_exists=None):
    ensure_write_scope(workspace, project)
    existing = {row["path"] for row in rows}
    target_exists = target_exists or (lambda target: target in existing)
    plan = {"schema": 1, "server_url": server_url, "workspace": workspace,
            "project": project, "candidates": [], "pending": []}
    for row in rows:
        fm = row["frontmatter"]
        relations = fm.get("relations", {}) if isinstance(fm, dict) else {}
        if not isinstance(relations, dict):
            plan["pending"].append({"head_id": row["head_id"], "path": row["path"],
                                    "relation": None, "raw_target": relations,
                                    "origin": relation_origin(fm),
                                    "source_fact": {"field": "relations", "value": relations},
                                    "action": "pending; no inference or content change"})
            continue
        removed = 0
        for relation, values in relations.items():
            is_array = isinstance(values, list)
            if not is_array:
                values = [values]
            for value in values:
                if is_array and relation in KEYS and is_sentinel(value):
                    removed += 1
                elif not is_array or relation not in KEYS or not isinstance(value, str) or not target_exists(value):
                    plan["pending"].append({"head_id": row["head_id"], "path": row["path"],
                                            "relation": relation, "raw_target": value,
                                            "origin": relation_origin(fm),
                                            "source_fact": {"field": "relations", "value": value},
                                            "action": "pending; no inference or content change"})
        if removed:
            plan["candidates"].append({"path": row["path"], "expected_head_id": row["head_id"],
                                       "expected_body_sha256": row["body_sha256"], "removed": removed})
    return plan


def execute_plan(plan, request, apply=False, record_receipts=None):
    ensure_write_scope(plan["workspace"], plan["project"])
    payloads = [{"workspace": plan["workspace"], "project": plan["project"],
                 "path": row["path"], "expected_head_id": row["expected_head_id"],
                 "expected_body_sha256": row["expected_body_sha256"], "dry_run": True}
                for row in plan["candidates"]]
    # Every candidate must pass the real server simulator before any write.
    previews = []
    for row, payload in zip(plan["candidates"], payloads):
        preview = request(payload)
        if (preview.get("page_id") != payload["expected_head_id"] or preview.get("applied") is not False
                or preview.get("removed") != row["removed"]):
            raise CurationError("preview differs from the reviewed plan; nothing applied")
        previews.append(preview)
    if not apply:
        return previews
    receipts = []
    for payload in payloads:
        receipt = request({**payload, "dry_run": False})
        receipts.append(receipt)
        if record_receipts:
            record_receipts(receipts)
        if not receipt.get("applied") or not receipt.get("before_checkpoint") or not receipt.get("checkpoint"):
            raise CurationError("server did not return a recoverable mutation receipt; stopped")
    return receipts


def write_private(path, value):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".curation-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as out:
            json.dump(value, out, ensure_ascii=False, indent=2)
            out.write("\n")
            out.flush()
            os.fsync(out.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def snapshot(database, workspace, project):
    con = sqlite3.connect(pathlib.Path(database).resolve().as_uri() + "?mode=ro", uri=True)
    con.execute("PRAGMA query_only=ON")
    scope = con.execute("SELECT w.id,p.id FROM workspaces w JOIN projects p ON p.workspace_id=w.id "
                        "WHERE w.name=? AND p.name=?", (workspace, project)).fetchone()
    if scope is None:
        con.close()
        raise CurationError("explicit scope does not exist; nothing created")
    if project.lower() == "qlmed":
        # No path, title, frontmatter or body is selected for QLMED.
        count = con.execute("SELECT count(*) FROM pages WHERE workspace_id=? AND project_id=? AND is_latest=1",
                            scope).fetchone()[0]
        con.close()
        return None, {"metadata_only": True, "latest_pages": count}
    ensure_write_scope(workspace, project)
    rows = [{"head_id": str(uuid.UUID(bytes=page_id)), "path": path, "body_sha256": body_hash.lower(),
             "frontmatter": json.loads(fm)} for page_id, path, body_hash, fm in con.execute(
                 "SELECT id,path,hex(body_sha256),frontmatter_json FROM pages "
                 "WHERE workspace_id=? AND project_id=? AND is_latest=1 ORDER BY path", scope)]
    def exists(target):
        if not isinstance(target, str) or not target.endswith(".md"):
            return False
        target_ws, target_project, path = workspace, project, target
        if ":" in target:
            names, path = target.split(":", 1)
            if "/" in names:
                target_ws, target_project = names.split("/", 1)
            else:
                target_project = names
        return con.execute("SELECT 1 FROM pages pg JOIN projects p ON p.id=pg.project_id "
                           "JOIN workspaces w ON w.id=pg.workspace_id WHERE w.name=? AND p.name=? "
                           "AND pg.path=? AND pg.is_latest=1 LIMIT 1", (target_ws, target_project, path)).fetchone() is not None
    return con, (rows, exists)


def server_request(server_url, token):
    parsed = urllib.parse.urlsplit(server_url)
    if parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise CurationError("server URL must be explicit HTTP(S), without embedded credentials")
    opener = urllib.request.build_opener(NoRedirect())
    def request(payload):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        req = urllib.request.Request(server_url.rstrip("/") + "/admin/curate-relations",
                                     data=json.dumps(payload).encode(), headers=headers, method="POST")
        try:
            with opener.open(req, timeout=30) as response:
                result = json.load(response)
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            raise CurationError(f"server refused curation (HTTP {status}); stopped") from None
        except (urllib.error.URLError, TimeoutError, ValueError):
            raise CurationError("server transport or receipt failed; stopped") from None
        return result
    return request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for argument in ("db", "server-url", "workspace", "project", "out"):
        parser.add_argument("--" + argument, required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--plan", help="previous reviewed dry-run artifact; required for apply")
    parser.add_argument("--token-stdin", action="store_true", help="root supplies bearer via stdin; never env/config")
    args = parser.parse_args()
    con = None
    try:
        if args.apply and not args.plan:
            raise CurationError("apply requires a reviewed --plan")
        if args.apply:
            ensure_write_scope(args.workspace, args.project)
        con, data = snapshot(args.db, args.workspace, args.project)
        if args.project.lower() == "qlmed":
            write_private(args.out, data)
            print(json.dumps(data))
            return 0
        rows, exists = data
        plan = build_plan(rows, args.server_url.rstrip("/"), args.workspace, args.project, exists)
        if args.apply:
            reviewed = json.loads(pathlib.Path(args.plan).read_text(encoding="utf-8"))
            for field in ("schema", "server_url", "workspace", "project", "candidates"):
                if reviewed.get(field) != plan[field]:
                    raise CurationError("latest metadata differs from the reviewed plan; nothing applied")
        token = sys.stdin.readline().strip() if args.token_stdin else None
        request = server_request(args.server_url, token)
        # Persist the plan before network activity so even an interrupted apply has
        # its expected predecessors recorded. Checkpoint ids are added on success.
        write_private(args.out, plan)
        def record_receipts(receipts):
            plan["receipts"] = receipts
            write_private(args.out, plan)
        plan["receipts"] = execute_plan(plan, request, apply=args.apply, record_receipts=record_receipts)
        write_private(args.out, plan)
        print(json.dumps({"candidates": len(plan["candidates"]), "pending": len(plan["pending"]), "applied": args.apply}))
        return 0
    except (CurationError, sqlite3.Error, OSError, ValueError) as error:
        # Only controlled errors may reach output; database/JSON exceptions can
        # carry stored text, paths or response contents.
        print(str(error) if isinstance(error, CurationError) else "metadata inventory failed; nothing inferred", file=sys.stderr)
        return 2
    finally:
        if con is not None:
            con.close()


if __name__ == "__main__":
    sys.exit(main())
