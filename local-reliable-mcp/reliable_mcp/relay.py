"""Loopback HTTP ingress. Only memory_write_page is intercepted."""

from __future__ import annotations

import json
import socket
import socketserver
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

from reliable_mcp.backend import Client, HOP_BY_HOP, forward_headers
from reliable_mcp.deliver import attempt, confirm_delivered, drain
from reliable_mcp.outbox import (
    enqueue,
    find_by_hash,
    global_lock,
    load_cache,
    load_intent,
    save_cache,
)
from reliable_mcp.protocol import (
    arguments_of,
    auth_fingerprint,
    cas_advertised,
    duplicate_write,
    intent_hash,
    is_write,
    normalize_write,
    parse_message,
    receipt,
    rpc_error,
    rpc_notification_ok,
    rpc_request_ok,
    rpc_result,
    safe_echo_id,
)
from reliable_mcp.safe import (
    LOOPBACK_HOSTS,
    ConfigError,
    assert_loopback_host,
    assert_not_root,
    host_allowed,
    load_public_allowlist,
    origin_allowed,
    parse_listen,
    parse_origin,
    safe_fields,
)

DEFAULT_MAX_BODY = 10 * 1024 * 1024
IDENTITY_HEADERS = {"origin", "cookie", "mcp-session-id", "mcp-protocol-version", "host"}


class Relay:
    def __init__(
        self,
        listen: str,
        backend: str,
        outbox,
        auto_drain_seconds: float = 0,
        log=None,
        max_body: int = DEFAULT_MAX_BODY,
        max_inflight: int = 32,
        acquire_timeout: float = 2.0,
        forward_timeout: float = 8.0,
        data_dir=None,
        read_timeout: float = 8.0,
        max_header: int = 8192,
        allowed_hosts=None,
        allowed_origins=None,
        config_path=None,
    ):
        assert_not_root()
        self.listen_host, self.listen_port = parse_listen(listen)
        self.backend_host, self.backend_port = parse_origin(backend)
        if self.listen_port != 0 and (self.listen_host, self.listen_port) == (
            self.backend_host,
            self.backend_port,
        ):
            raise ConfigError("listen and backend must differ")
        self._backend_origin = f"http://{self.backend_host}:{self.backend_port}"
        self.auto_drain_seconds = auto_drain_seconds
        self.log = log
        self.max_body = max_body
        self.max_inflight = max_inflight
        self.acquire_timeout = acquire_timeout
        self.forward_timeout = forward_timeout
        self.client = Client(self._backend_origin, timeout=forward_timeout, max_body=max_body)
        self.read_timeout = read_timeout
        self.max_header = max_header
        self.handlers_entered = 0
        # Reject a bad config before the outbox directory is created.
        if config_path:
            self.allowed_hosts, self.allowed_origins = load_public_allowlist(config_path)
        else:
            self.allowed_hosts = list(allowed_hosts or LOOPBACK_HOSTS)
            self.allowed_origins = list(allowed_origins or [])
        from reliable_mcp.outbox import ensure_outbox

        self.outbox = ensure_outbox(Path(outbox), data_dir)
        self._cache = load_cache(self.outbox)
        self._slots = threading.BoundedSemaphore(max_inflight)
        self._httpd = None
        self._thread = None
        self._drain_thread = None
        self._stopped = threading.Event()
        self.port = None

    @property
    def backend_origin(self) -> str:
        return self._backend_origin

    @backend_origin.setter
    def backend_origin(self, value: str) -> None:
        host, port = parse_origin(value)
        if self.port and (host, port) == (self.listen_host, self.port):
            raise ConfigError("listen and backend must differ")
        self.backend_host = host
        self.backend_port = port
        self._backend_origin = f"http://{host}:{port}"
        if self.client is not None:
            self.client.host = host
            self.client.port = port

    @property
    def url(self) -> str:
        return f"http://{self.listen_host}:{self.port}"

    def start(self) -> None:
        if self._httpd is not None:
            return
        httpd = _Server((self.listen_host, self.listen_port), _Handler)
        httpd.relay = self
        self.port = httpd.server_address[1]
        if (self.listen_host, self.port) == (self.backend_host, self.backend_port):
            httpd.server_close()
            raise ConfigError("listen and backend must differ")
        self._httpd = httpd
        self._stopped.clear()
        self._thread = threading.Thread(target=httpd.serve_forever, name="reliable-mcp", daemon=True)
        self._thread.start()
        if self.auto_drain_seconds and self.auto_drain_seconds > 0:
            self._drain_thread = threading.Thread(target=self._drain_loop, name="reliable-mcp-drain", daemon=True)
            self._drain_thread.start()

    def stop(self) -> None:
        self._stopped.set()
        httpd = self._httpd
        if httpd is None:
            return
        self._httpd = None
        httpd.shutdown()
        httpd.server_close()
        if self._thread is not None:
            self._thread.join(2)
            self._thread = None

    def drain(self) -> int:
        return drain(self.outbox, self.client)

    def _drain_loop(self) -> None:
        while not self._stopped.wait(self.auto_drain_seconds):
            try:
                self.drain()
            except Exception as exc:
                self._log(error_class=type(exc).__name__)

    def _log(self, **fields) -> None:
        if self.log is None:
            return
        self.log(safe_fields(**fields))

    def _acquire(self) -> bool:
        return self._slots.acquire(timeout=self.acquire_timeout)

    def _release(self) -> None:
        self._slots.release()

    def handle(self, method: str, target: str, header_items, body: bytes):
        path = target.split("?", 1)[0]
        self._log(method=method, path=path)
        if path == "/mcp" and method == "POST":
            return self._mcp(target, header_items, body)
        return self._forward(method, target, header_items, body)

    def _mcp(self, target: str, header_items, body: bytes):
        message = parse_message(body)
        if isinstance(message, dict) and is_write(message):
            if not rpc_request_ok(message):
                self._log(method="POST", path="/mcp", error_class="refused")
                return _json(rpc_error(safe_echo_id(message), -32600, "Invalid Request"))
            return self._write(message, header_items)
        # Duplicate keys must not be reinterpreted as a clean write. Other
        # malformed envelopes are forwarded unchanged so we do not synthesize
        # a valid request from them.
        if message is None and duplicate_write(body):
            self._log(method="POST", path="/mcp", error_class="refused")
            return _json(rpc_error(None, -32600, "Invalid Request"))
        if isinstance(message, dict) and (rpc_request_ok(message) or rpc_notification_ok(message)):
            return self._mcp_control(message, target, header_items, body)
        return self._forward("POST", target, header_items, body)

    def _mcp_control(self, message: dict, target: str, header_items, body: bytes):
        method = message.get("method")
        response = self.client.exchange(
            "POST",
            target,
            _forward_header_map(header_items, _host_value(header_items, self)),
            body,
        )
        if not response.transport_failed:
            self._observe(method, response.body)
            return _exchange_result(response)
        rpc_id = message.get("id", None)
        if isinstance(method, str) and method.startswith("notifications/") and "id" not in message:
            if self._cache:
                return 202, [("Content-Type", "text/plain"), ("Content-Length", "0")], b""
            return 502, [("Content-Type", "text/plain")], b"bad gateway\n"
        if method == "initialize":
            cached = _cached_initialize(self._cache, rpc_id)
            if cached is not None:
                return _json(cached)
            return _json(rpc_error(rpc_id, -32000, "backend unavailable"))
        if method == "tools/list":
            cached = _cached_tools(self._cache, rpc_id)
            if cached is not None:
                return _json(cached)
            return _json(rpc_error(rpc_id, -32000, "backend unavailable"))
        if method == "tools/call":
            return _json(rpc_error(rpc_id, -32000, "backend unavailable"))
        return _json(rpc_error(rpc_id, -32000, "backend unavailable"))

    def _observe(self, method, raw: bytes) -> None:
        message = parse_message(raw)
        if not isinstance(message, dict) or "error" in message:
            return
        result = message.get("result")
        if not isinstance(result, dict):
            return
        cache = dict(self._cache or {})
        if method == "initialize":
            cache["protocolVersion"] = result.get("protocolVersion")
            cache["capabilities"] = result.get("capabilities")
            cache["serverInfo"] = result.get("serverInfo")
        elif method == "tools/list" and cas_advertised(result.get("tools")):
            cache["tools"] = result.get("tools")
            cache["cas"] = True
        self._cache = cache
        if cache.get("cas") and cache.get("serverInfo") and cache.get("protocolVersion") and cache.get("tools"):
            try:
                save_cache(
                    self.outbox,
                    {
                        "cas": True,
                        "protocolVersion": cache["protocolVersion"],
                        "capabilities": cache.get("capabilities"),
                        "serverInfo": cache["serverInfo"],
                        "tools": cache["tools"],
                    },
                )
            except OSError as exc:
                self._log(error_class=type(exc).__name__)

    def _write(self, message: dict, header_items):
        rpc_id = message.get("id", None)
        if not _json_mime(_header(header_items, "content-type")):
            self._log(method="POST", path="/mcp", error_class="refused")
            return 415, [("Content-Type", "text/plain; charset=utf-8")], b"unsupported media type\n"
        arguments = arguments_of(message)
        normalized, error = normalize_write(arguments)
        if error:
            self._log(method="POST", path="/mcp", error_class="refused")
            return _json(rpc_error(rpc_id, -32602, error))
        authorization = _authorization(header_items)
        if _rejected_authorization(authorization):
            self._log(method="POST", path="/mcp", error_class="refused")
            return _json(rpc_error(rpc_id, -32602, "refused: authorization is empty"))
        identity = _identity(header_items)
        digest = intent_hash(normalized, auth_fingerprint(authorization))
        try:
            with global_lock(self.outbox):
                existing = find_by_hash(self.outbox, digest)
                if existing is None:
                    operation_id = str(uuid.uuid4())
                    intent = {
                        "operation_id": operation_id,
                        "status": "pending",
                        "path": normalized["path"],
                        "scope": normalized["scope"],
                        "tier": normalized["tier"],
                        "pinned": normalized["pinned"],
                        "tags": normalized["tags"],
                        "title": normalized["title"],
                        "expires_at": normalized["expires_at"],
                        "cas": normalized["cas"],
                        "client_cas": normalized["client_cas"],
                        "intent_hash": digest,
                        "auth": {
                            "present": authorization is not None,
                            "fingerprint": auth_fingerprint(authorization),
                        },
                        "baseline": None,
                        "attempts": 0,
                        "page_id": None,
                        "body": normalized["body"],
                        "params": {
                            "path": normalized["path"],
                            "tier": normalized["tier"],
                            "pinned": normalized["pinned"],
                            "tags": normalized["tags"],
                            "title": normalized["title"],
                            "expires_at": normalized["expires_at"],
                            "scope": normalized["scope"],
                            "cas": normalized["client_cas"],
                        },
                    }
                    stored = enqueue(self.outbox, intent, authorization, identity or None)
                    operation_id = stored["operation_id"]
                    state = "pending"
                else:
                    operation_id = existing["operation_id"]
                    state = existing["status"]
        except OSError as exc:
            self._log(method="POST", path="/mcp", error_class=type(exc).__name__)
            return 507, [("Content-Type", "text/plain; charset=utf-8")], b"not queued\n"
        self._log(
            method="POST",
            path="/mcp",
            operation_id=operation_id,
            scope=normalized["scope"].get("kind"),
            error_class=state,
        )
        if state == "delivered":
            shown = confirm_delivered(self.outbox, operation_id, self.client)
            return _json(_receipt_message(rpc_id, shown))
        if state == "pending":
            try:
                shown = attempt(self.outbox, operation_id, self.client)
            except Exception as exc:
                self._log(operation_id=operation_id, error_class=type(exc).__name__)
                shown = load_intent(self.outbox, operation_id)
            return _json(_receipt_message(rpc_id, shown))
        shown = load_intent(self.outbox, operation_id)
        return _json(_receipt_message(rpc_id, shown))

    def _forward(self, method: str, target: str, header_items, body: bytes):
        headers = _forward_header_map(header_items, _host_value(header_items, self))
        if body:
            headers["Content-Length"] = str(len(body))
        response = self.client.exchange(method, target, headers, body or None)
        if response.transport_failed:
            if response.error_class in {"TimeoutError", "timeout", "socket.timeout"}:
                return 504, [("Content-Type", "text/plain")], b"gateway timeout\n"
            return 502, [("Content-Type", "text/plain")], b"bad gateway\n"
        return _redirect_or_pass(self, response)


def _receipt_message(rpc_id, intent: dict) -> dict:
    payload = receipt(intent)
    if intent.get("unconfirmed") or intent.get("reason") == "unconfirmed":
        payload["queued"] = False
        payload["page_id"] = None
        payload["written"] = False
        payload["status"] = "PENDING"
    is_error = intent.get("status") in {"conflict", "refused"}
    return rpc_result(rpc_id, payload, is_error)


def _authorization(header_items):
    for key, value in header_items:
        if key.lower() == "authorization":
            return value
    return None


def _rejected_authorization(value) -> bool:
    if value is None:
        return False
    stripped = value.strip()
    return stripped == "" or stripped.lower() == "bearer"


def _identity(header_items) -> dict:
    found = {}
    for key, value in header_items:
        lower = key.lower()
        if lower == "authorization" or "\r" in value or "\n" in value:
            continue
        if lower in IDENTITY_HEADERS or lower.startswith("x-memory-"):
            found[key] = value
    return found


def _host_value(header_items, relay: Relay) -> str:
    for key, value in header_items:
        if key.lower() == "host" and value:
            return value
    return f"{relay.listen_host}:{relay.port}"


def _forward_header_map(header_items, host_value: str) -> dict:
    forwarded = forward_headers(header_items, host_value)
    return {key: value for key, value in forwarded.items() if key.lower() not in HOP_BY_HOP}


def _json(payload: dict):
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    headers = [
        ("Content-Type", "application/json"),
        ("Content-Length", str(len(body))),
    ]
    return 200, headers, body


def _exchange_result(response):
    headers = []
    for key, value in response.headers:
        if key.lower() in HOP_BY_HOP:
            continue
        headers.append((key, value))
    headers.append(("Content-Length", str(len(response.body))))
    return response.status, headers, response.body


def _redirect_or_pass(relay: Relay, response):
    if response.status not in {301, 302, 303, 307, 308}:
        return _exchange_result(response)
    location = None
    for key, value in response.headers:
        if key.lower() == "location":
            location = value
    if not location:
        return _exchange_result(response)
    parts = urlsplit(location)
    if not parts.scheme and not parts.netloc:
        return _exchange_result(response)
    try:
        assert_loopback_host(parts.hostname or "")
    except ConfigError:
        relay._log(method="REDIRECT", path=parts.path or "/", error_class="off_loopback")
        payload = b"bad gateway\n"
        return 502, [("Content-Type", "text/plain"), ("Content-Length", str(len(payload)))], payload
    if parts.port == relay.backend_port:
        host = parts.hostname
        rewritten = urlunsplit(
            (parts.scheme, f"{host}:{relay.port}", parts.path, parts.query, parts.fragment)
        )
        headers = []
        replaced = False
        for key, value in response.headers:
            if key.lower() in HOP_BY_HOP:
                continue
            if key.lower() == "location":
                headers.append((key, rewritten))
                replaced = True
            else:
                headers.append((key, value))
        if not replaced:
            headers.append(("Location", rewritten))
        headers.append(("Content-Length", str(len(response.body))))
        return response.status, headers, response.body
    return _exchange_result(response)


def _cached_initialize(cache, rpc_id):
    if not cache or not cache.get("serverInfo") or not cache.get("protocolVersion"):
        return None
    return {
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "protocolVersion": cache["protocolVersion"],
            "capabilities": cache.get("capabilities") or {},
            "serverInfo": cache["serverInfo"],
        },
    }


def _cached_tools(cache, rpc_id):
    if not cache or not cache.get("cas") or not cache.get("tools"):
        return None
    return {"jsonrpc": "2.0", "id": rpc_id, "result": {"tools": cache["tools"]}}


class HeaderTooLarge(Exception):
    pass


class _BoundedReader:
    def __init__(self, raw, max_header: int):
        self.raw = raw
        self.left = max_header
        self.headers_done = False

    def readline(self, size=-1):
        line = self.raw.readline(size)
        if not self.headers_done:
            self.left -= len(line)
            if self.left < 0:
                raise HeaderTooLarge()
            if line in (b"\r\n", b"\n", b""):
                self.headers_done = True
        return line

    def read(self, size=-1):
        return self.raw.read(size)

    def __getattr__(self, name):
        return getattr(self.raw, name)


_BUSY = (
    b"HTTP/1.1 503 Service Unavailable\r\n"
    b"Content-Type: text/plain\r\n"
    b"Content-Length: 5\r\n"
    b"Connection: close\r\n"
    b"\r\n"
    b"busy\n"
)
_BODYLESS = {"GET", "HEAD", "OPTIONS"}


class _Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    relay = None

    def handle_error(self, request, client_address):
        return

    def process_request(self, request, client_address):
        relay: Relay = self.relay
        if relay is None or not relay._acquire():
            try:
                request.settimeout(1)
                request.sendall(_BUSY)
            except OSError:
                pass
            request.close()
            return
        try:
            request.settimeout(relay.read_timeout)
        except Exception:
            relay._release()
            raise

        def _run():
            try:
                self.process_request_thread(request, client_address)
            finally:
                relay._release()

        thread = threading.Thread(target=_run, name="reliable-mcp-conn")
        thread.daemon = self.daemon_threads
        if self.block_on_close:
            vars(self).setdefault("_threads", socketserver._Threads())
        self._threads.append(thread)
        try:
            thread.start()
        except Exception:
            relay._release()
            raise


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        return

    def setup(self):
        super().setup()
        relay: Relay = self.server.relay
        self.connection.settimeout(relay.read_timeout)
        self.rfile = _BoundedReader(self.rfile, relay.max_header)

    def handle(self):
        relay: Relay = self.server.relay
        relay.handlers_entered += 1
        self.close_connection = True
        try:
            self.handle_one_request()
        except HeaderTooLarge:
            self._plain(431, b"headers too large\n")
        except (TimeoutError, socket.timeout):
            try:
                self._plain(408, b"timeout\n")
            except Exception:
                return

    def handle_one_request(self):
        # The stdlib swallows TimeoutError and closes with no status. A read
        # timeout has to be visible to the client.
        try:
            self.raw_requestline = self.rfile.readline(65537)
            if len(self.raw_requestline) > 65536:
                self.requestline = ""
                self.request_version = ""
                self.command = ""
                self._plain(414, b"uri too long\n")
                return
            if not self.raw_requestline:
                self.close_connection = True
                return
            if not self.parse_request():
                return
            method_name = "do_" + self.command
            if not hasattr(self, method_name):
                self._plain(501, b"unsupported\n")
                return
            getattr(self, method_name)()
            self.wfile.flush()
        except (TimeoutError, socket.timeout):
            self.close_connection = True
            try:
                self._plain(408, b"timeout\n")
            except Exception:
                return

    def do_GET(self):
        self._dispatch()

    def do_POST(self):
        self._dispatch()

    def do_PUT(self):
        self._dispatch()

    def do_DELETE(self):
        self._dispatch()

    def do_OPTIONS(self):
        self._dispatch()

    def do_HEAD(self):
        self._dispatch()

    def _dispatch(self):
        relay: Relay = self.server.relay
        target = self.path
        try:
            self._dispatch_checked(relay, target)
        except HeaderTooLarge:
            self._plain(431, b"headers too large\n")
        except (TimeoutError, socket.timeout):
            self._plain(408, b"timeout\n")
        except Exception as exc:
            relay._log(method=self.command, path=target.split("?", 1)[0], error_class=type(exc).__name__)
            self._plain(500, b"error\n")

    def _dispatch_checked(self, relay: Relay, target: str):
        if target.startswith("http://") or target.startswith("https://"):
            self._plain(400, b"absolute-form refused\n")
            return
        if _repeated(self.headers, "host"):
            self._plain(400, b"duplicate host\n")
            return
        if _repeated(self.headers, "authorization"):
            self._plain(400, b"duplicate authorization\n")
            return
        host_value = _single(self.headers, "host")
        if not host_value:
            self._plain(400, b"missing host\n")
            return
        if not host_allowed(host_value, relay.allowed_hosts):
            self._plain(403, b"forbidden host\n")
            return
        if _repeated(self.headers, "origin"):
            self._plain(400, b"duplicate origin\n")
            return
        origin = _single(self.headers, "origin")
        if origin and not origin_allowed(origin, relay.allowed_hosts, relay.allowed_origins):
            self._plain(403, b"forbidden origin\n")
            return
        if _repeated(self.headers, "transfer-encoding") or _single(self.headers, "transfer-encoding"):
            self._plain(411, b"transfer-encoding refused\n")
            return
        lengths = _all(self.headers, "content-length")
        if len(lengths) > 1:
            self._plain(400, b"ambiguous content-length\n")
            return
        if not lengths:
            if self.command not in _BODYLESS:
                self._plain(411, b"content-length required\n")
                return
            body = b""
        else:
            raw_length = lengths[0].strip()
            if not raw_length.isdigit():
                self._plain(400, b"bad content-length\n")
                return
            size = int(raw_length)
            if size > relay.max_body:
                self._plain(413, b"payload too large\n")
                return
            body = self.rfile.read(size) if size else b""
            if len(body) != size:
                self._plain(400, b"incomplete body\n")
                return
        status, headers, payload = relay.handle(self.command, target, list(self.headers.items()), body)
        self._send(status, headers, payload)

    def _plain(self, status: int, payload: bytes) -> None:
        self._send(status, [("Content-Type", "text/plain; charset=utf-8")], payload)

    def _send(self, status: int, headers, payload: bytes) -> None:
        self.send_response(status)
        seen_length = False
        seen_type = False
        for key, value in headers:
            if key.lower() == "content-length":
                seen_length = True
            if key.lower() == "content-type":
                seen_type = True
            if "\r" in str(value) or "\n" in str(value):
                continue
            self.send_header(key, value)
        if not seen_type:
            self.send_header("Content-Type", "application/octet-stream")
        if not seen_length:
            self.send_header("Content-Length", str(len(payload)))
        self.send_header("Connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(payload)


def _all(headers, name: str) -> list:
    values = headers.get_all(name)
    return list(values) if values else []


def _single(headers, name: str):
    values = _all(headers, name)
    return values[0] if values else None


def _repeated(headers, name: str) -> bool:
    return len(_all(headers, name)) > 1


def _header(items, name: str):
    name = name.lower()
    for key, value in items:
        if str(key).lower() == name:
            return value
    return None


def _json_mime(value) -> bool:
    if not isinstance(value, str) or not value.strip():
        return False
    media = value.split(";", 1)[0].strip().lower()
    return media == "application/json"
