"""Deterministic delivery. Unknown baselines never overwrite a divergent head.

Jev does not apply. Status and CAS compare body bytes, metadata, and UUIDs.
Nothing here classifies free text or calls a model.
"""

from __future__ import annotations

from reliable_mcp.backend import Client
from reliable_mcp.outbox import (
    load_intent,
    op_lock,
    read_authorization,
    read_body,
    read_identity,
    save_intent,
)
from reliable_mcp.protocol import (
    matches,
    parse_message,
    read_arguments,
    summarize_page,
    tool_payload,
    write_arguments,
)
from reliable_mcp.safe import assert_not_root


def attempt(box, operation_id: str, client: Client) -> dict:
    assert_not_root()
    with op_lock(box, operation_id):
        intent = load_intent(box, operation_id)
        if intent["status"] != "pending":
            return intent
        if intent.get("reauthorization_required"):
            return intent
        if intent.get("auth", {}).get("present") and read_authorization(box, operation_id) is None:
            intent["reauthorization_required"] = True
            intent["reason"] = "reauthorization_required"
            intent["attempts"] = intent.get("attempts", 0) + 1
            save_intent(box, intent)
            return intent
        advertised = client.cas_available()
        if advertised is None:
            return _hold(box, intent, unknown=intent.get("baseline") is None)
        if advertised is False:
            # The server is up but would ignore CAS fields. Do not send.
            # The baseline stays unknown until a later read can see the head.
            intent["reason"] = "backend_missing_cas"
            return _hold(box, intent, unknown=intent.get("baseline") is None)
        return _online(box, intent, client)


def drain(box, client: Client) -> int:
    from reliable_mcp.outbox import iter_operation_ids

    delivered = 0
    for operation_id in iter_operation_ids(box):
        before = load_intent(box, operation_id)["status"]
        if before != "pending":
            continue
        after = attempt(box, operation_id, client)
        if before == "pending" and after["status"] == "delivered":
            delivered += 1
    return delivered


def _hold(box, intent: dict, unknown: bool) -> dict:
    if unknown:
        intent["baseline"] = {"state": "unknown"}
    intent["attempts"] = intent.get("attempts", 0) + 1
    save_intent(box, intent)
    return intent


def _online(box, intent: dict, client: Client) -> dict:
    kind, page = client.read_page(read_arguments(intent), _auth(box, intent), _extra(box, intent))
    if kind == "transport":
        return _hold(box, intent, unknown=intent.get("baseline") is None)
    if kind == "rejected":
        intent["reason"] = "read_rejected"
        return _hold(box, intent, unknown=False)
    was_unknown = isinstance(intent.get("baseline"), dict) and intent["baseline"].get("state") == "unknown"
    if intent.get("baseline") is None or was_unknown:
        intent["baseline"] = {"state": "absent"} if kind == "absent" else summarize_page(page)
        save_intent(box, intent)
        if was_unknown:
            if kind == "absent":
                return _send_observed(box, intent, client, kind, page)
            if kind == "present" and matches(intent, page):
                return _deliver(box, intent, page["page_id"], None)
            return _conflict(box, intent, page["page_id"] if kind == "present" else None)
    return _send_observed(box, intent, client, kind, page)


def _send_observed(box, intent: dict, client: Client, kind: str, page) -> dict:
    if kind == "present" and matches(intent, page):
        return _deliver(box, intent, page["page_id"], None)
    mode = intent["cas"]["mode"]
    if kind == "present" and mode == "create_only":
        return _conflict(box, intent, page["page_id"])
    if kind == "absent" and mode == "expected_page_id":
        return _conflict(box, intent, None)
    baseline = intent.get("baseline") or {"state": "unknown"}
    if kind == "present" and baseline.get("state") == "present":
        if page["page_id"] != baseline.get("page_id"):
            return _conflict(box, intent, page["page_id"])
        expected = baseline["page_id"]
        if mode == "expected_page_id":
            expected = intent["cas"]["expected_page_id"]
            if page["page_id"] != expected:
                return _conflict(box, intent, page["page_id"])
        return _send(box, intent, client, {"mode": "expected_page_id", "expected_page_id": expected})
    if kind == "absent" and baseline.get("state") in {"absent", "unknown"}:
        if mode == "expected_page_id":
            return _conflict(box, intent, None)
        return _send(box, intent, client, {"mode": "create_only"})
    if kind == "present" and baseline.get("state") == "absent":
        return _conflict(box, intent, page["page_id"])
    if kind == "absent" and baseline.get("state") == "present":
        return _conflict(box, intent, None)
    return _conflict(box, intent, page["page_id"] if kind == "present" else None)


def _send(box, intent: dict, client: Client, cas: dict) -> dict:
    intent["body"] = read_body(box, intent["operation_id"])
    arguments = write_arguments(intent, cas)
    response = client.write_page(arguments, _auth(box, intent), _extra(box, intent))
    intent["attempts"] = intent.get("attempts", 0) + 1
    save_intent(box, intent)
    if response.transport_failed or response.status != 200:
        return _reconcile(box, intent, client)
    message = parse_message(response.body)
    if message is None or "error" in message:
        return _reconcile(box, intent, client)
    if isinstance(message.get("result"), dict) and message["result"].get("isError") is True:
        return _reconcile(box, intent, client)
    payload = tool_payload(message) or {}
    claimed = payload.get("page_id")
    kind, page = client.read_page(read_arguments(intent), _auth(box, intent), _extra(box, intent))
    if kind == "present" and matches(intent, page):
        checkpoint = payload.get("checkpoint")
        if claimed == page["page_id"]:
            return _deliver(box, intent, page["page_id"], checkpoint)
        return _deliver(box, intent, page["page_id"], None)
    return _reconcile(box, intent, client)


def _reconcile(box, intent: dict, client: Client) -> dict:
    kind, page = client.read_page(read_arguments(intent), _auth(box, intent), _extra(box, intent))
    if kind in {"transport", "rejected"}:
        save_intent(box, intent)
        return intent
    if kind == "present" and matches(intent, page):
        return _deliver(box, intent, page["page_id"], None)
    baseline = intent.get("baseline") or {}
    if baseline.get("state") == "present" and kind == "present" and page["page_id"] == baseline.get("page_id"):
        save_intent(box, intent)
        return intent
    if baseline.get("state") == "absent" and kind == "absent":
        save_intent(box, intent)
        return intent
    return _conflict(box, intent, page["page_id"] if kind == "present" else None)


def _deliver(box, intent: dict, page_id: str, checkpoint) -> dict:
    intent["status"] = "delivered"
    intent["page_id"] = page_id
    intent["checkpoint"] = checkpoint
    intent["reason"] = None
    intent["current_head_page_id"] = None
    save_intent(box, intent)
    return intent


def _conflict(box, intent: dict, head) -> dict:
    intent["status"] = "conflict"
    intent["page_id"] = None
    intent["current_head_page_id"] = head
    intent["reason"] = "divergent_head"
    save_intent(box, intent)
    return intent


def _auth(box, intent: dict) -> str | None:
    if not intent.get("auth", {}).get("present"):
        return None
    return read_authorization(box, intent["operation_id"])


def _extra(box, intent: dict) -> dict:
    return read_identity(box, intent["operation_id"])


def confirm_delivered(box, operation_id: str, client: Client) -> dict:
    """Read the live page. Do not write, and do not flip a stored delivered op."""
    intent = load_intent(box, operation_id)
    kind, page = client.read_page(read_arguments(intent), _auth(box, intent), _extra(box, intent))
    shown = dict(intent)
    if kind == "present" and matches(intent, page):
        shown["status"] = "delivered"
        shown["page_id"] = page["page_id"]
        return shown
    shown["page_id"] = None
    shown["checkpoint"] = None
    if kind == "present":
        shown["status"] = "conflict"
        shown["current_head_page_id"] = page["page_id"]
        shown["reason"] = "divergent_head"
        return shown
    shown["status"] = "pending"
    shown["reason"] = "unconfirmed"
    shown["unconfirmed"] = True
    return shown


def accept_current_head(box, operation_id: str, client: Client) -> dict:
    """Requeue against the live head. A later move conflicts again."""
    assert_not_root()
    with op_lock(box, operation_id):
        intent = load_intent(box, operation_id)
        kind, page = client.read_page(read_arguments(intent), _auth(box, intent), _extra(box, intent))
        if kind != "present":
            intent["reason"] = "head_unreadable"
            save_intent(box, intent)
            return intent
        intent["baseline"] = summarize_page(page)
        intent["cas"] = {"mode": "expected_page_id", "expected_page_id": page["page_id"]}
        intent["status"] = "pending"
        intent["page_id"] = None
        intent["checkpoint"] = None
        intent["current_head_page_id"] = None
        intent["reason"] = None
        save_intent(box, intent)
        return intent
