//! Adversarial metadata-only writes through admission, CAS and recovery.
use super::*;
use ai_memory_core::{PageEvidence, PageEvidenceKind, SanitizeConfig};
use ai_memory_store::Store;
use axum::{Json, Router, http::StatusCode, routing::post};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Store,
    wiki: Wiki,
    ws: WorkspaceId,
    proj: ProjectId,
    path: PagePath,
    id: PageId,
    hash: [u8; 32],
}

struct RefusingEmbedder(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ai_memory_llm::Embedder for RefusingEmbedder {
    fn provider(&self) -> &'static str {
        "synthetic"
    }
    fn model(&self) -> &str {
        "synthetic-model"
    }
    fn dim(&self) -> u32 {
        2
    }
    async fn embed(&self, _text: &str) -> ai_memory_llm::LlmResult<Vec<f32>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ai_memory_llm::LlmError::NotConfigured(
            "synthetic refusal".into(),
        ))
    }
}

#[tokio::test]
async fn curation_of_unchanged_content_never_calls_the_embedding_provider() {
    let mut f = fixture("Synthetic unchanged body.\r\n").await;
    let calls = Arc::new(AtomicUsize::new(0));
    f.wiki = f
        .wiki
        .with_embedder(Arc::new(RefusingEmbedder(calls.clone())));
    let receipt = f.wiki.curate_relations(f.request(false)).await.unwrap();
    assert!(receipt.applied);
    assert!(receipt.checkpoint.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

impl Fixture {
    fn request(&self, dry_run: bool) -> RelationCurationRequest {
        RelationCurationRequest {
            workspace_id: self.ws,
            project_id: self.proj,
            path: self.path.clone(),
            expected_head_id: self.id,
            expected_body_sha256: self.hash,
            dry_run,
            actor: ActorContext::anonymous(),
        }
    }
}

fn write_req(ws: WorkspaceId, proj: ProjectId, path: PagePath, body: &str) -> WritePageRequest {
    WritePageRequest {
        workspace_id: ws,
        project_id: proj,
        path,
        frontmatter: serde_json::json!({"relations":{"contradicts":["None","Free title","none.md"],"fixes":["N/A"],"references":["Nenhuma"]},
            "tier":"procedural","pinned":true,"expires_at":"2099-01-01","tags":["preserve"],"custom":{"nested":[1,true,null]},"origin":{"session_id":"synthetic"}}),
        body: body.into(),
        tier: Tier::Procedural,
        pinned: true,
        title: Some("Synthetic page".into()),
        admission_ctx: None,
        author_id: None,
        actor: ActorContext::anonymous(),
        evidence: vec![PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: "synthetic".into(),
        }],
    }
}

async fn fixture(body: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("synthetic")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "curation", None)
        .await
        .unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone())
        .unwrap()
        .with_store_reader(store.reader.clone());
    let path = PagePath::new("notes/curate.md").unwrap();
    let id = wiki
        .write_page(write_req(ws, proj, path.clone(), body))
        .await
        .unwrap();
    let hash = Sha256::digest(body.as_bytes()).into();
    Fixture {
        _tmp: tmp,
        store,
        wiki,
        ws,
        proj,
        path,
        id,
        hash,
    }
}

async fn webhook(app: Router) -> (AdmissionChain, tokio::task::JoinHandle<()>) {
    use crate::admission::{FailurePolicy, WebhookConfig};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let chain = AdmissionChain::new(vec![WebhookConfig {
        name: "synthetic-guard".into(),
        url: format!("http://{address}/hook"),
        timeout_ms: 500,
        failure_policy: FailurePolicy::Reject,
        events: vec![AdmissionOp::WritePage],
        blocking: true,
    }])
    .unwrap();
    (chain, task)
}

#[tokio::test]
async fn curation_dryrun_and_stale_checks_never_recreate_missing_manifests() {
    let f = fixture("Synthetic facts.\r\n").await;
    let manifests = [
        f.wiki.root.join(f.ws.to_string()).join("_meta.md"),
        f.wiki.project_root(f.ws, f.proj).join("_meta.md"),
    ];
    for path in &manifests {
        f.wiki.git.remove_file(path).unwrap();
    }
    let reopened = Wiki::new(f._tmp.path(), f.store.writer.clone())
        .unwrap()
        .with_store_reader(f.store.reader.clone());
    let bytes = std::fs::read(reopened.abs_path(f.ws, f.proj, &f.path)).unwrap();
    let preview = reopened.curate_relations(f.request(true)).await.unwrap();
    assert!(!preview.applied);
    assert_eq!(preview.removed, 4);
    let mut stale = f.request(false);
    stale.expected_head_id = PageId::new();
    assert!(matches!(
        reopened.curate_relations(stale).await,
        Err(WikiError::StalePageHead)
    ));
    assert!(manifests.iter().all(|path| !path.exists()));
    assert_eq!(
        std::fs::read(reopened.abs_path(f.ws, f.proj, &f.path)).unwrap(),
        bytes
    );
}

#[tokio::test]
async fn curation_before_and_after_checkpoints_contain_the_exact_recoverable_documents() {
    let f = fixture("# Synthetic\r\n\r\nEvery fact and whitespace remains.\r\n").await;
    let abs = f.wiki.abs_path(f.ws, f.proj, &f.path);
    let before = std::fs::read(&abs).unwrap();
    let before_md = f.wiki.read_page(f.ws, f.proj, &f.path).unwrap();
    let receipt = f.wiki.curate_relations(f.request(false)).await.unwrap();
    assert!(receipt.applied);
    let rel = PathBuf::from(f.ws.to_string())
        .join(f.proj.to_string())
        .join(f.path.as_str());
    let before_rev = receipt.before_checkpoint.unwrap();
    assert_eq!(f.wiki.git.file_at_rev(&before_rev, &rel).unwrap(), before);
    assert_eq!(
        f.wiki
            .git
            .file_at_rev(&receipt.checkpoint.unwrap(), &rel)
            .unwrap(),
        std::fs::read(&abs).unwrap()
    );
    let mut expected = before_md.clone();
    expected.frontmatter["relations"]["contradicts"] = serde_json::json!(["Free title"]);
    expected.frontmatter["relations"]["fixes"] = serde_json::json!([]);
    expected.frontmatter["relations"]["references"] = serde_json::json!([]);
    assert_eq!(f.wiki.read_page(f.ws, f.proj, &f.path).unwrap(), expected);
    f.wiki
        .restore_page_from_checkpoint(f.ws, f.proj, f.path.clone(), &before_rev)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&abs).unwrap(), before);
    assert_eq!(f.wiki.read_page(f.ws, f.proj, &f.path).unwrap(), before_md);
}

#[tokio::test]
async fn curation_protected_body_and_frontmatter_reject_admission_mutations_without_writing() {
    for field in ["body", "frontmatter"] {
        let f = fixture("Synthetic facts.\r\n").await;
        let before = std::fs::read(f.wiki.abs_path(f.ws, f.proj, &f.path)).unwrap();
        let app = Router::new().route(
            "/hook",
            post(move |Json(payload): Json<serde_json::Value>| async move {
                let page = if field == "body" {
                    serde_json::json!({"body":"Unauthorized rewrite."})
                } else {
                    let mut fm = payload["page"]["frontmatter"].clone();
                    fm["custom"]["nested"] = serde_json::json!(["replacement"]);
                    serde_json::json!({"frontmatter":fm})
                };
                Json(serde_json::json!({"page":page}))
            }),
        );
        let (chain, task) = webhook(app).await;
        let wiki = f.wiki.clone().with_admission_chain(chain);
        for dry_run in [true, false] {
            assert!(matches!(
                wiki.curate_relations(f.request(dry_run)).await,
                Err(WikiError::ProtectedPageContent)
            ));
            assert_eq!(
                std::fs::read(wiki.abs_path(f.ws, f.proj, &f.path)).unwrap(),
                before
            );
            assert_eq!(
                f.store
                    .reader
                    .latest_page_id_by_ids(f.ws, f.proj, f.path.to_string())
                    .await
                    .unwrap(),
                Some(f.id)
            );
        }
        task.abort();
    }
}

#[tokio::test]
async fn curation_scrub_refusal_happens_before_sending_protected_body_to_admission() {
    let f = fixture("Synthetic PRIVACY_FIXTURE_ONLY marker.\r\n").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let app = Router::new().route(
        "/hook",
        post(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        }),
    );
    let (chain, task) = webhook(app).await;
    let sanitizer = Sanitizer::new(&SanitizeConfig {
        extra_patterns: vec!["PRIVACY_FIXTURE_ONLY".into()],
        ..Default::default()
    })
    .unwrap();
    let wiki = f
        .wiki
        .clone()
        .with_admission_chain(chain)
        .with_sanitizer(sanitizer);
    assert!(matches!(
        wiki.curate_relations(f.request(false)).await,
        Err(WikiError::ProtectedPageContent)
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "protected content must never cross the callback boundary"
    );
    assert_eq!(
        f.store
            .reader
            .latest_page_id_by_ids(f.ws, f.proj, f.path.to_string())
            .await
            .unwrap(),
        Some(f.id)
    );
    task.abort();
}

#[tokio::test]
async fn curation_admission_can_reenter_the_common_write_path_without_holding_its_lock() {
    let f = fixture("Synthetic facts.\r\n").await;
    let callback_wiki = f.wiki.clone();
    let ws = f.ws;
    let proj = f.proj;
    let app = Router::new().route(
        "/hook",
        post(move || {
            let wiki = callback_wiki.clone();
            async move {
                wiki.write_page(write_req(
                    ws,
                    proj,
                    PagePath::new("notes/callback.md").unwrap(),
                    "Independent callback facts.",
                ))
                .await
                .unwrap();
                StatusCode::NO_CONTENT
            }
        }),
    );
    let (chain, task) = webhook(app).await;
    let wiki = f.wiki.clone().with_admission_chain(chain);
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(900),
        wiki.curate_relations(f.request(false)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.applied);
    assert!(
        f.store
            .reader
            .latest_page_id_by_ids(f.ws, f.proj, "notes/callback.md".into())
            .await
            .unwrap()
            .is_some()
    );
    task.abort();
}

#[tokio::test]
async fn curation_rechecks_a_head_changed_during_admission_without_overwriting_it() {
    let f = fixture("Original synthetic facts.\r\n").await;
    let callback_wiki = f.wiki.clone();
    let ws = f.ws;
    let proj = f.proj;
    let path = f.path.clone();
    let app = Router::new().route(
        "/hook",
        post(move || {
            let wiki = callback_wiki.clone();
            let path = path.clone();
            async move {
                wiki.write_page(write_req(ws, proj, path, "Concurrent synthetic facts.\r\n"))
                    .await
                    .unwrap();
                StatusCode::NO_CONTENT
            }
        }),
    );
    let (chain, task) = webhook(app).await;
    let wiki = f.wiki.clone().with_admission_chain(chain);
    assert!(matches!(
        wiki.curate_relations(f.request(false)).await,
        Err(WikiError::StalePageHead)
    ));
    let stored = f
        .store
        .reader
        .page_body_by_ids(f.ws, f.proj, f.path.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.body, "Concurrent synthetic facts.\r\n");
    assert_eq!(
        wiki.read_page(f.ws, f.proj, &f.path).unwrap().body,
        stored.body
    );
    assert_ne!(
        f.store
            .reader
            .latest_page_id_by_ids(f.ws, f.proj, f.path.to_string())
            .await
            .unwrap(),
        Some(f.id)
    );
    task.abort();
}

#[tokio::test]
async fn conditional_persistence_rolls_back_installed_metadata_when_writer_cas_refuses_it() {
    let f = fixture("Original synthetic facts.\r\n").await;
    let latest = f
        .wiki
        .write_page(write_req(
            f.ws,
            f.proj,
            f.path.clone(),
            "Concurrent synthetic facts.\r\n",
        ))
        .await
        .unwrap();
    let abs = f.wiki.abs_path(f.ws, f.proj, &f.path);
    let before = std::fs::read(&abs).unwrap();
    let mut md = f.wiki.read_page(f.ws, f.proj, &f.path).unwrap();
    md.frontmatter["relations"]["contradicts"] = serde_json::json!(["Free title"]);
    let emitted = emit(&md).unwrap();
    let _guard = f.wiki.mutation_lock.write().await;
    let result = f
        .wiki
        .persist_page_locked(
            NewPage {
                workspace_id: f.ws,
                project_id: f.proj,
                path: f.path.clone(),
                title: "Synthetic page".into(),
                body: md.body,
                frontmatter_json: md.frontmatter,
                tier: Tier::Procedural,
                pinned: true,
                links: Vec::new(),
                author_id: None,
                expires_at: None,
                entities: Vec::new(),
                evidence: Vec::new(),
            },
            emitted.as_bytes(),
            Some((f.id, f.hash)),
        )
        .await;
    assert!(matches!(result, Err(WikiError::StalePageHead)));
    assert_eq!(
        std::fs::read(&abs).unwrap(),
        before,
        "failed CAS must restore the installed file's exact prior bytes"
    );
    assert_eq!(
        f.store
            .reader
            .latest_page_id_by_ids(f.ws, f.proj, f.path.to_string())
            .await
            .unwrap(),
        Some(latest)
    );
}
