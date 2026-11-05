# Local runtime retained for the 3.4.2 port

This directory retains the fork's reliable MCP ingress, durable page outbox and receipt-based hook worker. It is source for the separately installed operator tooling; the Rust server does not start or install it. No installed relay files were changed by this port.

The hook worker holds one drain-lock descriptor for its lifetime and removes a spool entry only after its scoped `ingest_keys.completed_at` receipt is visible. An HTTP 202 alone does not authorize deletion. Generated TypeScript self-drains stay disabled; the operator must keep the receipt worker running or explicitly drain the native queue. Native spool retention remains seven days.

Unit templates and the installer staging patch are retained because the runtime staging code reads them. They are not an instruction to apply a historical patch to the current operator installer. Validate the installer and manifest separately before deployment. The backend absence/CAS fix is in the server, so the old backend patch is not included.

Run the synthetic worker suite from the repository root:

```sh
PYTHONPATH=local-reliable-mcp python3 -B -m unittest discover -s local-reliable-mcp/tests
```
