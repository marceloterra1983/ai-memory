# Data Handling

This page exists for reviewers who need a single place to answer "what data
does ai-memory collect, where does it live, and what (if anything) leaves the
machine" — typically a security, legal, or data-protection review ahead of
internal approval. It restates facts that already live in
[`SECURITY.md`](SECURITY.md), cited by section below, rather than making new
claims; if the two ever disagree, `SECURITY.md` is authoritative.

## Default: fully local, no telemetry

ai-memory does not phone home. There is no analytics, crash reporting, or
usage telemetry built into the binary. Everything it stores — wiki pages and
the SQLite database — lives under a single, operator-controlled data
directory on the local filesystem (`SECURITY.md`, "Local data
confidentiality"). External providers and integrations receive content only
when the operator configures them, as described below.

The out-of-the-box embedding default is in-process local inference
(all-MiniLM-L6-v2, 384-dim), with no API key or corpus/query egress. Initial
model setup can download checksum-pinned files; stage those files in advance
for an offline install. See [`docs/local-embeddings.md`](docs/local-embeddings.md).

There is no encryption at rest; ai-memory relies on OS filesystem permissions
(owner-only directories/files on Unix, ACLs on Windows — `SECURITY.md`, same section).

## Configured external providers and integrations

Cloud calls are off by default. A local or self-hosted provider sends content
to its configured endpoint rather than necessarily to a cloud service.

| path | what reaches the configured endpoint |
|---|---|
| Cloud LLM provider | bounded observations, page context, requested answers/digests, and bootstrap source material used by the selected operation |
| `embedding_provider = openai\|voyage\|google\|openai-compat` | page text needing a vector and eligible search queries; with `embedding_query_base_url` set, `openai-compat` query text goes to that endpoint and page text stays on `embedding_base_url`. In dream-local mode both endpoints must be literal loopback and query text stays within the bounded local client |
| `AI_MEMORY_RERANKER=llm` | each eligible live query + bounded page titles/snippets |
| Admission webhooks | page frontmatter/body and actor context for the configured write operations |

1. **LLM operations.** Configuring a cloud LLM provider can send captured
   observations and wiki context to consolidation, lint and learning-review
   calls. Explicit answer/digest requests and bootstrap can send additional
   query or repository material. This does not require assistant capture.
   Leave the provider unset or use a local endpoint to keep these operations
   on-host; see [`docs/llm-providers.md`](docs/llm-providers.md).
2. **Embedding provider.** `OpenAiEmbedder`, `VoyageEmbedder`, and
   `GoogleEmbedder` send page text and eligible search queries to their
   provider. This is not limited to new writes: restarting with a cloud
   provider triggers a background backfill of pages missing matching vectors.
   Explicit `POST /admin/embed` / `ai-memory embed --force` can also re-embed
   the existing corpus. Leave the provider on `local` (the default) or `none`
   when stored content must not leave the server.
3. **LLM-based search reranking** (`SECURITY.md`, "Search reranking is an
   outbound-data opt-in"). `AI_MEMORY_RERANKER=llm` sends each eligible live
   query plus bounded page titles/snippets to the configured LLM provider.
   The query is bounded but **not sanitized**; manually edited wiki content
   may also contain unsanitized text. Leave reranking off or use a local
   provider when queries and recalled snippets must stay on-host.
4. **Admission webhooks and operator backups.** Configured admission
   webhooks receive write content; remote wiki backups/sync send the selected
   files through the operator's own tools. Review those destinations separately;
   see [`docs/admission-webhooks.md`](docs/admission-webhooks.md) and
   [`docs/backup.md`](docs/backup.md).

**Assistant/Stop capture** remains a separate double opt-in: `capture_assistant`
on the server and `install-hooks --capture-assistant` on the client. It adds the
assistant's sanitized final-turn text to captured context and handoffs, and can
therefore add that text to LLM operations when a cloud provider is separately
configured. Capture alone does not send it to a provider. The flag is global
and has no per-project assistant-text exclusion.

Sanitization strips obvious credentials from managed writes and captured text,
but is best-effort. It is not a guarantee for manually edited pages, bootstrap
source material or live queries; see `SECURITY.md` for the exact caveats.

## Is this "personal data" under GDPR?

ai-memory stores developer-authored artifacts — wiki pages, session
handoffs, entity extractions — derived from coding-agent sessions. It has no
consent-tracking or data-subject-request tooling. Optional user accounts do
store identity/attribution fields such as username, name and email. In addition
to those fields, session material can contain personal data. In practice this means:

- If your team's session content incidentally contains personal data (e.g. a
  commit message with a name, or free text a developer typed), that data is
  stored and retained exactly like any other memory content, under your own
  organization's control on infrastructure you run.
- External transmission depends on configured providers, integrations and
  operator backup/sync tools described above; those destinations are under
  your control.
- Treat this the same way you'd treat any local database your team
  operates — your organization is the data controller and decides retention,
  access, and disclosure. This document does not constitute legal advice;
  confirm classification with your own data-protection function.

## Processor relationship

Self-hosted with no opt-in features enabled: there is no third party in the
data path, so ai-memory does not create a data-processor relationship under
GDPR Art. 28. If you configure a cloud LLM or embedding provider (for core
functionality, capture, or reranking), **that provider** becomes your
processor for the data you send it, under whatever agreement you have with
them directly — ai-memory is a conduit, not a party to that relationship.

## Deletion / retention

Most data persists until removed, but two mechanisms expire some of it without
operator action, both on by default:

- **Decay eviction of cold episodic pages.** The daily forget sweep
  (`[maintenance] forget_sweep_interval_secs`, default 86400) tombstones
  episodic pages whose relevance score falls below `[decay] cold_threshold`
  (default 0.20, with an ~80-day survival floor) and hard-deletes those
  tombstones after `[decay] hard_delete_after_days` (default 180). Semantic,
  procedural and pinned pages are exempt, and raw observations are kept
  (`observation_retention_days = 0`).
- **TTL pages.** After `expires_at`, search, briefing, GET recent, and the
  HTML Recent Activity column hide the page. `memory_read_page` still reads
  the markdown from disk and does not consult `expires_at`; `list_pages`, the
  sidebar, and GET .../pages keep listing the file. The next forget sweep
  hard-deletes that file and its rows even when the page is pinned.

Everything else persists until removed. Deletion is filesystem-level: removing
the data directory removes everything in it. For scoped deletion, a per-project
purge operation exists and is isolation-safe — it cannot delete files or rows
belonging to a different `(workspace_id, project_id)` (`SECURITY.md`,
"Per-project isolation").

## Network exposure (if you run the server non-loopback)

Binding the server beyond `127.0.0.1` is a separate, opt-in operational
choice unrelated to the data-export paths above — it controls who can reach
the API, not what ai-memory sends externally. See `SECURITY.md`'s "Network
exposure when binding to non-loopback addresses" section and
[`docs/https-via-proxy.md`](docs/https-via-proxy.md) before doing this.

## Related documents

- [`SECURITY.md`](SECURITY.md) — threat model, scope, vulnerability reporting.
- [`docs/sso.md`](docs/sso.md) — enterprise identity/SSO integration.
- [`docs/airgapped-install.md`](docs/airgapped-install.md) — offline/air-gapped
  installation.
