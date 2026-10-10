"""Operator commands. serve, worker, replay, and reconcile refuse root."""

from __future__ import annotations

import argparse
import signal
import sys
import threading
from pathlib import Path

from reliable_mcp.backend import Client
from reliable_mcp.deliver import accept_current_head, attempt, drain
from reliable_mcp.outbox import DEFAULT_OUTBOX, ensure_outbox, list_intents, load_intent
from reliable_mcp.ready import probe
from reliable_mcp.safe import assert_not_root

POLL_SECONDS = 1.0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="reliable-mcp")
    sub = parser.add_subparsers(dest="command", required=True)

    serve = sub.add_parser("serve")
    serve.add_argument("--listen", default="127.0.0.1:49374")
    serve.add_argument("--backend", default="http://127.0.0.1:49376")
    serve.add_argument("--outbox", default=None)
    serve.add_argument("--max-body", type=int, default=10 * 1024 * 1024)
    serve.set_defaults(func=_serve)

    worker = sub.add_parser("worker")
    worker.add_argument("--once", action="store_true")
    worker.add_argument("--backend", default="http://127.0.0.1:49376")
    worker.add_argument("--outbox", default=None)
    worker.set_defaults(func=_worker)

    ready = sub.add_parser("ready")
    ready.add_argument("--backend", required=True)
    ready.add_argument("--workspace", required=True)
    ready.add_argument("--project", required=True)
    ready.set_defaults(func=_ready)

    status = sub.add_parser("status")
    status.add_argument("--outbox", default=None)
    status.set_defaults(func=_status)

    replay = sub.add_parser("replay")
    replay.add_argument("--outbox", default=None)
    replay.add_argument("--backend", required=True)
    replay.add_argument("--operation", required=True)
    replay.set_defaults(func=_replay)

    reconcile = sub.add_parser("reconcile")
    reconcile.add_argument("--outbox", default=None)
    reconcile.add_argument("--backend", required=True)
    reconcile.add_argument("--operation", required=True)
    reconcile.add_argument("--accept-current-head", action="store_true")
    reconcile.set_defaults(func=_reconcile)

    bootstrap = sub.add_parser("bootstrap")
    bootstrap.add_argument("--backend", required=True)
    bootstrap.add_argument("--outbox", default=None)
    bootstrap.set_defaults(func=_bootstrap)

    install = sub.add_parser("install")
    install.add_argument("--dry-run", action="store_true")
    install.add_argument("--plan", action="store_true")
    install.add_argument("--stage", default=None)
    install.add_argument("--unit", default=None)
    install.set_defaults(func=_install)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except Exception as exc:
        print(f"{type(exc).__name__}: {exc}", file=sys.stderr)
        return 1


def _box(value) -> Path:
    return ensure_outbox(Path(value) if value else DEFAULT_OUTBOX)


def _scope_label(scope) -> str:
    if not isinstance(scope, dict):
        return ""
    if scope.get("kind") == "global":
        return "global"
    return f"{scope.get('workspace', '')} {scope.get('project', '')}".strip()


def _serve(args) -> int:
    assert_not_root()
    from reliable_mcp.relay import Relay

    relay = Relay(
        listen=args.listen,
        backend=args.backend,
        outbox=_box(args.outbox),
        auto_drain_seconds=0,
        max_body=args.max_body,
    )
    relay.start()
    print(f"listen {relay.url}", flush=True)
    stop = threading.Event()

    def _stop(signum, _frame):
        stop.set()

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    try:
        while not stop.wait(3600):
            pass
    finally:
        relay.stop()
    return 0


def _worker(args) -> int:
    assert_not_root()
    box = _box(args.outbox)
    client = Client(args.backend, timeout=8)
    if args.once:
        count = drain(box, client)
        print(f"delivered {count}")
        return 0
    stop = threading.Event()

    def _stop(signum, _frame):
        stop.set()

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    while not stop.is_set():
        try:
            drain(box, client)
        except Exception as exc:
            print(type(exc).__name__, file=sys.stderr)
        stop.wait(POLL_SECONDS)
    return 0


def _ready(args) -> int:
    ok, reason = probe(args.backend, args.workspace, args.project)
    print("ready" if ok else reason)
    return 0 if ok else 1


def _status(args) -> int:
    box = _box(args.outbox)
    for intent in list_intents(box):
        print(
            f"{intent.get('status')} {intent.get('operation_id')} {intent.get('path')} {_scope_label(intent.get('scope'))}"
        )
    return 0


def _replay(args) -> int:
    assert_not_root()
    box = _box(args.outbox)
    intent = load_intent(box, args.operation)
    if intent["status"] == "conflict":
        print(f"conflict {intent['operation_id']} {intent['path']}")
        return 0
    if intent["status"] == "pending":
        intent = attempt(box, args.operation, Client(args.backend, timeout=8))
    print(f"{intent['status']} {intent['operation_id']} {intent['path']}")
    return 0


def _reconcile(args) -> int:
    assert_not_root()
    if not args.accept_current_head:
        print("accept-current-head required", file=sys.stderr)
        return 2
    box = _box(args.outbox)
    intent = accept_current_head(box, args.operation, Client(args.backend, timeout=8))
    if intent["status"] != "pending":
        print(intent["status"], file=sys.stderr)
        return 1
    print(f"requeued {intent['operation_id']}")
    return 0


def _bootstrap(args) -> int:
    from reliable_mcp.outbox import save_cache
    from reliable_mcp.protocol import cas_advertised, parse_message

    box = _box(args.outbox)
    client = Client(args.backend, timeout=8)
    init = client.rpc(
        {
            "jsonrpc": "2.0",
            "id": "bootstrap-init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "reliable-mcp", "version": "1"},
            },
        }
    )
    listed = client.rpc({"jsonrpc": "2.0", "id": "bootstrap-tools", "method": "tools/list", "params": {}})
    init_message = parse_message(init.body) if init.status == 200 and not init.transport_failed else None
    tools_message = parse_message(listed.body) if listed.status == 200 and not listed.transport_failed else None
    result = (init_message or {}).get("result") if isinstance(init_message, dict) else None
    tools = (tools_message or {}).get("result", {}).get("tools") if isinstance(tools_message, dict) else None
    if not isinstance(result, dict) or not cas_advertised(tools):
        print("backend missing create_only", file=sys.stderr)
        return 1
    save_cache(
        box,
        {
            "cas": True,
            "protocolVersion": result.get("protocolVersion"),
            "capabilities": result.get("capabilities"),
            "serverInfo": result.get("serverInfo"),
            "tools": tools,
        },
    )
    print("cached")
    return 0


def _install(args) -> int:
    from reliable_mcp.install import ROOT, dry_run_text, stage

    if args.unit:
        unit_text = Path(args.unit).read_text(encoding="utf-8")
    else:
        unit_text = (ROOT / "templates" / "backend-bind.conf").read_text(encoding="utf-8")
    if args.stage and not args.dry_run and not args.plan:
        stage(Path(args.stage), unit_text)
        print(f"staged {args.stage}")
        return 0
    print(dry_run_text(unit_text), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
