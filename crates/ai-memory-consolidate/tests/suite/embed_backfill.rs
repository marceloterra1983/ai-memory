//! Integration tests for [`run_embedding_backfill`]: candidate
//! scanning, skip/dry-run/reembed semantics, and batched writes, using
//! a temp-dir store + wiki and the deterministic `SyntheticEmbedder`.

use std::sync::{Arc, Mutex};

use ai_memory_consolidate::{EmbedBackfillCounts, EmbedBackfillOptions, run_embedding_backfill};
use ai_memory_core::{PagePath, ProjectId, Tier, WorkspaceId};
use ai_memory_llm::{Embedder, OpenAiCompatEmbedder, SyntheticEmbedder};
use ai_memory_store::Store;
use ai_memory_wiki::{Wiki, WritePageRequest};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    _tmp: TempDir,
    store: Store,
    wiki: Wiki,
    ws: WorkspaceId,
    proj: ProjectId,
    embedder: Arc<dyn Embedder>,
}

async fn fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let store = Store::open(tmp.path()).expect("open store");
    let ws = store
        .writer
        .get_or_create_workspace("default")
        .await
        .expect("ws");
    let proj = store
        .writer
        .get_or_create_project(ws, "scratch", None)
        .await
        .expect("proj");
    let embedder: Arc<dyn Embedder> = Arc::new(SyntheticEmbedder::new(64));
    // No embedder attached: pages written here start without embedding
    // rows so the backfill has work to do.
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).expect("wiki");
    Fixture {
        _tmp: tmp,
        store,
        wiki,
        ws,
        proj,
        embedder,
    }
}

impl Fixture {
    async fn write_page(&self, path: &str, body: &str) {
        self.wiki
            .write_page(WritePageRequest {
                workspace_id: self.ws,
                project_id: self.proj,
                path: PagePath::new(path.to_string()).expect("path"),
                frontmatter: serde_json::json!({"title": path}),
                body: body.to_string(),
                tier: Tier::Semantic,
                pinned: false,
                title: None,
                admission_ctx: None,
                author_id: None,
                actor: ai_memory_core::ActorContext::anonymous(),
                evidence: Vec::new(),
            })
            .await
            .expect("write page");
    }

    async fn embedded_count(&self) -> usize {
        self.store
            .reader
            .embedded_page_ids(
                self.ws,
                self.proj,
                self.embedder.provider().to_string(),
                self.embedder.model().to_string(),
                self.embedder.dim(),
            )
            .await
            .expect("embedded page ids")
            .len()
    }

    async fn backfill(&self, reembed: bool, dry_run: bool) -> EmbedBackfillCounts {
        run_embedding_backfill(
            &self.store.reader,
            &self.store.writer,
            &self.wiki,
            &self.embedder,
            self.ws,
            self.proj,
            EmbedBackfillOptions { reembed, dry_run },
        )
        .await
        .expect("backfill")
    }
}

#[tokio::test]
async fn backfill_embeds_missing_and_skips_current_and_empty_pages() {
    let f = fixture().await;
    f.write_page("notes/a.md", "writer actor uses an mpsc channel")
        .await;
    f.write_page("notes/b.md", "hybrid retrieval combines fts and vectors")
        .await;
    f.write_page("notes/empty.md", "   \n").await;

    let counts = f.backfill(false, false).await;
    assert_eq!(counts.embedded, 2);
    assert_eq!(counts.failed, 0);
    assert_eq!(
        counts.skipped, 1,
        "the whitespace-only page should be skipped"
    );
    assert_eq!(f.embedded_count().await, 2);

    let second = f.backfill(false, false).await;
    assert_eq!(second.embedded, 0, "second pass embeds nothing new");
    assert_eq!(
        second.skipped, 3,
        "already-embedded pages and the empty page are skipped"
    );
    assert_eq!(f.embedded_count().await, 2);
}

#[tokio::test]
async fn backfill_dry_run_counts_without_writing() {
    let f = fixture().await;
    f.write_page("notes/a.md", "dry run should not write embeddings")
        .await;

    let counts = f.backfill(false, true).await;
    assert_eq!(counts.would_embed, 1);
    assert_eq!(counts.embedded, 0);
    assert_eq!(
        f.embedded_count().await,
        0,
        "dry run must not store embeddings"
    );
}

#[tokio::test]
async fn backfill_reembed_regenerates_existing_rows() {
    let f = fixture().await;
    f.write_page("notes/a.md", "reembed regenerates current rows")
        .await;
    assert_eq!(f.backfill(false, false).await.embedded, 1);

    let counts = f.backfill(true, false).await;
    assert_eq!(
        counts.embedded, 1,
        "reembed should regenerate the existing row"
    );
    assert_eq!(counts.skipped, 0);
    assert_eq!(f.embedded_count().await, 1);
}

/// A fake embedder standing in for `OpenAiEmbedder`/`OpenAiCompatEmbedder`
/// with a configurable document prefix: `identity_suffix` plays the role a
/// non-empty `document_prefix` plays there (`Embedder::model_identity`
/// diverges from `Embedder::model` when set), and `fail_on_substring` lets
/// one test drive a genuine per-page provider failure without a real
/// network call.
struct ConfigurableEmbedder {
    dim: u32,
    identity_suffix: String,
    fail_on_substring: Option<&'static str>,
}

#[async_trait::async_trait]
impl Embedder for ConfigurableEmbedder {
    fn provider(&self) -> &'static str {
        "synthetic"
    }

    fn model(&self) -> &str {
        "bag-of-words-v1"
    }

    fn model_identity(&self) -> String {
        if self.identity_suffix.is_empty() {
            self.model().to_string()
        } else {
            format!("{}+{}", self.model(), self.identity_suffix)
        }
    }

    fn dim(&self) -> u32 {
        self.dim
    }

    async fn embed(&self, text: &str) -> ai_memory_llm::LlmResult<Vec<f32>> {
        if let Some(marker) = self.fail_on_substring
            && text.contains(marker)
        {
            return Err(ai_memory_llm::LlmError::Provider {
                status: 500,
                body: "synthetic configured failure".into(),
            });
        }
        Ok(vec![0.5_f32; self.dim as usize])
    }
}

/// A document-prefix change (a new `identity_suffix` here) must make a
/// **normal** (non-force) backfill pass treat every page as needing a
/// fresh embedding, exactly like a model change would — this is what
/// keeps `memory_query`'s retrieval eligibility filter (keyed on the
/// currently configured embedder's `model_identity`) from ever mixing
/// vectors embedded under different prefixes. The old row is left in
/// place (nothing here deletes it — deletion is a separate, explicit step
/// mirroring `POST /admin/embed`'s `reembed` purge, covered by the next
/// test), but it is no longer reachable under the newly configured
/// identity.
#[tokio::test]
async fn document_prefix_change_makes_backfill_treat_pages_as_stale_and_reembeds_them() {
    let f = fixture().await;
    f.write_page("notes/a.md", "alpha content").await;

    let legacy: Arc<dyn Embedder> = Arc::new(ConfigurableEmbedder {
        dim: 8,
        identity_suffix: String::new(),
        fail_on_substring: None,
    });
    let first = run_embedding_backfill(
        &f.store.reader,
        &f.store.writer,
        &f.wiki,
        &legacy,
        f.ws,
        f.proj,
        EmbedBackfillOptions::default(),
    )
    .await
    .expect("first backfill");
    assert_eq!(first.embedded, 1);

    // Simulates reconfiguring `embedding_document_prefix`: same provider,
    // same wire model, same dim — only the document-side identity differs.
    let prefixed: Arc<dyn Embedder> = Arc::new(ConfigurableEmbedder {
        dim: 8,
        identity_suffix: "dp-test".into(),
        fail_on_substring: None,
    });
    assert_ne!(legacy.model_identity(), prefixed.model_identity());

    // Between the config change and the next backfill pass, the stored
    // row is still under the OLD identity — this is the property the fix
    // protects: a query using the NOW-configured embedder's identity (what
    // `memory_query`'s retrieval eligibility filter does) does not match
    // it, so the page is excluded from vector search rather than silently
    // matched with content embedded under the old prefix.
    let prefixed_ids_before = f
        .store
        .reader
        .embedded_page_ids(
            f.ws,
            f.proj,
            prefixed.provider().to_string(),
            prefixed.model_identity(),
            prefixed.dim(),
        )
        .await
        .expect("prefixed embedded_page_ids before backfill");
    assert_eq!(
        prefixed_ids_before.len(),
        0,
        "the pre-change row must not be matched under the new identity"
    );

    let second = run_embedding_backfill(
        &f.store.reader,
        &f.store.writer,
        &f.wiki,
        &prefixed,
        f.ws,
        f.proj,
        EmbedBackfillOptions::default(), // reembed: false — a NORMAL pass
    )
    .await
    .expect("backfill after a document-prefix change");
    assert_eq!(
        second.embedded, 1,
        "a document-prefix change must be treated as stale and re-embedded, \
         not silently skipped as \"already embedded\""
    );
    assert_eq!(second.skipped, 0);

    // `page_embeddings` is one row per page (replace-on-write), so the
    // fresh write replaces the pre-change row rather than accumulating
    // alongside it: the old identity now matches nothing …
    let legacy_ids_after = f
        .store
        .reader
        .embedded_page_ids(
            f.ws,
            f.proj,
            legacy.provider().to_string(),
            legacy.model_identity(),
            legacy.dim(),
        )
        .await
        .expect("legacy embedded_page_ids after backfill");
    assert_eq!(
        legacy_ids_after.len(),
        0,
        "the pre-change row was replaced, not left behind, by the re-embed"
    );
    // … and the new identity now does.
    let prefixed_ids_after = f
        .store
        .reader
        .embedded_page_ids(
            f.ws,
            f.proj,
            prefixed.provider().to_string(),
            prefixed.model_identity(),
            prefixed.dim(),
        )
        .await
        .expect("prefixed embedded_page_ids after backfill");
    assert_eq!(prefixed_ids_after.len(), 1);
}

/// Mirrors `POST /admin/embed`'s `reembed: true` sequence exactly
/// (`ai-memory-mcp/src/admin.rs::handle_embed`): purge stale rows for the
/// configured triple, then run a forced backfill. One of two pages fails
/// mid-run; the purge must still have applied to both, the surviving page
/// must still get a fresh row, and the failed page must be left with NO
/// row (not a stale one and not a half-written one) rather than silently
/// keeping a purged-then-never-replaced gap invisible to the operator —
/// `counts.failed` is what surfaces it.
#[tokio::test]
async fn force_rebuild_purges_first_and_counts_a_partial_failure_correctly() {
    let f = fixture().await;
    f.write_page("notes/ok.md", "alpha content").await;
    f.write_page("notes/bad.md", "FAIL_MARKER content").await;

    // Seed both pages under a stand-in "old" identity, matching a real
    // force-rebuild's starting point: pages already have rows, possibly
    // under a different identity than the one about to be forced.
    let legacy: Arc<dyn Embedder> = Arc::new(ConfigurableEmbedder {
        dim: 8,
        identity_suffix: "old".into(),
        fail_on_substring: None,
    });
    let seeded = run_embedding_backfill(
        &f.store.reader,
        &f.store.writer,
        &f.wiki,
        &legacy,
        f.ws,
        f.proj,
        EmbedBackfillOptions::default(),
    )
    .await
    .expect("seed backfill");
    assert_eq!(seeded.embedded, 2);

    let flaky: Arc<dyn Embedder> = Arc::new(ConfigurableEmbedder {
        dim: 8,
        identity_suffix: String::new(), // the configured (new) identity
        fail_on_substring: Some("FAIL_MARKER"),
    });

    // Step 1 (admin.rs's purge-before-reembed): delete every row in scope
    // that does not match the target triple — here that is BOTH seeded
    // rows, since they are under `legacy`'s identity.
    let purged = f
        .store
        .writer
        .delete_stale_page_embeddings(
            f.ws,
            Some(f.proj),
            flaky.provider().to_string(),
            flaky.model_identity(),
            flaky.dim(),
        )
        .await
        .expect("purge");
    assert_eq!(
        purged, 2,
        "purge removes every row under a non-matching identity"
    );

    // Step 2: forced backfill with the embedder that fails on notes/bad.md.
    let counts = run_embedding_backfill(
        &f.store.reader,
        &f.store.writer,
        &f.wiki,
        &flaky,
        f.ws,
        f.proj,
        EmbedBackfillOptions {
            reembed: true,
            dry_run: false,
        },
    )
    .await
    .expect("forced backfill with a partial failure");
    assert_eq!(counts.embedded, 1, "notes/ok.md succeeds");
    assert_eq!(counts.failed, 1, "notes/bad.md's provider call fails");

    let current_ids = f
        .store
        .reader
        .embedded_page_ids(
            f.ws,
            f.proj,
            flaky.provider().to_string(),
            flaky.model_identity(),
            flaky.dim(),
        )
        .await
        .expect("embedded_page_ids after the forced rebuild");
    assert_eq!(
        current_ids.len(),
        1,
        "exactly the succeeding page has a row under the new identity; \
         the failed page is left with none — purged, not silently kept \
         stale, and not half-written — so it surfaces as `failed` for the \
         operator to retry rather than looking either fully migrated or \
         still on the old prefix"
    );
}

/// Only the provider HTTP boundary is replaced; wiki, FTS and the writer actor
/// remain real. Content-dependent vectors expose a lost tail, and the failure
/// marker makes a late chunk fail after earlier chunks have succeeded.
struct LocalEmbeddingServer {
    endpoint: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl LocalEmbeddingServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let worker = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (body_start, body_len) = loop {
                    let mut buffer = [0u8; 1024];
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(read, 0, "embedding request must contain a complete header");
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..end]).unwrap();
                        assert!(header.starts_with("POST /v1/embeddings "));
                        assert!(!header.to_ascii_lowercase().contains("authorization:"));
                        let len = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (end + 4, len);
                    }
                };
                while bytes.len() < body_start + body_len {
                    let mut buffer = [0u8; 1024];
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(read, 0, "embedding request must contain its complete body");
                    bytes.extend_from_slice(&buffer[..read]);
                }
                assert!(
                    body_len <= 8192,
                    "every complete wire request must fit the guard"
                );
                let request: serde_json::Value =
                    serde_json::from_slice(&bytes[body_start..body_start + body_len]).unwrap();
                let input = request["input"].as_str().unwrap();
                let embedding = if input.contains("LATEFAILMARKER") {
                    serde_json::json!([1.0])
                } else if input.contains("tailmarker") {
                    serde_json::json!([0.0, 1.0])
                } else {
                    serde_json::json!([1.0, 0.0])
                };
                recorded.lock().unwrap().push(request);
                let body = serde_json::json!({"data": [{"embedding": embedding}]}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            worker,
        }
    }

    fn embedder(&self) -> Arc<dyn Embedder> {
        Arc::new(
            OpenAiCompatEmbedder::new_local(&self.endpoint, "synthetic-integral", 2)
                .unwrap()
                .with_prefixes("query: ", "passage: "),
        )
    }

    fn inputs(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                assert_eq!(request["model"], "synthetic-integral");
                request["input"].as_str().unwrap().to_string()
            })
            .collect()
    }
}

impl Drop for LocalEmbeddingServer {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

type PageIndexSnapshot = (Vec<u8>, String, String, Vec<u8>, String, i64);

fn page_index_snapshot(f: &Fixture) -> Vec<PageIndexSnapshot> {
    let connection = rusqlite::Connection::open(f.store.db_path()).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT id,path,body,body_sha256,frontmatter_json,is_latest FROM pages ORDER BY path",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn local_backfill_embeds_twenty_two_oversized_utf8_pages_without_changing_wiki_or_fts() {
    let mut f = fixture().await;
    let server = LocalEmbeddingServer::start().await;
    f.embedder = server.embedder();
    let mut pages = Vec::new();
    for index in 0..22 {
        let path = PagePath::new(format!("notes/integral-{index}.md")).unwrap();
        let body = format!(
            "PAGE{index}START {} tailmarker{index}",
            "é🦀 quoted \"text\" \\ line\n\t".repeat(700)
        );
        assert!(body.len() > 16_384);
        f.write_page(path.as_str(), &body).await;
        let bytes = std::fs::read(f.wiki.abs_path(f.ws, f.proj, &path)).unwrap();
        pages.push((path, body, bytes));
    }
    let before = page_index_snapshot(&f);
    let first = f.backfill(false, false).await;
    assert_eq!(
        first.embedded, 22,
        "all oversized latest pages must receive a vector"
    );
    assert_eq!(first.failed, 0);
    assert_eq!(
        f.embedded_count().await,
        0,
        "wire model alone must not match prefixed identity"
    );
    let embeddings = f
        .store
        .reader
        .load_embeddings(
            f.ws,
            f.proj,
            f.embedder.provider().to_string(),
            f.embedder.model_identity(),
            f.embedder.dim(),
        )
        .await
        .unwrap();
    assert_eq!(embeddings.len(), 22);
    for (provider, model, dim) in [
        ("openai", f.embedder.model_identity(), 2),
        ("openai-compat", f.embedder.model_identity(), 3),
        ("openai-compat", "synthetic-integral".into(), 2),
    ] {
        let mismatched = f
            .store
            .reader
            .load_embeddings(f.ws, f.proj, provider.into(), model, dim)
            .await
            .unwrap();
        assert!(
            mismatched.is_empty(),
            "mismatched vector coordinates must stay ineligible"
        );
    }
    for embedding in embeddings {
        assert!(
            embedding.vector[1] > 0.0,
            "content after the first window must affect the vector"
        );
        let norm = embedding.vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }
    assert_eq!(
        page_index_snapshot(&f),
        before,
        "heads, bodies, hashes and metadata must remain identical"
    );
    let inputs = server.inputs();
    assert!(inputs.len() > 22);
    let complete = inputs
        .iter()
        .map(|input| {
            input
                .strip_prefix("passage: ")
                .expect("each document chunk needs its prefix")
        })
        .collect::<String>();
    assert_eq!(
        complete.len(),
        pages.iter().map(|(_, body, _)| body.len()).sum::<usize>()
    );
    for (path, body, bytes) in pages {
        assert!(
            complete.contains(&body),
            "every byte of every document must reach the provider"
        );
        assert_eq!(
            std::fs::read(f.wiki.abs_path(f.ws, f.proj, &path)).unwrap(),
            bytes
        );
        let marker = body.split_whitespace().last().unwrap();
        let hits = f
            .store
            .reader
            .search_pages_for_project(f.ws, f.proj, marker.into(), 30, None)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "the final content must remain searchable in FTS"
        );
        assert_eq!(hits[0].path, path);
    }
    let second = f.backfill(false, false).await;
    assert_eq!(second.embedded, 0);
    assert_eq!(second.failed, 0);
    assert_eq!(second.skipped, 22);
    assert_eq!(
        server.inputs().len(),
        inputs.len(),
        "matching rows must suppress provider calls"
    );
}

#[tokio::test]
async fn local_backfill_late_chunk_failure_never_persists_a_partial_document_vector() {
    let mut f = fixture().await;
    let server = LocalEmbeddingServer::start().await;
    f.embedder = server.embedder();
    let failed_body = format!("{} LATEFAILMARKER", "synthetic é🦀\n".repeat(1500));
    f.write_page("notes/failure.md", &failed_body).await;
    f.write_page("notes/control.md", "short legitimate control tailmarker")
        .await;
    let before = page_index_snapshot(&f);
    let report = f.backfill(false, false).await;
    assert_eq!(report.embedded, 1);
    assert_eq!(report.failed, 1);
    let inputs = server.inputs();
    assert!(
        inputs.len() > 2,
        "earlier chunks must reach the provider before the late failure"
    );
    assert!(inputs.iter().any(|input| input.contains("LATEFAILMARKER")));
    let embeddings = f
        .store
        .reader
        .load_embeddings(
            f.ws,
            f.proj,
            f.embedder.provider().into(),
            f.embedder.model_identity(),
            2,
        )
        .await
        .unwrap();
    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].path.as_str(), "notes/control.md");
    assert_eq!(page_index_snapshot(&f), before);
    let failed_path = PagePath::new("notes/failure.md").unwrap();
    assert_eq!(
        f.wiki.read_page(f.ws, f.proj, &failed_path).unwrap().body,
        failed_body
    );
    let hits = f
        .store
        .reader
        .search_pages_for_project(f.ws, f.proj, "LATEFAILMARKER".into(), 10, None)
        .await
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "failed embedding must preserve final source text in FTS"
    );
    assert_eq!(hits[0].path, failed_path);
}

#[tokio::test]
async fn local_wiki_write_embeds_complete_oversized_body_and_abstract() {
    let mut f = fixture().await;
    let server = LocalEmbeddingServer::start().await;
    f.embedder = server.embedder();
    f.wiki = f.wiki.with_embedder(f.embedder.clone());
    let path = PagePath::new("notes/eager.md").unwrap();
    let body = format!("{} tailmarkerbody", "synthetic body é🦀\n".repeat(1100));
    let abstract_text = format!(
        "{} tailmarkerabstract",
        "synthetic abstract é🦀 ".repeat(1000)
    );
    let id = f
        .wiki
        .write_page(WritePageRequest {
            workspace_id: f.ws,
            project_id: f.proj,
            path: path.clone(),
            frontmatter: serde_json::json!({"title": "eager", "abstract": abstract_text}),
            body: body.clone(),
            tier: Tier::Semantic,
            pinned: false,
            title: None,
            admission_ctx: None,
            author_id: None,
            actor: ai_memory_core::ActorContext::anonymous(),
            evidence: Vec::new(),
        })
        .await
        .unwrap();
    let body_ids = f
        .store
        .reader
        .embedded_page_ids(
            f.ws,
            f.proj,
            f.embedder.provider().into(),
            f.embedder.model_identity(),
            2,
        )
        .await
        .unwrap();
    let abstract_ids = f
        .store
        .reader
        .abstract_embedded_page_ids(
            f.ws,
            f.proj,
            f.embedder.provider().into(),
            f.embedder.model_identity(),
            2,
        )
        .await
        .unwrap();
    assert_eq!(body_ids, vec![id]);
    assert_eq!(abstract_ids, vec![id]);
    let stored = f.wiki.read_page(f.ws, f.proj, &path).unwrap();
    assert_eq!(stored.body, body);
    assert_eq!(stored.frontmatter["abstract"], abstract_text);
    let complete = server
        .inputs()
        .into_iter()
        .map(|input| input.strip_prefix("passage: ").unwrap().to_string())
        .collect::<String>();
    assert_eq!(complete, format!("{body}{abstract_text}"));
    let hits = f
        .store
        .reader
        .search_pages_for_project(f.ws, f.proj, "tailmarkerbody".into(), 10, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, path);
}
