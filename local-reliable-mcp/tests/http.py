"""HTTP helpers for the ingress tests."""

from __future__ import annotations

import json
import os
import urllib.parse
from http.client import HTTPConnection


def post_json(url, payload, headers=None, timeout=5, raw=None):
    parts = urllib.parse.urlsplit(url)
    connection = HTTPConnection(parts.hostname, parts.port, timeout=timeout)
    body = raw if raw is not None else json.dumps(payload).encode()
    request_headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        "Host": f"{parts.hostname}:{parts.port}",
        "Content-Length": str(len(body)),
    }
    if headers:
        request_headers.update(headers)
    path = parts.path or "/"
    if parts.query:
        path = f"{path}?{parts.query}"
    connection.request("POST", path, body=body, headers=request_headers)
    response = connection.getresponse()
    data = response.read()
    status = response.status
    response_headers = {k.lower(): v for k, v in response.getheaders()}
    connection.close()
    return status, response_headers, data


def rpc(url, method, params=None, rpc_id=1, headers=None, timeout=5):
    status, response_headers, data = post_json(
        url,
        {"jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params or {}},
        headers=headers,
        timeout=timeout,
    )
    parsed = json.loads(data.decode())
    return status, response_headers, parsed


def tool_text(message):
    content = message["result"]["content"]
    return json.loads(content[0]["text"])


def request(url, method, path, body=b"", headers=None, timeout=5):
    parts = urllib.parse.urlsplit(url)
    connection = HTTPConnection(parts.hostname, parts.port, timeout=timeout)
    request_headers = {
        "Host": f"{parts.hostname}:{parts.port}",
        "Content-Length": str(len(body)),
    }
    if headers:
        request_headers.update(headers)
    connection.request(method, path, body=body, headers=request_headers)
    response = connection.getresponse()
    data = response.read()
    status = response.status
    response_headers = {k.lower(): v for k, v in response.getheaders()}
    connection.close()
    return status, response_headers, data


def file_mode(path):
    return os.stat(path).st_mode & 0o777
