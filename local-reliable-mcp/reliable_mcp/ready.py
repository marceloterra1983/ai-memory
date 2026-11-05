"""Readiness talks to the backend. A cached schema is not enough."""

from __future__ import annotations

import time

from reliable_mcp.backend import Client
from reliable_mcp.protocol import cas_advertised, parse_message, tool_payload, tools_named


def probe(url: str, workspace: str, project: str, per_call: float = 2.5, budget: float = 8.0):
    if not workspace or not project:
        return False, "missing explicit scope"
    started = time.monotonic()
    client = Client(url, timeout=per_call)

    def call(payload):
        remaining = budget - (time.monotonic() - started)
        if remaining <= 0:
            return None
        client.timeout = min(per_call, remaining)
        return client.rpc(payload)

    init = call(
        {
            "jsonrpc": "2.0",
            "id": "ready-init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "reliable-mcp", "version": "1"},
            },
        }
    )
    message = _ok_result(init, "ready-init")
    if message is None:
        return False, "initialize failed"
    result = message["result"]
    if not isinstance(result, dict) or not result.get("protocolVersion") or not isinstance(result.get("serverInfo"), dict):
        return False, "initialize failed"

    listed = call({"jsonrpc": "2.0", "id": "ready-tools", "method": "tools/list", "params": {}})
    tools_message = _ok_result(listed, "ready-tools")
    if tools_message is None:
        return False, "tools/list failed"
    tools = tools_message["result"].get("tools") if isinstance(tools_message["result"], dict) else None
    if not cas_advertised(tools):
        return False, "tools/list missing create_only"
    if "memory_status" not in tools_named(tools):
        return False, "tools/list missing memory_status"

    status = call(
        {
            "jsonrpc": "2.0",
            "id": "ready-status",
            "method": "tools/call",
            "params": {
                "name": "memory_status",
                "arguments": {"workspace": workspace, "project": project},
            },
        }
    )
    if status is None or status.transport_failed or status.status != 200:
        return False, "memory_status failed"
    parsed = parse_message(status.body)
    if not _matches(parsed, "ready-status"):
        return False, "memory_status malformed"
    if "error" in parsed:
        return False, "memory_status error"
    if isinstance(parsed.get("result"), dict) and parsed["result"].get("isError") is True:
        return False, "memory_status isError"
    payload = tool_payload(parsed)
    if not isinstance(payload, dict):
        return False, "memory_status malformed"
    pages = (payload.get("counts") or {}).get("pages_latest")
    if isinstance(pages, bool) or not isinstance(pages, int):
        return False, "memory_status pages_latest"
    scope = payload.get("scope") if isinstance(payload.get("scope"), dict) else {}
    if scope.get("workspace") != workspace or scope.get("project") != project or scope.get("resolved_by") != "explicit":
        return False, "memory_status scope mismatch"
    return True, "ready"


def _matches(message, rpc_id) -> bool:
    return isinstance(message, dict) and message.get("jsonrpc") == "2.0" and message.get("id") == rpc_id


def _ok_result(response, rpc_id):
    if response is None or response.transport_failed or response.status != 200:
        return None
    message = parse_message(response.body)
    if not _matches(message, rpc_id) or "error" in message or "result" not in message:
        return None
    return message
