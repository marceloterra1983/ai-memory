//! Dream CAS preserves every source through concurrent writes and git recovery.

use ai_memory_core::{ActorContext, PagePath, ProjectId, Tier, WorkspaceId};
use ai_memory_store::Store;
use ai_memory_wiki::{RelationCurationRequest, Wiki, WritePageRequest};
use std::sync::Arc;

fn page(ws: WorkspaceId, proj: ProjectId, path: &str, body: &str) -> WritePageRequest {
    WritePageRequest {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new(path).unwrap(),
        body: body.into(),
        frontmatter: serde_json::json!({"title":path}),
        tier: Tier::Episodic,
        pinned: false,
        title: None,
        admission_ctx: None,
        author_id: None,
        actor: ActorContext::anonymous(),
        evidence: vec![],
    }
}

async fn fixture() -> (tempfile::TempDir, Store, Wiki, WorkspaceId, ProjectId) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("synthetic")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "dream", None)
        .await
        .unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone())
        .unwrap()
        .with_store_reader(store.reader.clone());
    wiki.apply_batch(vec![
        page(ws, proj, "a.md", "fact A\nexception A\n"),
        page(ws, proj, "b.md", "fact B\nexception B\n"),
    ])
    .await
    .unwrap();
    (tmp, store, wiki, ws, proj)
}

#[tokio::test]
async fn dream_cas_refuses_metadata_curation_of_survivor_or_loser_without_body_change() {
    for curated_path in ["a.md", "b.md"] {
        let (_tmp, store, wiki, ws, proj) = fixture().await;
        let mut original = page(ws, proj, curated_path, "synthetic fact and exception\r\n");
        original.frontmatter = serde_json::json!({
            "title": curated_path, "relations": {"references": ["None", "Free title"]},
            "tags": ["preserve"], "abstract": "Synthetic abstract",
            "custom": {"nested": [1, true, null]}, "expires_at": "2099-01-01"
        });
        wiki.write_page(original).await.unwrap();
        let sources = wiki
            .snapshot_dream_sources(
                store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
                6000,
            )
            .await
            .unwrap();
        let source = sources
            .iter()
            .find(|s| s.head.candidate.path.as_str() == curated_path)
            .unwrap();
        let receipt = wiki
            .curate_relations(RelationCurationRequest {
                workspace_id: ws,
                project_id: proj,
                path: source.head.candidate.path.clone(),
                expected_head_id: source.head.candidate.id,
                expected_body_sha256: source.head.body_sha256,
                dry_run: false,
                actor: ActorContext::anonymous(),
            })
            .await
            .unwrap();
        assert!(receipt.applied);
        assert_ne!(receipt.page_id, source.head.candidate.id);
        let mut expected = source.markdown.clone();
        expected.frontmatter["relations"]["references"] = serde_json::json!(["Free title"]);
        assert_eq!(
            wiki.read_page(ws, proj, &source.head.candidate.path)
                .unwrap(),
            expected,
            "curation preserves the body and every unrelated frontmatter value"
        );
        let current = wiki
            .snapshot_dream_sources(
                store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
                6000,
            )
            .await
            .unwrap();
        let curated = current
            .iter()
            .find(|s| s.head.candidate.path.as_str() == curated_path)
            .unwrap();
        assert_eq!(curated.head.body_sha256, source.head.body_sha256);
        assert_ne!(
            curated.head.eligibility_sha256,
            source.head.eligibility_sha256
        );
        let drafts = vec![
            page(ws, proj, "a.md", "synthetic reviewed merge\n"),
            page(ws, proj, "b.md", "synthetic reviewed stub\n"),
        ];
        // First exercise the complete file guard. Then present current file
        // digests with stale SQL heads to independently exercise writer CAS and
        // rollback after files have been installed.
        let mut stale_sql = sources.clone();
        for stale in &mut stale_sql {
            stale.file_sha256 = current
                .iter()
                .find(|s| s.head.candidate.path == stale.head.candidate.path)
                .unwrap()
                .file_sha256;
        }
        for stale in [&sources, &stale_sql] {
            assert!(
                wiki.apply_dream_batch(drafts.clone(), stale, Arc::new(|| false))
                    .await
                    .is_err()
            );
            let after = wiki
                .snapshot_dream_sources(
                    store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
                    6000,
                )
                .await
                .unwrap();
            for saved in &current {
                let actual = after
                    .iter()
                    .find(|s| s.head.candidate.path == saved.head.candidate.path)
                    .unwrap();
                assert_eq!(actual.head.candidate.id, saved.head.candidate.id);
                assert_eq!(actual.file_sha256, saved.file_sha256);
                assert_eq!(actual.markdown, saved.markdown);
            }
        }
        // Refusing every batch cannot satisfy this legitimate fresh-snapshot control.
        assert_eq!(
            wiki.apply_dream_batch(drafts, &current, Arc::new(|| false))
                .await
                .unwrap()
                .ids
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn dream_cas_concurrent_edit_during_inference_cancels_entire_batch() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    // The model has the snapshot; another harness edits one source before it returns.
    wiki.write_page(page(ws, proj, "b.md", "new exception from other harness\n"))
        .await
        .unwrap();
    let result = wiki
        .apply_dream_batch(
            vec![
                page(ws, proj, "a.md", "merged\n"),
                page(ws, proj, "b.md", "stub\n"),
            ],
            &sources,
            Arc::new(|| false),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("b.md").unwrap())
            .unwrap()
            .body,
        "new exception from other harness\n"
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "a.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
}

#[tokio::test]
async fn dream_cas_external_disk_edit_is_preserved_and_long_source_is_refused() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let heads = store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap();
    assert!(
        wiki.snapshot_dream_sources(heads.clone(), 10)
            .await
            .is_err()
    );
    let sources = wiki.snapshot_dream_sources(heads, 6000).await.unwrap();
    let abs = wiki.abs_path(ws, proj, &PagePath::new("b.md").unwrap());
    std::fs::write(&abs, "external exception\n").unwrap();
    assert!(
        wiki.apply_dream_batch(
            vec![
                page(ws, proj, "a.md", "merged\n"),
                page(ws, proj, "b.md", "stub\n")
            ],
            &sources,
            Arc::new(|| false)
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(abs).unwrap(),
        "external exception\n"
    );
}

#[tokio::test]
async fn dream_cas_legitimate_apply_has_real_git_premerge_recovery() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let heads = store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap();
    let old_ids: Vec<_> = heads.iter().map(|h| h.candidate.id).collect();
    let sources = wiki.snapshot_dream_sources(heads, 6000).await.unwrap();
    let result = wiki
        .apply_dream_batch(
            vec![
                page(
                    ws,
                    proj,
                    "a.md",
                    "fact A\nfact B\nexception A\nexception B\n",
                ),
                page(ws, proj, "b.md", "merged into a.md\n"),
            ],
            &sources,
            Arc::new(|| false),
        )
        .await
        .unwrap();
    assert_eq!(result.ids.len(), 2);
    assert!(result.ids.iter().all(|id| !old_ids.contains(id)));
    wiki.restore_page_from_checkpoint(ws, proj, PagePath::new("b.md").unwrap(), &result.checkpoint)
        .await
        .unwrap();
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("b.md").unwrap())
            .unwrap()
            .body,
        "fact B\nexception B\n"
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "b.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "fact B\nexception B\n"
    );
}

#[tokio::test]
async fn dream_cas_cancel_refuses_files_and_heads() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    assert!(
        wiki.apply_dream_batch(
            vec![
                page(ws, proj, "a.md", "merged\n"),
                page(ws, proj, "b.md", "stub\n")
            ],
            &sources,
            Arc::new(|| true)
        )
        .await
        .is_err()
    );
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
}

#[tokio::test]
async fn dream_snapshot_refuses_unindexed_frontmatter_only_edit() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let heads = store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap();
    let path = PagePath::new("a.md").unwrap();
    let mut md = wiki.read_page(ws, proj, &path).unwrap();
    md.frontmatter["pinned"] = serde_json::json!(true);
    std::fs::write(
        wiki.abs_path(ws, proj, &path),
        ai_memory_wiki::emit(&md).unwrap(),
    )
    .unwrap();
    assert!(
        wiki.snapshot_dream_sources(heads, 6000).await.is_err(),
        "frontmatter changed eligibility even though the body digest is unchanged"
    );
}

#[tokio::test]
async fn dream_sql_refusal_restores_all_installed_files() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    let path = PagePath::new("b.md").unwrap();
    store
        .writer
        .upsert_page(ai_memory_core::NewPage {
            workspace_id: ws,
            project_id: proj,
            path: path.clone(),
            title: "b.md".into(),
            body: "concurrent actor-only write".into(),
            tier: Tier::Episodic,
            frontmatter_json: serde_json::json!({}),
            pinned: false,
            links: vec![],
            author_id: None,
            expires_at: None,
            entities: vec![],
            evidence: vec![],
        })
        .await
        .unwrap();
    assert!(
        wiki.apply_dream_batch(
            vec![
                page(ws, proj, "a.md", "merged\n"),
                page(ws, proj, "b.md", "stub\n")
            ],
            &sources,
            Arc::new(|| false)
        )
        .await
        .is_err()
    );
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
    assert_eq!(
        wiki.read_page(ws, proj, &path).unwrap().body,
        "fact B\nexception B\n"
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "a.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "b.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "concurrent actor-only write"
    );
}

#[tokio::test]
async fn dream_apply_refuses_admission_mutation_after_semantic_review() {
    use ai_memory_wiki::{AdmissionChain, AdmissionOp, FailurePolicy, WebhookConfig};
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    let mutate = Arc::new(AtomicBool::new(true));
    let mode = mutate.clone();
    let app=Router::new().route("/admission",post(move|Json(payload):Json<serde_json::Value>|{let mode=mode.clone();async move {
        if mode.load(Ordering::SeqCst) {Json(serde_json::json!({"page":{"body": "unsupported synthetic assertion\n", "frontmatter": payload["page"]["frontmatter"].clone()}})).into_response()}
        else {StatusCode::NO_CONTENT.into_response()}
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let wiki = wiki.with_admission_chain(
        AdmissionChain::new(vec![WebhookConfig {
            name: "synthetic".into(),
            url: format!("http://{addr}/admission"),
            timeout_ms: 500,
            failure_policy: FailurePolicy::Reject,
            events: vec![AdmissionOp::WritePage],
            blocking: true,
        }])
        .unwrap(),
    );
    let draft = vec![
        page(ws, proj, "a.md", "reviewed draft\n"),
        page(ws, proj, "b.md", "reviewed stub\n"),
    ];
    assert!(
        wiki.apply_dream_batch(draft.clone(), &sources, Arc::new(|| false))
            .await
            .is_err(),
        "a semantic gate must not authorize subsequently mutated content"
    );
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
    mutate.store(false, Ordering::SeqCst);
    wiki.apply_dream_batch(draft, &sources, Arc::new(|| false))
        .await
        .unwrap();
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "reviewed draft\n"
    );
    server.abort();
}

#[tokio::test]
async fn dream_checkpoint_excludes_uncommitted_foreign_scope_files() {
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    let foreign = store
        .writer
        .get_or_create_project(ws, "foreign", None)
        .await
        .unwrap();
    let path = PagePath::new("foreign.md").unwrap();
    wiki.write_page(page(
        ws,
        foreign,
        path.as_str(),
        "private synthetic foreign fact\n",
    ))
    .await
    .unwrap();
    let result = wiki
        .apply_dream_batch(
            vec![
                page(ws, proj, "a.md", "merge\n"),
                page(ws, proj, "b.md", "stub\n"),
            ],
            &sources,
            Arc::new(|| false),
        )
        .await
        .unwrap();
    let relative = std::path::PathBuf::from(ws.to_string())
        .join(foreign.to_string())
        .join(path.as_str());
    assert!(
        wiki.git()
            .file_at_rev(&result.checkpoint, &relative)
            .is_err(),
        "dream checkpoint must not materialize uncommitted foreign source files"
    );
    assert_eq!(
        wiki.read_page(ws, foreign, &path).unwrap().body,
        "private synthetic foreign fact\n"
    );
}

#[tokio::test]
async fn dream_admission_refuses_foreign_endpoints_and_redirects_with_local_control() {
    use ai_memory_wiki::{AdmissionChain, AdmissionOp, FailurePolicy, WebhookConfig};
    use axum::{Router, http::StatusCode, routing::post};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let path = PagePath::new("a.md").unwrap();
    for url in [
        "https://example.invalid/admission",
        "http://127.0.0.1:8789/admission",
        "http://localhost:8787/admission",
        "http://operator:synthetic@127.0.0.1:8787/admission",
    ] {
        let chain = AdmissionChain::new(vec![WebhookConfig {
            name: "synthetic".into(),
            url: url.into(),
            timeout_ms: 100,
            failure_policy: FailurePolicy::Reject,
            events: vec![AdmissionOp::Consolidate, AdmissionOp::WritePage],
            blocking: true,
        }])
        .unwrap();
        let restricted = wiki.clone().with_admission_chain(chain);
        assert!(
            restricted
                .preflight_dream_admission(
                    ws,
                    proj,
                    &path,
                    ActorContext::anonymous(),
                    Arc::new(|| false)
                )
                .await
                .is_err()
        );
    }
    let received = Arc::new(AtomicUsize::new(0));
    let count = received.clone();
    let destination = Router::new().route(
        "/sink",
        post(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        }),
    );
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_addr = sink.local_addr().unwrap();
    let sink_server = tokio::spawn(async move { axum::serve(sink, destination).await.unwrap() });
    let redirect = Arc::new(AtomicBool::new(true));
    let mode = redirect.clone();
    let app = Router::new().route(
        "/admission",
        post(move || {
            let mode = mode.clone();
            async move {
                if mode.load(Ordering::SeqCst) {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [("Location", format!("http://{sink_addr}/sink"))],
                    )
                } else {
                    (StatusCode::NO_CONTENT, [("Location", String::new())])
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let restricted = wiki.with_admission_chain(
        AdmissionChain::new(vec![WebhookConfig {
            name: "synthetic".into(),
            url: format!("http://{addr}/admission"),
            timeout_ms: 100,
            failure_policy: FailurePolicy::Ignore,
            events: vec![AdmissionOp::Consolidate, AdmissionOp::WritePage],
            blocking: true,
        }])
        .unwrap(),
    );
    assert!(
        restricted
            .preflight_dream_admission(
                ws,
                proj,
                &path,
                ActorContext::anonymous(),
                Arc::new(|| false)
            )
            .await
            .is_err()
    );
    let sources = restricted
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    let draft = vec![
        page(ws, proj, "a.md", "reviewed\n"),
        page(ws, proj, "b.md", "stub\n"),
    ];
    assert!(
        restricted
            .apply_dream_batch(draft.clone(), &sources, Arc::new(|| false))
            .await
            .is_err()
    );
    assert_eq!(
        received.load(Ordering::SeqCst),
        0,
        "redirect must not transmit source/draft bodies to a second destination"
    );
    redirect.store(false, Ordering::SeqCst);
    restricted
        .preflight_dream_admission(
            ws,
            proj,
            &path,
            ActorContext::anonymous(),
            Arc::new(|| false),
        )
        .await
        .unwrap();
    restricted
        .apply_dream_batch(draft, &sources, Arc::new(|| false))
        .await
        .unwrap();
    server.abort();
    sink_server.abort();
}

#[tokio::test]
async fn dream_cancel_before_second_admission_hook_prevents_new_request() {
    use ai_memory_wiki::{AdmissionChain, AdmissionOp, FailurePolicy, WebhookConfig};
    use axum::{Router, http::StatusCode, routing::post};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (_tmp, store, wiki, ws, proj) = fixture().await;
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let app = Router::new()
        .route(
            "/cancel",
            post(move || {
                let stop = stop.clone();
                async move {
                    stop.store(true, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        )
        .route(
            "/second",
            post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let hooks = ["cancel", "second"]
        .into_iter()
        .map(|p| WebhookConfig {
            name: p.into(),
            url: format!("http://{addr}/{p}"),
            timeout_ms: 500,
            failure_policy: FailurePolicy::Reject,
            events: vec![AdmissionOp::WritePage],
            blocking: true,
        })
        .collect();
    let wiki = wiki.with_admission_chain(AdmissionChain::new(hooks).unwrap());
    assert!(
        wiki.apply_dream_batch(
            vec![
                page(ws, proj, "a.md", "draft\n"),
                page(ws, proj, "b.md", "stub\n")
            ],
            &sources,
            Arc::new(move || cancelled.load(Ordering::SeqCst))
        )
        .await
        .is_err()
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "cancelled dream must not admit another webhook request"
    );
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "fact A\nexception A\n"
    );
    server.abort();
}
