"""Local native-hook spool recovery, independent of the MCP page outbox.

HTTP acknowledgements include policy drops. Only a scoped, completed SQLite
ingest receipt permits archiving and unlinking an event. Incomplete receipts
remain queued without reposting into an already-running consolidation tail.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import signal
import sqlite3
import stat
import threading
import time
from collections import deque
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from reliable_mcp.backend import Client
from reliable_mcp.safe import assert_not_root, parse_origin

MAX_BYTES = 8 * 1024 * 1024
MAX_ITEMS = 32
MAX_REQUESTS = 4
SLOW_EVENTS = {"pre-compact", "post-compaction", "session-end"}


def _sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _read_private_source(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as handle:
        before = os.fstat(handle.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_BYTES:
            raise ValueError("invalid source file")
        raw = handle.read(MAX_BYTES + 1)
        after = os.fstat(handle.fileno())
        if (before.st_ino, before.st_size, before.st_mtime_ns) != (
            after.st_ino, after.st_size, after.st_mtime_ns
        ):
            raise ValueError("source changed")
        return raw, before


def _preserve(path, raw):
    """Exclusive private write; a partial/colliding archive never authorizes unlink."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        existing, meta = _read_private_source(path)
        if existing != raw or stat.S_IMODE(meta.st_mode) != 0o600:
            raise ValueError("archive collision")
        # Re-fsync on retry, including an earlier write whose fsync failed.
        with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as handle:
            os.fsync(handle.fileno())
    else:
        with os.fdopen(fd, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
    _sync_directory(path.parent)


class HookDelivery:
    """Unix operator sidecar; retain native spool ownership for the entire lifetime."""

    def __init__(self, spool, database, archive, origin, client=None):
        assert_not_root()
        self.spool, self.archive = Path(spool), Path(archive)
        database = Path(database)
        self.host, self.port = parse_origin(origin)
        self.origin = f"http://{self.host}:{self.port}"
        self.client = client or Client(self.origin, timeout=2)
        self._retry_after = {}
        self._closed = False
        for path in (self.spool, self.archive, database):
            if any(p.is_symlink() for p in (path, *path.parents)):
                raise ValueError("symlink root refused")
        if self.archive == self.spool or self.spool in self.archive.parents:
            raise ValueError("archive must be outside spool")
        self.spool.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.archive.mkdir(mode=0o700, parents=True, exist_ok=True)
        if stat.S_IMODE(self.archive.stat().st_mode) != 0o700:
            raise ValueError("archive must be private")
        self.db = sqlite3.connect(database.absolute().as_uri() + "?mode=ro", uri=True, timeout=2)
        self.db.execute("PRAGMA query_only=ON")
        fd = os.open(self.spool / ".drain.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        self.lock = os.fdopen(fd, "a+b")
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BaseException:
            self.lock.close()
            self.db.close()
            raise

    def close(self):
        if not self._closed:
            self.db.close()
            self.lock.close()
            self._closed = True

    def _load(self, path):
        raw, meta = _read_private_source(path)
        entry = json.loads(raw)
        if not isinstance(entry, dict) or not isinstance(entry.get("url"), str):
            raise ValueError("invalid event envelope")
        parts = urlsplit(entry["url"])
        if (parts.scheme, parts.hostname, parts.port, parts.path) != (
            "http", self.host, self.port, "/hook"
        ) or parts.username or parts.password or parts.fragment:
            raise ValueError("event bound elsewhere")
        query = parse_qs(parts.query, keep_blank_values=True)
        for field in ("workspace", "project", "ingest_key"):
            values = query.get(field, [])
            if len(values) != 1 or not values[0] or values[0] != values[0].strip():
                raise ValueError("explicit scope and key required")
        scope = self.db.execute(
            "SELECT p.id,w.id FROM projects p JOIN workspaces w ON w.id=p.workspace_id "
            "WHERE w.name=? AND p.name=?", (query["workspace"][0], query["project"][0])
        ).fetchall()
        if len(scope) != 1:
            raise ValueError("scope absent or ambiguous")
        if entry.get("profile") is not None or entry["auth_mode"] not in ("none", "static"):
            raise ValueError("unsupported bound auth")
        token = entry.get("token") if entry["auth_mode"] == "static" else None
        if entry["auth_mode"] == "static" and (not isinstance(token, str) or not token):
            raise ValueError("missing static credential")
        body = json.loads(entry["body"])
        if not isinstance(body, dict):
            raise ValueError("invalid body")
        # Same privacy backstop as native load_live_entry and server ingress.
        body.pop("last_assistant_message", None)
        return dict(path=path, raw=raw, meta=meta, url=entry["url"], body=body,
                    project_id=scope[0][0], workspace_id=scope[0][1],
                    key=query["ingest_key"][0], token=token,
                    slow=query.get("event", [""])[0] in SLOW_EVENTS)

    def _checkpoint(self, item):
        return self.db.execute(
            "SELECT seen_at,completed_at FROM ingest_keys WHERE project_id=? AND key=?",
            (item["project_id"], item["key"])
        ).fetchone()

    def _archive(self, item, checkpoint):
        raw, meta, path = item["raw"], item["meta"], item["path"]
        receipt = dict(sha256=hashlib.sha256(raw).hexdigest(), size=len(raw),
                       mode=stat.S_IMODE(meta.st_mode), uid=meta.st_uid, gid=meta.st_gid,
                       mtime_ns=meta.st_mtime_ns, created_inode=meta.st_ino,
                       workspace_id=item["workspace_id"].hex(),
                       project_id=item["project_id"].hex(), ingest_key=item["key"],
                       seen_at=checkpoint[0], completed_at=checkpoint[1])
        _preserve(self.archive / path.name, raw)
        _preserve(self.archive / (path.name + ".receipt.json"),
                  (json.dumps(receipt, sort_keys=True) + "\n").encode())
        current, current_meta = _read_private_source(path)
        if current != raw or current_meta.st_ino != meta.st_ino:
            raise ValueError("source changed before unlink")
        path.unlink()
        _sync_directory(self.spool)
        self._retry_after.pop(path.name, None)

    def drain_once(self):
        result = dict(delivered=0, pending=0, unconfirmed=0, errors=0)

        def load_pending(path):
            try:
                item = self._load(path)
                checkpoint = self._checkpoint(item)
                if checkpoint and checkpoint[1] is not None:
                    self._archive(item, checkpoint)
                    result["delivered"] += 1
                elif checkpoint:
                    result["unconfirmed"] += 1
                else:
                    return item
            except (OSError, ValueError, KeyError, TypeError, sqlite3.Error):
                result["errors"] += 1
            return None

        ready = deque()
        for path in sorted(self.spool.glob("*.json")):
            item = load_pending(path)
            if item is not None:
                if self._retry_after.get(path.name, 0) > time.monotonic():
                    result["unconfirmed"] += 1
                else:
                    ready.append(path)
        item = None
        started = time.monotonic()
        requests = 0
        while ready and requests < MAX_REQUESTS and time.monotonic() - started < 2:
            first = load_pending(ready.popleft())
            if first is None:
                continue
            batch = [first]
            size = len(first["raw"])
            # Keep each event's credential and original order, without mixing tokens.
            while not first["slow"] and ready and len(batch) < MAX_ITEMS:
                next_item = load_pending(ready[0])
                if next_item is None:
                    ready.popleft()
                    continue
                if next_item["slow"] or next_item["token"] != first["token"]:
                    break
                if size + len(next_item["raw"]) > MAX_BYTES // 2:
                    break
                ready.popleft()
                batch.append(next_item)
                size += len(batch[-1]["raw"])
            headers = {"Content-Type": "application/json"}
            if first["token"]:
                headers["Authorization"] = "Bearer " + first["token"]
            if first["slow"]:
                # /hook admits asynchronously; an inline consolidation must not
                # hold the whole batch behind the relay's forwarding timeout.
                url = urlsplit(first["url"])
                target = url.path + "?" + url.query
                payload = json.dumps(first["body"]).encode()
            else:
                target = "/hook/batch"
                payload = json.dumps([dict(url=i["url"], body=i["body"]) for i in batch]).encode()
            requests += 1
            try:
                # Never trust a 2xx, redirect, or prefix/indices ACK as a receipt.
                self.client.exchange("POST", target, headers, payload)
            except (OSError, ValueError):
                result["errors"] += 1
            for item in batch:
                try:
                    checkpoint = self._checkpoint(item)
                    if checkpoint and checkpoint[1] is not None:
                        self._archive(item, checkpoint)
                        result["delivered"] += 1
                    else:
                        result["unconfirmed"] += 1
                        # Fairness is in-memory scheduling, never an edit to
                        # attempts, age, identity, or the persisted envelope.
                        self._retry_after[item["path"].name] = time.monotonic() + 5
                except (OSError, ValueError, sqlite3.Error):
                    result["errors"] += 1
        result["pending"] = len(list(self.spool.glob("*.json")))
        return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spool", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--origin", required=True)
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args(argv)
    os.umask(0o077)
    stopped = threading.Event()
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda *_: stopped.set())
    drainer = None
    while not stopped.is_set():
        try:
            drainer = HookDelivery(args.spool, args.database, args.archive, args.origin)
            break
        except BlockingIOError:
            # A staged service may wait for the preservation guard to hand
            # over the native flock without restarting any existing service.
            stopped.wait(0.05)
    if drainer is None:
        return 0
    try:
        while not stopped.is_set():
            print(json.dumps(drainer.drain_once(), sort_keys=True), flush=True)
            if args.once:
                break
            stopped.wait(1)
    finally:
        drainer.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
