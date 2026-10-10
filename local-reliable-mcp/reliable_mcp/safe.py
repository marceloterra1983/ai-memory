"""Loopback checks and logs that stay free of bodies and tokens."""

from __future__ import annotations

import ipaddress
import os
import socket
from urllib.parse import urlsplit


class ConfigError(ValueError):
    pass


def assert_not_root() -> None:
    # A root process must not replay another user's queued write.
    if hasattr(os, "geteuid") and os.geteuid() == 0:
        raise PermissionError("refusing to replay or serve as root")


def assert_loopback_host(host: str) -> str:
    if not host:
        raise ConfigError("empty host")
    try:
        infos = socket.getaddrinfo(host, None, type=socket.SOCK_STREAM)
    except socket.gaierror as exc:
        raise ConfigError(f"unresolved host {host}") from exc
    if not infos:
        raise ConfigError(f"unresolved host {host}")
    for info in infos:
        address = ipaddress.ip_address(info[4][0])
        if not address.is_loopback:
            raise ConfigError(f"host {host} is not loopback")
    return host


def parse_origin(url: str) -> tuple[str, int]:
    parts = urlsplit(url)
    if parts.scheme != "http" or parts.username or parts.password:
        raise ConfigError("backend must be an http loopback origin")
    if parts.path not in ("", "/") or parts.query or parts.fragment:
        raise ConfigError("backend origin must not include a path")
    if parts.hostname is None or parts.port is None:
        raise ConfigError("backend origin needs a host and port")
    return assert_loopback_host(parts.hostname), parts.port


def parse_listen(value: str) -> tuple[str, int]:
    if value.startswith("["):
        host, _, port_text = value[1:].partition("]")
        port_text = port_text.removeprefix(":")
    else:
        host, _, port_text = value.rpartition(":")
    if host in {"0.0.0.0", "::"}:
        raise ConfigError("listen must be loopback")
    host = assert_loopback_host(host)
    try:
        port = int(port_text)
    except ValueError as exc:
        raise ConfigError("listen port is not an integer") from exc
    if not 0 <= port <= 65535:
        raise ConfigError("listen port is out of range")
    return host, port


def safe_fields(**fields):
    blocked = {
        "body",
        "authorization",
        "token",
        "params",
        "content",
        "text",
        "auth",
        "headers",
        "arguments",
    }
    return {key: value for key, value in fields.items() if key.lower() not in blocked}
