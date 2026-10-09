//! Shared, serialized driver for explicit and scheduled local DREAM runs.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ai_memory_consolidate::{ActivityClock, DreamCancel, DreamConfig, DreamReport, run_dream_pass};
use ai_memory_core::{AuthLevel, Capability};
use ai_memory_llm::LlmProvider;
use ai_memory_store::{DecayParams, ReaderPool, lookup_existing_scope};
use ai_memory_wiki::Wiki;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

/// An exact scope admitted to local DREAM. Names are never inferred from cwd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamScope {
    /// Existing workspace name.
    pub workspace: String,
    /// Existing project name within that workspace.
    pub project: String,
}

/// Process runtime shared by the admin route and the native scheduler.
pub struct DreamRuntime {
    reader: ReaderPool,
    wiki: Wiki,
    llm: Option<Arc<dyn LlmProvider>>,
    params: DecayParams,
    breadth_weight: f64,
    config: DreamConfig,
    scopes: Vec<DreamScope>,
    activity: ActivityClock,
    gate: tokio::sync::Mutex<()>,
    next_run: AtomicU64,
    stopping: AtomicBool,
    current_cancel: Mutex<Option<DreamCancel>>,
}

/// Terminal result of one manual run or one complete scheduled round.
#[derive(Debug, Serialize)]
pub struct DreamRunReport {
    /// Process-unique run identifier; no source text is emitted.
    pub run_id: String,
    /// Whether inference and writes were forbidden.
    pub dry_run: bool,
    /// Terminal disposition of this run.
    pub terminal: String,
    /// Reason for a run that did not execute.
    pub reason: Option<String>,
    /// Scoped plans and preservation/provenance metadata.
    pub scopes: Vec<DreamScopedReport>,
}

/// Result for one exact allowlisted scope.
#[derive(Debug, Serialize)]
pub struct DreamScopedReport {
    /// Exact names used by the no-create resolver.
    pub scope: DreamScope,
    /// Library plan/result containing IDs and hashes, never bodies.
    pub report: DreamReport,
}

impl DreamRunReport {
    fn include(&mut self, scope: DreamScope, report: DreamReport) {
        if self.terminal == "error" {
            // A later scoped outcome cannot erase a failed round.
        } else if report.cancelled || self.terminal == "cancelled" {
            self.terminal = "cancelled".to_owned();
        } else if report.clusters_selected > 0 {
            self.terminal = if self.dry_run {
                "planned"
            } else if report.clusters_merged > 0 || self.terminal == "applied" {
                "applied"
            } else {
                "skipped"
            }
            .to_owned();
        }
        self.scopes.push(DreamScopedReport { scope, report });
    }
}

impl DreamRuntime {
    fn refused(&self, status: StatusCode, reason: &'static str) -> (StatusCode, &'static str) {
        let now_us = jiff::Timestamp::now().as_microsecond();
        let run_id = format!(
            "dream-{now_us}-{}",
            self.next_run.fetch_add(1, Ordering::Relaxed)
        );
        tracing::info!(%run_id, terminal = "refused", %status, reason, "DREAM run completed");
        (status, reason)
    }
    /// Construct a driver using the running server's reader and wiki writer.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        reader: ReaderPool,
        wiki: Wiki,
        llm: Option<Arc<dyn LlmProvider>>,
        params: DecayParams,
        breadth_weight: f64,
        config: DreamConfig,
        scopes: Vec<DreamScope>,
        activity: ActivityClock,
    ) -> Self {
        Self {
            reader,
            wiki,
            llm,
            params,
            breadth_weight,
            config,
            scopes,
            activity,
            gate: tokio::sync::Mutex::new(()),
            next_run: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            current_cancel: Mutex::new(None),
        }
    }

    /// Stop admitting work and synchronously cancel the current run.
    pub fn begin_shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(cancel) = self
            .current_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
    }

    /// Drain the serialized run through its apply/rollback boundary.
    /// An already admitted backend request may finish before this returns.
    pub async fn shutdown(&self) {
        self.begin_shutdown();
        let _drained = self.gate.lock().await;
    }

    /// Run one explicitly named scope with dry-run selected by default.
    pub async fn manual(
        &self,
        scope: DreamScope,
        apply: bool,
    ) -> Result<DreamRunReport, (StatusCode, &'static str)> {
        if scope.workspace.trim().is_empty() || scope.project.trim().is_empty() {
            return Err(self.refused(
                StatusCode::BAD_REQUEST,
                "workspace and project must be explicit and nonempty",
            ));
        }
        if !self.scopes.contains(&scope) {
            return Err(self.refused(StatusCode::FORBIDDEN, "scope is not in dream.scopes"));
        }
        if apply && (self.llm.is_none() || self.config.semantic_gate.is_none()) {
            return Err(self.refused(
                StatusCode::SERVICE_UNAVAILABLE,
                "local DREAM provider and semantic gate are required for apply",
            ));
        }
        self.run(&[scope], !apply, false).await
    }

    /// Consider a single native scheduler tick across only the exact allowlist.
    pub async fn scheduled(&self) -> Result<DreamRunReport, (StatusCode, &'static str)> {
        self.run(&self.scopes, false, true).await
    }

    async fn run(
        &self,
        scopes: &[DreamScope],
        dry_run: bool,
        scheduled: bool,
    ) -> Result<DreamRunReport, (StatusCode, &'static str)> {
        let _serial = self.gate.try_lock().map_err(|_| {
            self.refused(StatusCode::CONFLICT, "a DREAM run is already in progress")
        })?;
        if self.stopping.load(Ordering::SeqCst) {
            return Err(self.refused(StatusCode::SERVICE_UNAVAILABLE, "DREAM runtime is stopping"));
        }
        let snapshot = self.activity.snapshot();
        let deadline = Instant::now() + ai_memory_consolidate::dream::DREAM_RUN_TIMEOUT;
        let now_us = jiff::Timestamp::now().as_microsecond();
        let mut result = DreamRunReport {
            run_id: format!(
                "dream-{now_us}-{}",
                self.next_run.fetch_add(1, Ordering::Relaxed)
            ),
            dry_run,
            terminal: "noop".to_owned(),
            reason: None,
            scopes: Vec::new(),
        };
        let cancel = DreamCancel::for_activity(self.activity.clone(), snapshot);
        *self
            .current_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(cancel.clone());
        let _running = RunCancellation(&self.current_cancel);
        if self.stopping.load(Ordering::SeqCst) {
            cancel.cancel();
        }
        if snapshot.in_flight > 0
            || (scheduled
                && !ai_memory_consolidate::dream_idle_ready(
                    &self.config,
                    snapshot.last_activity_us,
                    now_us,
                ))
        {
            result.reason = Some("activity gate".to_owned());
            tracing::info!(run_id = %result.run_id, terminal = %result.terminal, dry_run, "DREAM activity gate refused run");
            return Ok(result);
        }
        // Resolve every scope before admitting inference. A stale allowlist
        // cannot perform partial work and cannot create a typoed project.
        let mut resolved = Vec::with_capacity(scopes.len());
        for scope in scopes {
            if cancel.is_cancelled() {
                result.terminal = "cancelled".to_owned();
                tracing::info!(run_id = %result.run_id, terminal = %result.terminal, dry_run, "DREAM run completed");
                return Ok(result);
            }
            let ids = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                lookup_existing_scope(&self.reader, &scope.workspace, &scope.project),
            )
                .await
                .map_err(|_| {
                    tracing::info!(run_id = %result.run_id, terminal = "error", reason = "scope resolution deadline", "DREAM run completed");
                    (StatusCode::REQUEST_TIMEOUT, "DREAM round deadline during scope resolution")
                })?
                .map_err(|_| {
                    tracing::info!(run_id = %result.run_id, terminal = "error", reason = "allowlisted scope does not exist", "DREAM run completed");
                    (StatusCode::NOT_FOUND, "allowlisted scope does not exist")
                })?;
            resolved.push((scope, ids));
        }
        let mut remaining = self.config.effective_max_clusters();
        for (scope, ids) in resolved {
            if remaining == 0 || cancel.is_cancelled() || Instant::now() >= deadline {
                if cancel.is_cancelled() {
                    result.terminal = "cancelled".to_owned();
                } else if Instant::now() >= deadline {
                    result.terminal = "cancelled".to_owned();
                    result.reason = Some("round deadline".to_owned());
                }
                break;
            }
            let mut config = self.config.clone();
            // Manual invocation is independent of the scheduler master switch.
            config.enabled = true;
            config.max_clusters_per_run = remaining;
            config.deadline = Some(deadline);
            match run_dream_pass(
                &self.reader,
                &self.wiki,
                self.llm.as_deref(),
                ids.workspace_id,
                ids.project_id,
                &self.params,
                self.breadth_weight,
                &config,
                &cancel,
                dry_run,
            )
            .await
            {
                Ok(report) => {
                    remaining = remaining.saturating_sub(report.clusters_selected);
                    result.include(scope.clone(), report);
                }
                Err(error) => {
                    result.terminal = "error".to_owned();
                    result.reason = Some(match error {
                        ai_memory_consolidate::DreamError::Limit(reason) => reason.to_owned(),
                        _ => "DREAM storage or eligibility read failed".to_owned(),
                    });
                    break;
                }
            }
        }
        tracing::info!(run_id = %result.run_id, terminal = %result.terminal, dry_run, scopes = result.scopes.len(), "DREAM run completed");
        Ok(result)
    }
}

struct RunCancellation<'a>(&'a Mutex<Option<DreamCancel>>);

impl Drop for RunCancellation<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Root-only request. `apply` is the only way to admit inference and writes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DreamRequest {
    workspace: String,
    project: String,
    #[serde(default)]
    apply: bool,
}

pub(crate) async fn handle_dream(
    level: Option<Extension<AuthLevel>>,
    runtime: Option<Extension<Arc<DreamRuntime>>>,
    Json(request): Json<DreamRequest>,
) -> Response {
    let level = level.map_or(AuthLevel::Anonymous, |v| v.0);
    if let Err(error) = level.authorize(Capability::Admin, true) {
        let status = if error.is_authentication_required() {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        };
        return (status, Json(serde_json::json!({"error": error.message()}))).into_response();
    }
    let Some(Extension(runtime)) = runtime else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "DREAM exact scopes are not configured"})),
        )
            .into_response();
    };
    // Own the admitted operation independently of the HTTP connection. A
    // disconnect or graceful-drain deadline must not drop apply/rollback.
    let run = tokio::spawn(async move {
        runtime
            .manual(
                DreamScope {
                    workspace: request.workspace,
                    project: request.project,
                },
                request.apply,
            )
            .await
    });
    match run.await {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err((status, error))) => {
            (status, Json(serde_json::json!({"error": error}))).into_response()
        }
        Err(_) => {
            tracing::error!(
                terminal = "error",
                reason = "DREAM task interrupted",
                "DREAM driver task failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"DREAM driver task interrupted"})),
            )
                .into_response()
        }
    }
}

/// Hold admitted HTTP work until completion, excluding DREAM's own request.
pub async fn record_http_activity(
    State(clock): State<ActivityClock>,
    mut request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    request.extensions_mut().insert(clock.clone());
    let probe = matches!(
        request.method(),
        &axum::http::Method::GET | &axum::http::Method::HEAD
    ) && request.uri().path().ends_with("/healthz");
    if request.uri().path().ends_with("/admin/dream") || probe {
        return next.run(request).await;
    }
    let _active = clock.begin(jiff::Timestamp::now().as_microsecond());
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_memory_consolidate::EmbeddingCoord;
    use ai_memory_consolidate::dream_semantics::{
        ClusterSuitability, DraftSupport, DreamSemanticError, DreamSemanticGate,
        DreamSemanticSource,
    };
    use ai_memory_core::{ActorContext, PagePath, Tier};
    use ai_memory_llm::{Embedder, SyntheticEmbedder};
    use ai_memory_store::Store;
    use ai_memory_wiki::WritePageRequest;
    use std::future::Future;
    use std::pin::Pin;
    use tower::ServiceExt;

    #[test]
    fn terminal_report_retains_prior_apply_and_cancellation_across_scopes() {
        let scope = DreamScope {
            workspace: "synthetic".into(),
            project: "app".into(),
        };
        let mut result = DreamRunReport {
            run_id: "synthetic".into(),
            dry_run: false,
            terminal: "noop".into(),
            reason: None,
            scopes: Vec::new(),
        };
        result.include(
            scope.clone(),
            DreamReport {
                clusters_selected: 1,
                clusters_merged: 1,
                ..DreamReport::default()
            },
        );
        result.include(
            scope.clone(),
            DreamReport {
                clusters_selected: 1,
                skipped: 1,
                ..DreamReport::default()
            },
        );
        assert_eq!(
            result.terminal, "applied",
            "later refusal cannot erase a completed apply"
        );
        assert_eq!(
            result
                .scopes
                .iter()
                .map(|s| s.report.clusters_merged)
                .sum::<usize>(),
            1
        );
        result.include(
            scope.clone(),
            DreamReport {
                cancelled: true,
                ..DreamReport::default()
            },
        );
        assert_eq!(result.terminal, "cancelled");
        result.include(
            scope,
            DreamReport {
                clusters_selected: 1,
                skipped: 1,
                ..DreamReport::default()
            },
        );
        assert_eq!(
            result.terminal, "cancelled",
            "terminal cancellation retains priority"
        );
    }

    #[tokio::test]
    async fn health_probes_and_own_dream_request_preserve_idle_but_admin_work_cancels() {
        let clock = ActivityClock::default();
        let router = axum::Router::new()
            .route("/healthz", axum::routing::get(|| async { "healthy" }))
            .route("/admin/info", axum::routing::get(|| async { "metadata" }))
            .route("/admin/dream", axum::routing::post(|| async { "report" }))
            .layer(axum::middleware::from_fn_with_state(
                clock.clone(),
                record_http_activity,
            ));
        for (method, path, changed) in [
            ("GET", "/healthz", false),
            ("HEAD", "/healthz", false),
            ("POST", "/admin/dream", false),
            ("GET", "/admin/info", true),
            ("POST", "/healthz", true),
        ] {
            let cancel = DreamCancel::for_activity(clock.clone(), clock.snapshot());
            router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(cancel.is_cancelled(), changed, "{method} {path}");
            assert_eq!(clock.snapshot().in_flight, 0);
        }
    }

    #[derive(Debug)]
    struct PendingClusterGate {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl DreamSemanticGate for PendingClusterGate {
        fn cluster_suitability<'a>(
            &'a self,
            _sources: &'a [DreamSemanticSource],
        ) -> Pin<Box<dyn Future<Output = Result<ClusterSuitability, DreamSemanticError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(ClusterSuitability::Approved)
            })
        }

        fn draft_support<'a>(
            &'a self,
            _sources: &'a [DreamSemanticSource],
            _draft: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<DraftSupport, DreamSemanticError>> + Send + 'a>>
        {
            Box::pin(async { panic!("shutdown must prevent the second semantic request") })
        }
    }

    async fn fixture() -> (tempfile::TempDir, Store, Wiki, Vec<DreamScope>) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let wiki = Wiki::new(tmp.path(), store.writer.clone())
            .unwrap()
            .with_store_reader(store.reader.clone())
            .with_embedder(Arc::new(SyntheticEmbedder::new(64)) as Arc<dyn Embedder>);
        let scopes = vec![
            DreamScope {
                workspace: "allowed".into(),
                project: "app".into(),
            },
            DreamScope {
                workspace: "foreign".into(),
                project: "app".into(),
            },
        ];
        for scope in &scopes {
            let ws = store
                .writer
                .get_or_create_workspace(&scope.workspace)
                .await
                .unwrap();
            store
                .writer
                .get_or_create_project(ws, &scope.project, None)
                .await
                .unwrap();
        }
        (tmp, store, wiki, scopes)
    }

    fn runtime(
        store: &Store,
        wiki: &Wiki,
        scopes: Vec<DreamScope>,
        cap: usize,
        activity: ActivityClock,
    ) -> DreamRuntime {
        DreamRuntime::new(
            store.reader.clone(),
            wiki.clone(),
            None,
            DecayParams::default(),
            0.0,
            DreamConfig {
                enabled: true,
                min_cold_pages: 3,
                max_clusters_per_run: cap,
                embedding: Some(EmbeddingCoord {
                    provider: "synthetic".into(),
                    model: "bag-of-words-v1".into(),
                    dim: 64,
                }),
                ..DreamConfig::default()
            },
            scopes,
            activity,
        )
    }

    #[tokio::test]
    async fn exact_allowlist_refuses_foreign_scope_and_missing_scope_never_creates() {
        let (_tmp, store, wiki, scopes) = fixture().await;
        let missing = DreamScope {
            workspace: "allowed".into(),
            project: "missing".into(),
        };
        let driver = runtime(
            &store,
            &wiki,
            vec![scopes[0].clone(), missing.clone()],
            1,
            ActivityClock::default(),
        );
        assert_eq!(
            driver.manual(scopes[1].clone(), false).await.unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            driver.manual(missing, false).await.unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        let ws = store
            .reader
            .find_workspace("allowed".into())
            .await
            .unwrap()
            .unwrap();
        assert!(
            store
                .reader
                .find_project(ws, "missing".into())
                .await
                .unwrap()
                .is_none()
        );
        let control = driver.manual(scopes[0].clone(), false).await.unwrap();
        assert!(control.dry_run);
        assert_eq!(control.scopes.len(), 1);
        assert_eq!(control.scopes[0].report.inference_requests, 0);
    }

    #[tokio::test]
    async fn driver_refuses_overlap_and_live_work_then_accepts_idle_control() {
        let (_tmp, store, wiki, scopes) = fixture().await;
        let activity = ActivityClock::default();
        let driver = runtime(&store, &wiki, vec![scopes[0].clone()], 1, activity.clone());
        let serial = driver.gate.lock().await;
        assert_eq!(
            driver.manual(scopes[0].clone(), false).await.unwrap_err().0,
            StatusCode::CONFLICT
        );
        drop(serial);
        let live = activity.begin(0);
        let refused = driver.manual(scopes[0].clone(), false).await.unwrap();
        assert!(refused.scopes.is_empty());
        assert_eq!(refused.reason.as_deref(), Some("activity gate"));
        drop(live);
        assert_eq!(
            driver
                .manual(scopes[0].clone(), false)
                .await
                .unwrap()
                .scopes
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn cluster_cap_is_global_across_the_allowlisted_round() {
        let (tmp, store, wiki, scopes) = fixture().await;
        for scope in &scopes {
            let ids = lookup_existing_scope(&store.reader, &scope.workspace, &scope.project)
                .await
                .unwrap();
            for (path, body) in [
                (
                    "a.md",
                    "Investigated the flaky retention sweep test failure again and again during the long debugging session that afternoon. The unique culprit turned out to be in the file crates/ai-memory-store/src/ops.rs during the run.",
                ),
                (
                    "b.md",
                    "Investigated the flaky retention sweep test failure again and again during the long debugging session that afternoon. The unique culprit turned out to be in the file crates/ai-memory-store/src/reader.rs during the run.",
                ),
                (
                    "outlier.md",
                    "Watercolor painting pigments paper brushes gallery landscapes portrait composition and ocean sunset.",
                ),
            ] {
                wiki.write_page(WritePageRequest {
                    workspace_id: ids.workspace_id,
                    project_id: ids.project_id,
                    path: PagePath::new(path).unwrap(),
                    frontmatter: serde_json::json!({"title":path}),
                    body: body.into(),
                    tier: Tier::Episodic,
                    pinned: false,
                    title: Some(path.into()),
                    admission_ctx: None,
                    author_id: None,
                    actor: ActorContext::anonymous(),
                    evidence: Vec::new(),
                })
                .await
                .unwrap();
            }
        }
        let conn = rusqlite::Connection::open(tmp.path().join("db/memory.sqlite")).unwrap();
        let old = jiff::Timestamp::now().as_microsecond() - 400 * 86_400_000_000;
        conn.execute("UPDATE pages SET created_at=?1, updated_at=?1", [old])
            .unwrap();
        let driver = runtime(&store, &wiki, scopes.clone(), 1, ActivityClock::default());
        let bounded = driver.run(&scopes, true, false).await.unwrap();
        assert_eq!(
            bounded
                .scopes
                .iter()
                .map(|s| s.report.clusters_selected)
                .sum::<usize>(),
            1
        );
        assert_eq!(bounded.scopes.len(), 1);
        let control = runtime(&store, &wiki, scopes.clone(), 2, ActivityClock::default())
            .run(&scopes, true, false)
            .await
            .unwrap();
        assert_eq!(
            control
                .scopes
                .iter()
                .map(|s| s.report.clusters_selected)
                .sum::<usize>(),
            2
        );
        assert_eq!(control.scopes.len(), 2);
    }

    #[tokio::test]
    async fn shutdown_cancels_admitted_inference_drains_and_refuses_new_runs() {
        for disconnect in [false, true] {
            let (tmp, store, wiki, scopes) = fixture().await;
            let scope = scopes[0].clone();
            let ids = lookup_existing_scope(&store.reader, &scope.workspace, &scope.project)
                .await
                .unwrap();
            for (path, body) in [
                (
                    "a.md",
                    "Investigated the flaky retention sweep test failure again and again during the long debugging session that afternoon. The unique culprit turned out to be in the file crates/ai-memory-store/src/ops.rs during the run.",
                ),
                (
                    "b.md",
                    "Investigated the flaky retention sweep test failure again and again during the long debugging session that afternoon. The unique culprit turned out to be in the file crates/ai-memory-store/src/reader.rs during the run.",
                ),
                (
                    "outlier.md",
                    "Watercolor painting pigments paper brushes gallery landscapes portrait composition and ocean sunset.",
                ),
            ] {
                wiki.write_page(WritePageRequest {
                    workspace_id: ids.workspace_id,
                    project_id: ids.project_id,
                    path: PagePath::new(path).unwrap(),
                    frontmatter: serde_json::json!({"title":path}),
                    body: body.into(),
                    tier: Tier::Episodic,
                    pinned: false,
                    title: Some(path.into()),
                    admission_ctx: None,
                    author_id: None,
                    actor: ActorContext::anonymous(),
                    evidence: Vec::new(),
                })
                .await
                .unwrap();
            }
            let conn = rusqlite::Connection::open(tmp.path().join("db/memory.sqlite")).unwrap();
            let old = jiff::Timestamp::now().as_microsecond() - 400 * 86_400_000_000;
            conn.execute("UPDATE pages SET created_at=?1, updated_at=?1", [old])
                .unwrap();
            let before: i64 = conn
                .query_row("SELECT count(*) FROM pages", [], |row| row.get(0))
                .unwrap();
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let llm = Arc::new(
                ai_memory_llm::DreamLocalProvider::new(
                    "http://127.0.0.1:49777/v1",
                    "synthetic-never-called",
                )
                .unwrap(),
            );
            let mut driver = runtime(
                &store,
                &wiki,
                vec![scope.clone()],
                1,
                ActivityClock::default(),
            );
            driver.llm = Some(llm.clone());
            driver.config.semantic_gate = Some(Arc::new(PendingClusterGate {
                entered: entered.clone(),
                release: release.clone(),
            }));
            let driver = Arc::new(driver);
            let running_driver = driver.clone();
            let request = DreamRequest {
                workspace: scope.workspace.clone(),
                project: scope.project.clone(),
                apply: true,
            };
            let running = tokio::spawn(async move {
                handle_dream(
                    Some(Extension(AuthLevel::Root)),
                    Some(Extension(running_driver)),
                    Json(request),
                )
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            if disconnect {
                running.abort();
                tokio::task::yield_now().await;
            }
            driver.begin_shutdown();
            let draining_driver = driver.clone();
            let mut draining = tokio::spawn(async move { draining_driver.shutdown().await });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut draining)
                    .await
                    .is_err(),
                "shutdown must await the admitted request and cleanup"
            );
            release.notify_one();
            if !disconnect {
                let response = tokio::time::timeout(std::time::Duration::from_secs(5), running)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap();
                let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(report["terminal"], "cancelled");
                assert_eq!(report["scopes"][0]["report"]["inference_requests"], 1);
                assert_eq!(report["scopes"][0]["report"]["clusters_merged"], 0);
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), draining)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                llm.request_count(),
                Some(0),
                "no drafting request may start after cancellation"
            );
            let after: i64 = conn
                .query_row("SELECT count(*) FROM pages", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                after, before,
                "shutdown cancellation cannot create a new head/version"
            );
            assert_eq!(
                driver.manual(scope, false).await.unwrap_err().0,
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
    }
}
