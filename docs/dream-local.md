# Local dream: operation and pilot

Dream is off by default. The manual command uses the running server's existing
writer and plans by default; it never opens another database writer. Both manual
and scheduled runs require an exact `dream.scopes` allowlist. Scope lookup never
creates a workspace or project, and `serve --project` is not a scheduler filter.

## Configuration

Prepare an independent laboratory configuration. Do not copy production secrets
or change decay parameters, access history or timestamps to manufacture cold
production pages. Record the baseline after server initialization has settled:
the existing server startup embedding backfill can still run independently of
dream; `backfill_on_start` controls managed launches, not that native startup pass. The values below are a template: verify the endpoint and model
identities locally before replacing the model placeholders.

```toml
# Relevant fragment of a separate laboratory config, not a production config.
bind = "127.0.0.1:49474"
server_url = "http://127.0.0.1:49474"
consolidate_on_session_end = false
backfill_on_start = false
run_autowire = false

# Matching vectors must already exist before planning. The configured embedder
# also serves the explicit scoped `embed` command after an applied batch.
embedding_provider = "openai-compat"
embedding_base_url = "http://127.0.0.1:8002/v1"
embedding_model = "REPLACE_WITH_VERIFIED_LOCAL_EMBEDDING_MODEL"
embedding_dim = 1024

[dream]
enabled = false
scopes = [{ workspace = "default", project = "dev" }]
llm_endpoint = "http://127.0.0.1:8000/v1"
llm_model = "REPLACE_WITH_VERIFIED_LOCAL_DRAFT_MODEL"
jev_model = "REPLACE_WITH_VERIFIED_LOCAL_SYSTEM_ONE_MODEL"
interval_secs = 3600
idle_window_secs = 1800
min_pts = 2
max_eps = 0.08
max_clusters_per_run = 1
min_cold_pages = 3

[maintenance]
enabled = false
embedding_backfill_interval_secs = 0

[auto_improve.scheduler]
enabled = false
```

`enabled` controls only the existing native dream scheduler. Manual execution
remains available while it is false. Enabling it requires a nonempty allowlist,
dedicated local draft endpoint/model, a semantic model, and a local embedder.
Unknown dream keys and duplicate or incomplete scopes fail configuration
validation. Safety ceilings below are fixed, not silently ignored config knobs.
No new timer, MCP tool, schema migration or version bump is introduced.

Draft requests use a dedicated credential-free OpenAI-compatible client, with
literal loopback endpoints, no redirects, proxies, retries or provider fallback.
The semantic endpoint is fixed to `http://127.0.0.1:8787/v1/systemone`; port 8789
is rejected. Configuring dream also selects the restricted transport for the
local OpenAI-compatible embedder. Admission remains required; dream refuses
remote admission destinations and any admission/sanitizer mutation of a gated
draft. Local service operators must verify the actual backend behind each alias.

## Commands

First verify that `server_url` identifies the laboratory server. `--data-dir`
alone does not change the destination of HTTP commands. The CLI below is a thin
client; it does not start a server or activate a scheduler.

```bash
AI_MEMORY_SERVER_URL=http://127.0.0.1:49474 \
  /absolute/path/to/reviewed/ai-memory --config /absolute/private/lab.toml \
  dream --workspace default --project dev

# Equivalent explicit spelling of the default:
AI_MEMORY_SERVER_URL=http://127.0.0.1:49474 \
  /absolute/path/to/reviewed/ai-memory --config /absolute/private/lab.toml \
  dream --workspace default --project dev --dry-run

# Only after checkpoint, baseline and pilot acceptance:
AI_MEMORY_SERVER_URL=http://127.0.0.1:49474 \
  /absolute/path/to/reviewed/ai-memory --config /absolute/private/lab.toml \
  dream --workspace default --project dev --apply
```

`POST /admin/dream` requires root authorization even without database users.
An unauthenticated single-operator server assigns its normal local-root identity
at the HTTP boundary. On authenticated installations use a private curl header
file containing the laboratory root Authorization header; never print it.

```bash
curl --fail-with-body --silent --show-error \
  --header 'Content-Type: application/json' \
  --header @/absolute/private/lab-root-header \
  --data '{"workspace":"default","project":"dev"}' \
  http://127.0.0.1:49474/admin/dream

# Explicit apply has the same authorization, scope and safety gates:
curl --fail-with-body --silent --show-error \
  --header 'Content-Type: application/json' \
  --header @/absolute/private/lab-root-header \
  --data '{"workspace":"default","project":"dev","apply":true}' \
  http://127.0.0.1:49474/admin/dream
```

Responses contain terminal status, scope, source ids, native version timestamps,
body/file/eligibility SHA256 digests, plan digest, conservative input estimate,
attempted inference calls, actual dedicated-client HTTP requests, cancellation,
skips, fixed reasons, premerge git revision and new
head ids needing embeddings. They contain no page bodies or backend error text.
Plans are informational: apply captures and checks a fresh plan; a dry-run report
is never persisted or replayed after restart. Manual and scheduled runs share one
serialization gate. `max_clusters_per_run` limits selected clusters globally
across the entire allowlisted round, including skipped selected clusters.

## Safety limits and cancellation

Before body/vector materialization or clustering, a scope above 4096 heads or
64 eligible cold candidates is refused, without sampling. Matching vector rows
are bounded to 4096, dimensions to 8192, and their complete blob lengths are
checked before loading (at most 128 MiB of materialized vectors).
Selected clusters contain at most two members. Each source body, indexed
frontmatter and complete wiki file must fit 6000 UTF-8 bytes; nothing is
truncated. Oversized clusters or input refuse the scope.

Input is conservatively estimated as one complete UTF-8 wire byte per token,
including JSON escaping, messages, model reserve and schema, with an 8192-token
ceiling. The local transport checks the exact encoded request for both gates
and drafting; the support gate additionally charges repeated state for each
independent source question. Output requests are capped at 4000 tokens; empty/invalid output
and drafts above the conservative 4000-byte bound fail closed. This deliberately
rejects some inputs that a particular tokenizer could fit. It is not a measured
tokenizer count. Each request has a 120-second deadline and the whole round has
a shared 300-second deadline, including semantic decisions. There are no hidden
compatibility retries: tolerant parsing uses the same response only.

The driver takes one activity snapshot for idle admission and cancellation.
HTTP/admin/MCP activity, accepted hook processing and relevant inference/work
reservations cancel a run even if the wall clock does not advance. Checks occur
before each request, after each response, before file installation and inside
the writer transaction before commit. Dream's own HTTP request does not mark
itself active; inert GET/HEAD health probes also preserve idle admission.
Read-only pool/file-lock waits obey the remaining round deadline and cancellation.
An already admitted request may finish on the backend after
client cancellation. An apply already awaiting the writer is drained through
commit/refusal and file rollback; cleanup is awaited rather than abandoned at
the deadline. Disconnecting the HTTP client does not abandon the admitted
operation, and server shutdown cancels admission then drains the run. This is a
bounded work budget, not a hard real-time disk-I/O SLA.

Every source id, hash, timestamp and eligibility input is revalidated in the
same SQL transaction that creates the new versions. The corresponding complete
file digests are checked under the same exclusive wiki mutation/page locks.
A concurrent edit, access/feedback change, expiry, pin or cancellation refuses
the whole batch. Legitimate sources remain shared across users; author identity
does not become a read filter. Successful merges stay episodic. All original
versions, including the survivor, remain reachable; all source provenance is
attached to the new heads. The source checkpoint is verified through actual git
file recovery before merging.

## Embedding completion and pilot acceptance

An applied report explicitly lists `embedding_required_ids`. Until the existing
scoped command completes, the new heads have no matching vectors and the run
must not be accepted as a completed hybrid-recall pilot:

```bash
AI_MEMORY_SERVER_URL=http://127.0.0.1:49474 \
  /absolute/path/to/reviewed/ai-memory --config /absolute/private/lab.toml \
  embed --workspace default --project dev --dry-run
AI_MEMORY_SERVER_URL=http://127.0.0.1:49474 \
  /absolute/path/to/reviewed/ai-memory --config /absolute/private/lab.toml \
  embed --workspace default --project dev
AI_MEMORY_SERVER_URL=http://127.0.0.1:49474 \
  /absolute/path/to/reviewed/ai-memory --config /absolute/private/lab.toml \
  embed --workspace default --project dev --dry-run
```

Verify zero missing vectors for the coordinate, including every reported new
id, and compare the same FTS/hybrid queries against known-answer facts before
and after. No embedding timer is required or added.

The two semantic decisions are cluster suitability and draft support. Draft
support independently asks whether every draft claim is supported and whether
each source's information, exceptions and conflicts survived. Every judgment
must meet 0.95; failure, degraded output, uncertainty and invalid shape block
apply. Numeric limits, permissions, paths, identities and hashes belong to code.
The [Jev evaluation decision](design-decisions.md) records the adapter reached
at the local endpoint, rejected prompt and calibration limits. Its synthetic
pilot reported `model=qwen`; the downstream destination and native Jev weights
remain unverified. Root must verify no port 8789 or external fallback behind
the adapter before enabling dream. Availability, a probability,
or mechanical tests do not certify complete factual coverage.

Root integration must still pilot actual Qwen drafting plus the local gate on
synthetic fixtures and an isolated real-copy no-op, with a versioned answer key,
review of unique facts, a final exception, temporal conflict, superseded decision,
invalid reference, malicious instruction and similar-but-unrelated subjects.
Compare content retrieval rather than stub filenames. Prove restore from the
reported premerge checkpoint and full rollback in the isolated copy. Do not
activate production or recurrence solely because unit tests pass.
