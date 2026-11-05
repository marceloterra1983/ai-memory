# Local dream

The native dream scheduler and the manual driver share an exact scope
allowlist, one run at a time, a global cluster allowance and a 300-second round
deadline. `enabled=false` stops scheduling and still permits manual plans and
apply. An empty allowlist admits no scope. Names are matched literally; reads
resolve existing scopes and never create a typoed project.

```toml
[dream]
enabled = false
scopes = [{ workspace = "default", project = "dev" }]
interval_secs = 3600
idle_window_secs = 1800
min_pts = 2
max_eps = 0.08
max_clusters_per_run = 1
min_cold_pages = 3
```

That configuration is valid without local inference settings. Planning needs
an available local embedder and matching stored vectors. Apply additionally
requires `llm_endpoint`, `llm_model` and `jev_model`. The draft endpoint must be
HTTP on literal `127.0.0.1` or `[::1]`, with an explicit port. Jev uses
`http://127.0.0.1:8787/v1/systemone`; port 8789 is excluded. The dedicated clients
use no primary-provider credentials, proxies, redirects, retry or fallback.
Unknown keys in `[dream]` or its scope entries are errors.

```sh
ai-memory dream --workspace default --project dev
ai-memory dream --workspace default --project dev --apply
```

The thin client calls `POST /admin/dream` with explicit `workspace`, `project`
and optional `apply` (false by default). The endpoint follows the existing admin
policy: bootstrap access for an install that does not distinguish operators;
root once database users or trusted-proxy identities exist. It accepts no
replacement body or inferred scope. A scope outside the allowlist is refused.

Planning performs bounded metadata/vector/file reads without inference or wiki
writes. A selected cluster contains two episodic, unpinned, uncompacted cold
pages. The whole scope is refused at a head, vector, candidate, source-byte or
input-budget ceiling; it is never sampled or truncated. Source expiry instants,
including absence, must match exactly and remain unchanged at apply.

Jev judges only whether the complete sources concern the same subject. Rejected,
uncertain, malformed, degraded or timed-out judgments refuse the draft. Support
and preservation use deterministic proof: the body must contain exactly both
complete source bodies, separated by two newlines, and the title must be copied
from a source body. Rewording, lost keep-tokens, negation or exceptions, invented
facts and changed bindings are refusals. The legacy `dropped_keep_tokens` report
field remains empty; reports expose paths, IDs, hashes and terminal reasons.

Apply reuses the Wiki batch funnel: sanitizer and admission checks, confined
paths, ordered page/OS locks, atomic installation, one writer transaction and
rollback. Every source's head, body hash, full file hash and eligibility snapshot
must still match. The writer repeats scope/head/hash/eligibility/TTL checks
before committing. Admission may refuse but cannot change reviewed content.
The premerge checkpoint includes only the selected sources and verifies their
exact bytes; it retains the tag's repository confinement checks.

HTTP work, MCP tools, hooks (including work after a 202 response), actual
embedding inference and relevant background jobs reserve activity. Activity
under clock skew cancels via a generation counter. A cancelled lock wait releases
its acquired locks. Once files are installed, shutdown or client disconnect
still drains the writer/rollback boundary.

Both premerge versions remain in the supersession chain and checkpoint. Loser
stubs drop stale summary/abstract metadata. Successful receipts list
`embedding_required_ids`; restore their vectors with the existing explicitly
scoped embedding command. Metadata-only curation retains compatible vectors
without inference.

A nonempty `dream.scopes` continues selecting restricted full-document compat
embedding even with `enabled=false`, as in phase A. Dream does not change the
embedding coordinate or re-embed an existing collection silently.
