"""Loopback HTTP. Redirects are never followed.

Absence is equality on error.data.code (`page_not_found` or
`scope_not_found`). A message that merely contains "not found" is not
absence: unknown scope, IO, and authorization stay distinguishable.
Jev does not apply. No model is called.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from http.client import HTTPConnection, HTTPException

from reliable_mcp.protocol import (
    cas_advertised,
    page_view,
    parse_message,
    tool_payload,
)
from reliable_mcp.safe import parse_origin

HOP_BY_HOP = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
}


@dataclass
class Exchange:
    status: int | None
    headers: list[tuple[str, str]]
    body: bytes
    error_class: str | None = None

    @property
    def transport_failed(self) -> bool:
        return self.error_class is not None or self.status is None


class Client:
    def __init__(self, origin: str, timeout: float = 8.0, max_body: int = 10 * 1024 * 1024):
        self.host, self.port = parse_origin(origin)
        self.timeout = timeout
        self.max_body = max_body

    def exchange(self, method: str, path: str, headers: dict, body: bytes | None) -> Exchange:
        connection = HTTPConnection(self.host, self.port, timeout=self.timeout)
        try:
            connection.request(method, path, body=body, headers=headers)
            response = connection.getresponse()
            payload = response.read(self.max_body + 1)
            if len(payload) > self.max_body:
                return Exchange(None, [], b"", "response_too_large")
            return Exchange(response.status, response.getheaders(), payload, None)
        except (OSError, HTTPException) as exc:
            return Exchange(None, [], b"", type(exc).__name__)
        finally:
            connection.close()

    def rpc(self, payload: dict, authorization: str | None = None, extra: dict | None = None) -> Exchange:
        body = json.dumps(payload).encode("utf-8")
        headers = {
            "Host": f"{self.host}:{self.port}",
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Content-Length": str(len(body)),
        }
        if extra:
            headers.update(extra)
        if authorization:
            headers["Authorization"] = authorization
        return self.exchange("POST", "/mcp", headers, body)

    def cas_available(self, authorization: str | None = None, extra: dict | None = None) -> bool | None:
        response = self.rpc(
            {"jsonrpc": "2.0", "id": "cas", "method": "tools/list", "params": {}},
            authorization,
            extra,
        )
        message = _json_message(response)
        if message is None or response.status != 200 or "error" in message:
            return None if message is None or response.status != 200 else False
        tools = (message.get("result") or {}).get("tools") if isinstance(message.get("result"), dict) else None
        if not cas_advertised(tools):
            return False
        return True

    def read_page(self, arguments: dict, authorization: str | None, extra: dict | None = None):
        response = self.rpc(
            {
                "jsonrpc": "2.0",
                "id": "read",
                "method": "tools/call",
                "params": {"name": "memory_read_page", "arguments": arguments},
            },
            authorization,
            extra,
        )
        if response.transport_failed or response.status != 200:
            return "transport", None
        message = parse_message(response.body)
        if message is None:
            return "transport", None
        if "error" in message or (isinstance(message.get("result"), dict) and message["result"].get("isError") is True):
            code = _typed_code(message)
            if code == "page_not_found":
                return "absent", None
            if code == "scope_not_found":
                return "scope_absent", None
            return "rejected", None
        payload = tool_payload(message)
        page = page_view(payload) if payload else None
        if page is None:
            return "transport", None
        return "present", page

    def write_page(self, arguments: dict, authorization: str | None, extra: dict | None = None) -> Exchange:
        return self.rpc(
            {
                "jsonrpc": "2.0",
                "id": "write",
                "method": "tools/call",
                "params": {"name": "memory_write_page", "arguments": arguments},
            },
            authorization,
            extra,
        )


def _typed_code(message: dict) -> str | None:
    error = message.get("error")
    if not isinstance(error, dict):
        return None
    data = error.get("data")
    if not isinstance(data, dict):
        return None
    code = data.get("code")
    return code if isinstance(code, str) else None


def _json_message(response: Exchange):
    if response.transport_failed or response.status != 200:
        return None
    return parse_message(response.body)


def forward_headers(source_headers, host_value: str) -> dict:
    """Keep auth, identity, Host, and Origin. Drop hop-by-hop headers."""
    forwarded = {}
    for key, value in source_headers:
        lower = key.lower()
        if lower in HOP_BY_HOP or "\r" in value or "\n" in value:
            continue
        forwarded[key] = value
    forwarded["Host"] = host_value
    return forwarded


def header_map(pairs: list[tuple[str, str]]) -> dict[str, str]:
    return {key.lower(): value for key, value in pairs}
