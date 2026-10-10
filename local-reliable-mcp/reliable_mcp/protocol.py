"""JSON-RPC and the deterministic write contract.

CAS and the status enum compare bytes, metadata, and UUIDs. Jev does not
apply: nothing here classifies free text.
"""

from __future__ import annotations

import hashlib
import json
import uuid

# Canonical global scope from ai-memory-core: DEFAULT_WORKSPACE_NAME /
# GLOBAL_SCOPE_PROJECT. memory_read_page has no scope argument, so a global
# read names this pair explicitly instead of inheriting the server's last project.
GLOBAL_WORKSPACE = "default"
GLOBAL_PROJECT = "_global"
TIERS = {"working", "episodic", "semantic", "procedural"}


def parse_message(raw: bytes):
    try:
        message = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    if not isinstance(message, dict):
        return None
    return message


def is_write(message) -> bool:
    if not message or message.get("method") != "tools/call":
        return False
    params = message.get("params")
    if not isinstance(params, dict):
        return False
    return params.get("name") == "memory_write_page"


def arguments_of(message) -> dict:
    params = message.get("params") if isinstance(message, dict) else None
    if not isinstance(params, dict):
        return {}
    args = params.get("arguments")
    return args if isinstance(args, dict) else {}


def _blank(value) -> bool:
    return value is None or (isinstance(value, str) and value.strip() == "")


def _uuid(value) -> bool:
    if not isinstance(value, str):
        return False
    try:
        uuid.UUID(value)
    except ValueError:
        return False
    return True


def portable_path(path: str) -> bool:
    if not isinstance(path, str) or not path or path.startswith(("/", "~")):
        return False
    if "\x00" in path or "\\" in path or path.endswith("/"):
        return False
    return all(part not in ("", ".", "..") for part in path.split("/"))


def parse_scope(args: dict):
    scope = args.get("scope")
    workspace = args.get("workspace")
    project = args.get("project")
    if scope is not None and not isinstance(scope, str):
        return None, 'refused: scope must be the string "global"'
    if isinstance(scope, str) and scope.strip() == "global":
        if not _blank(workspace) or not _blank(project):
            return None, 'refused: scope "global" cannot be combined with workspace or project'
        return {"kind": "global"}, None
    if isinstance(scope, str) and scope.strip() not in ("",):
        return None, f"refused: unknown scope {scope.strip()!r}; the only value is \"global\""
    if _blank(workspace) and _blank(project):
        return None, 'refused: missing explicit scope; pass workspace and project together, or scope "global"'
    if _blank(workspace) or _blank(project):
        return None, "refused: workspace and project must be passed together"
    if not isinstance(workspace, str) or not isinstance(project, str):
        return None, "refused: workspace and project must be strings"
    return {"kind": "project", "workspace": workspace.strip(), "project": project.strip()}, None


def normalize_write(args: dict):
    if not isinstance(args, dict):
        return None, "refused: arguments must be an object"
    path = args.get("path")
    body = args.get("body")
    if not portable_path(path):
        return None, "refused: path must be a relative wiki path"
    if not isinstance(body, str):
        return None, "refused: body is required"
    scope, error = parse_scope(args)
    if error:
        return None, error
    tier = args.get("tier")
    if _blank(tier):
        tier = "semantic"
    elif not isinstance(tier, str) or tier.strip() not in TIERS:
        return None, "refused: unknown tier"
    else:
        tier = tier.strip()
    pinned = args.get("pinned", False)
    if pinned is None:
        pinned = False
    if not isinstance(pinned, bool):
        return None, "refused: pinned must be a boolean"
    tags = args.get("tags", [])
    if tags is None:
        tags = []
    if not isinstance(tags, list) or not all(isinstance(tag, str) for tag in tags):
        return None, "refused: tags must be a list of strings"
    title = args.get("title")
    if title is not None and not isinstance(title, str):
        return None, "refused: title must be a string"
    if isinstance(title, str) and title.strip() == "":
        title = None
    expires = args.get("expires_at")
    if expires is not None and not isinstance(expires, str):
        return None, "refused: expires_at must be a string"
    if isinstance(expires, str):
        expires = expires.strip() or None
    create_only = args.get("create_only")
    expected = args.get("expected_page_id")
    has_expected = isinstance(expected, str) and expected.strip() != ""
    if create_only is True and has_expected:
        return None, "refused: create_only and expected_page_id are mutually exclusive"
    if has_expected and not _uuid(expected):
        return None, "refused: expected_page_id must be a UUID"
    if create_only is True:
        cas = {"mode": "create_only"}
    elif has_expected:
        cas = {"mode": "expected_page_id", "expected_page_id": str(uuid.UUID(expected))}
    else:
        cas = {"mode": "auto"}
    return {
        "path": path,
        "body": body,
        "scope": scope,
        "tier": tier,
        "pinned": pinned,
        "tags": list(tags),
        "title": title,
        "expires_at": expires,
        "cas": cas,
        "client_cas": json.loads(json.dumps(cas)),
    }, None


def derive_title(body: str, path: str) -> str:
    for line in body.splitlines():
        stripped = line.strip()
        if stripped.startswith("# "):
            return stripped[2:].strip()
    stem = path.rsplit("/", 1)[-1]
    return stem[:-3] if stem.endswith(".md") else stem


def expected_title(intent: dict) -> str:
    if intent.get("title"):
        return intent["title"]
    return derive_title(intent["body"], intent["path"])


def intent_hash(intent: dict, auth_fingerprint: str) -> str:
    payload = {
        "auth": auth_fingerprint,
        "body": intent["body"],
        "cas": intent["client_cas"],
        "expires_at": intent["expires_at"],
        "path": intent["path"],
        "pinned": intent["pinned"],
        "scope": intent["scope"],
        "tags": intent["tags"],
        "tier": intent["tier"],
        "title": intent["title"],
    }
    raw = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def auth_fingerprint(authorization: str | None) -> str:
    if not authorization:
        return ""
    return hashlib.sha256(authorization.encode("utf-8")).hexdigest()


def read_arguments(intent: dict) -> dict:
    if intent["scope"]["kind"] == "global":
        return {
            "path": intent["path"],
            "workspace": GLOBAL_WORKSPACE,
            "project": GLOBAL_PROJECT,
        }
    return {
        "path": intent["path"],
        "workspace": intent["scope"]["workspace"],
        "project": intent["scope"]["project"],
    }


def write_arguments(intent: dict, cas: dict) -> dict:
    args = {
        "path": intent["path"],
        "body": intent["body"],
        "tier": intent["tier"],
        "pinned": intent["pinned"],
        "tags": intent["tags"],
    }
    if intent.get("title") is not None:
        args["title"] = intent["title"]
    if intent.get("expires_at") is not None:
        args["expires_at"] = intent["expires_at"]
    if intent["scope"]["kind"] == "global":
        args["scope"] = "global"
    else:
        args["workspace"] = intent["scope"]["workspace"]
        args["project"] = intent["scope"]["project"]
    if cas["mode"] == "create_only":
        args["create_only"] = True
    elif cas["mode"] == "expected_page_id":
        args["expected_page_id"] = cas["expected_page_id"]
    else:
        raise RuntimeError("refusing to send a write without create_only or expected_page_id")
    if "create_only" in args and "expected_page_id" in args:
        raise RuntimeError("refusing to send both CAS fields")
    return args


def tool_payload(message: dict):
    result = message.get("result")
    if not isinstance(result, dict):
        return None
    if result.get("isError") is True:
        return None
    content = result.get("content")
    if not isinstance(content, list) or not content:
        return None
    first = content[0]
    if not isinstance(first, dict) or not isinstance(first.get("text"), str):
        return None
    try:
        payload = json.loads(first["text"])
    except json.JSONDecodeError:
        return None
    return payload if isinstance(payload, dict) else None


def error_text(message: dict) -> str:
    error = message.get("error")
    if isinstance(error, dict):
        return str(error.get("message") or "")
    result = message.get("result")
    if isinstance(result, dict) and result.get("isError") is True:
        content = result.get("content")
        if isinstance(content, list) and content and isinstance(content[0], dict):
            return str(content[0].get("text") or "")
    return ""


def page_view(payload: dict):
    if not isinstance(payload, dict) or not _uuid(str(payload.get("page_id", ""))):
        return None
    if not isinstance(payload.get("body"), str):
        return None
    frontmatter = payload.get("frontmatter")
    if not isinstance(frontmatter, dict):
        return None
    return {
        "page_id": str(uuid.UUID(str(payload["page_id"]))),
        "body": payload["body"],
        "title": payload.get("title"),
        "frontmatter": frontmatter,
    }


def matches(intent: dict, page: dict) -> bool:
    if page["body"] != intent["body"]:
        return False
    frontmatter = page["frontmatter"]
    if frontmatter.get("tier") != intent["tier"]:
        return False
    pinned = frontmatter.get("pinned", False)
    if pinned is None:
        pinned = False
    if bool(pinned) != intent["pinned"]:
        return False
    tags = frontmatter.get("tags") or []
    if list(tags) != list(intent["tags"]):
        return False
    expires = frontmatter.get("expires_at")
    if expires == "":
        expires = None
    if expires != intent["expires_at"]:
        return False
    if page.get("title") != expected_title(intent):
        return False
    if intent.get("title") is not None and frontmatter.get("title") not in (None, intent["title"]):
        return False
    return True


def summarize_page(page: dict) -> dict:
    return {
        "state": "present",
        "page_id": page["page_id"],
        "body_sha256": hashlib.sha256(page["body"].encode("utf-8")).hexdigest(),
        "tier": page["frontmatter"].get("tier"),
        "pinned": bool(page["frontmatter"].get("pinned", False)),
        "tags": list(page["frontmatter"].get("tags") or []),
        "title": page.get("title"),
        "expires_at": page["frontmatter"].get("expires_at"),
    }


def rpc_error(rpc_id, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": rpc_id, "error": {"code": code, "message": message}}


def rpc_result(rpc_id, payload: dict, is_error: bool = False) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "content": [{"type": "text", "text": json.dumps(payload, ensure_ascii=False)}],
            "isError": is_error,
        },
    }


def receipt(intent: dict) -> dict:
    status = intent["status"]
    body = {
        "status": status.upper(),
        "operation_id": intent["operation_id"],
        "path": intent["path"],
        "scope": intent["scope"],
        "page_id": intent.get("page_id") if status == "delivered" else None,
        "written": status == "delivered",
        "queued": status == "pending",
    }
    if status == "pending":
        body["receipt"] = "PENDING"
    if intent.get("reauthorization_required"):
        body["reauthorization_required"] = True
    if intent.get("current_head_page_id"):
        body["current_head_page_id"] = intent["current_head_page_id"]
    if intent.get("reason"):
        body["reason"] = intent["reason"]
    if status == "delivered" and intent.get("checkpoint") is not None:
        body["checkpoint"] = intent["checkpoint"]
    return body


def cas_advertised(tools) -> bool:
    if not isinstance(tools, list):
        return False
    for tool in tools:
        if not isinstance(tool, dict) or tool.get("name") != "memory_write_page":
            continue
        props = (tool.get("inputSchema") or {}).get("properties")
        if not isinstance(props, dict):
            return False
        return "create_only" in props and "expected_page_id" in props
    return False


def tools_named(tools) -> set[str]:
    if not isinstance(tools, list):
        return set()
    return {tool.get("name") for tool in tools if isinstance(tool, dict)}
