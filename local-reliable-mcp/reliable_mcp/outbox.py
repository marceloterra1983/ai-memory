"""Durable intentions. A write is queued only after tmp, rename, and fsync."""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import shutil
import uuid
from pathlib import Path

from reliable_mcp.protocol import auth_fingerprint

DEFAULT_OUTBOX = Path.home() / ".local" / "share" / "ai-memory-delivery"


class OutboxError(OSError):
    pass


def default_data_dir() -> Path:
    return Path.home() / ".local" / "share" / "ai-memory"


def _inside(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
    except ValueError:
        return False
    return True


def _nofollow_dir(path: Path) -> None:
    if path.is_symlink():
        raise OutboxError(f"symlink refused: {path.name}")
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    os.close(fd)


def ensure_outbox(path: Path, data_dir: Path | None = None) -> Path:
    path = Path(path).expanduser()
    core = Path(data_dir) if data_dir is not None else default_data_dir()
    if _inside(path, core):
        raise OutboxError("outbox must stay outside the ai-memory data directory")
    if path.exists() or path.is_symlink():
        if path.is_symlink() or not path.is_dir():
            raise OutboxError("outbox path is not a real directory")
        if os.path.realpath(path) != os.path.abspath(path):
            raise OutboxError("symlink in outbox path")
    else:
        if path.parent.is_symlink() or not path.parent.is_dir():
            raise OutboxError("outbox parent is missing or a symlink")
        path.mkdir(mode=0o700)
    _nofollow_dir(path)
    os.chmod(path, 0o700)
    for name in ("ops", "staging", "cache"):
        child = path / name
        if child.is_symlink():
            raise OutboxError(f"symlink refused: {name}")
        child.mkdir(mode=0o700, exist_ok=True)
        os.chmod(child, 0o700)
        _nofollow_dir(child)
    return path


def atomic_write_bytes(path: Path, data: bytes) -> None:
    tmp = path.parent / f".{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        os.fchmod(fd, 0o600)
        view = memoryview(data)
        while view:
            written = os.write(fd, view)
            if written <= 0:
                raise OutboxError("short write")
            view = view[written:]
        os.fsync(fd)
    except Exception:
        os.close(fd)
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise
    os.close(fd)
    os.replace(tmp, path)
    os.chmod(path, 0o600)
    dirfd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)


def _fsync_dir(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _dump(value) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8")


@contextlib.contextmanager
def global_lock(box: Path):
    fd = os.open(box / ".lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    os.fchmod(fd, 0o600)
    fcntl.flock(fd, fcntl.LOCK_EX)
    try:
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


@contextlib.contextmanager
def op_lock(box: Path, operation_id: str):
    path = box / "ops" / operation_id
    if path.is_symlink() or not path.is_dir():
        raise OutboxError("operation is not a real directory")
    fd = os.open(path / ".lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    os.fchmod(fd, 0o600)
    fcntl.flock(fd, fcntl.LOCK_EX)
    try:
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def enqueue(box: Path, intent: dict, authorization: str | None, identity: dict | None = None) -> dict:
    """Persist intent, body, and params. Raise before anything is committed."""
    operation_id = intent["operation_id"]
    stage = box / "staging" / f"{operation_id}.{uuid.uuid4().hex}"
    os.mkdir(stage, 0o700)
    os.chmod(stage, 0o700)
    try:
        stored = dict(intent)
        stored.pop("body", None)
        atomic_write_bytes(stage / "intent.json", _dump(stored))
        atomic_write_bytes(stage / "body.md", intent["body"].encode("utf-8"))
        atomic_write_bytes(stage / "params.json", _dump(intent["params"]))
        if authorization is not None:
            atomic_write_bytes(stage / "auth.secret", authorization.encode("utf-8"))
        if identity:
            atomic_write_bytes(stage / "identity.secret", _dump(identity))
        destination = box / "ops" / operation_id
        os.rename(stage, destination)
        _fsync_dir(box / "ops")
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        with contextlib.suppress(OSError):
            if (box / "ops" / operation_id).is_dir() and not _intent_readable(box, operation_id):
                shutil.rmtree(box / "ops" / operation_id, ignore_errors=True)
        raise
    os.chmod(box / "ops" / operation_id, 0o700)
    return load_intent(box, operation_id)


def _intent_readable(box: Path, operation_id: str) -> bool:
    path = box / "ops" / operation_id / "intent.json"
    return path.is_file() and not path.is_symlink()


def _op_dir(box: Path, operation_id: str) -> Path:
    path = box / "ops" / operation_id
    if path.is_symlink() or not path.is_dir():
        raise OutboxError("operation is not a real directory")
    return path


def load_intent(box: Path, operation_id: str) -> dict:
    path = _op_dir(box, operation_id) / "intent.json"
    if path.is_symlink():
        raise OutboxError("symlink intent refused")
    intent = json.loads(path.read_text(encoding="utf-8"))
    intent["body"] = read_body(box, operation_id)
    return intent


def save_intent(box: Path, intent: dict) -> None:
    stored = dict(intent)
    stored.pop("body", None)
    atomic_write_bytes(_op_dir(box, intent["operation_id"]) / "intent.json", _dump(stored))


def read_body(box: Path, operation_id: str) -> str:
    path = _op_dir(box, operation_id) / "body.md"
    if path.is_symlink():
        raise OutboxError("symlink body refused")
    return path.read_text(encoding="utf-8")


def read_identity(box: Path, operation_id: str) -> dict:
    path = _op_dir(box, operation_id) / "identity.secret"
    if not path.exists():
        return {}
    if path.is_symlink():
        raise OutboxError("symlink identity refused")
    payload = json.loads(path.read_text(encoding="utf-8"))
    return payload if isinstance(payload, dict) else {}


def read_authorization(box: Path, operation_id: str) -> str | None:
    path = _op_dir(box, operation_id) / "auth.secret"
    if not path.exists():
        return None
    if path.is_symlink():
        raise OutboxError("symlink auth refused")
    return path.read_text(encoding="utf-8")


def iter_operation_ids(box: Path):
    ops = box / "ops"
    names = []
    for child in ops.iterdir():
        if child.is_symlink() or not child.is_dir():
            continue
        if (child / "intent.json").is_file() and not (child / "intent.json").is_symlink():
            names.append(child.name)
    return sorted(names)


def list_intents(box: Path) -> list[dict]:
    found = []
    for operation_id in iter_operation_ids(box):
        path = box / "ops" / operation_id / "intent.json"
        found.append(json.loads(path.read_text(encoding="utf-8")))
    return found


def find_by_hash(box: Path, digest: str):
    for intent in list_intents(box):
        if intent.get("intent_hash") == digest and intent.get("status") in {
            "pending",
            "delivered",
            "conflict",
            "refused",
        }:
            return intent
    return None


def fingerprint_of(authorization: str | None) -> str:
    return auth_fingerprint(authorization)


def load_cache(box: Path) -> dict | None:
    path = box / "cache" / "mcp_bootstrap.json"
    if not path.is_file() or path.is_symlink():
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None
    if not isinstance(payload, dict) or not payload.get("cas"):
        return None
    return payload


def save_cache(box: Path, cache: dict) -> None:
    atomic_write_bytes(box / "cache" / "mcp_bootstrap.json", _dump(cache))
