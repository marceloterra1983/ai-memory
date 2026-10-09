//! Operational admin compatibility must not bypass the live operator boundary.
//!
//! These requests use the same admin router and dual-auth middleware as HTTP
//! serving. No request is given an AuthLevel extension by the fixture.

use std::sync::Arc;

use ai_memory_consolidate::{ActivityClock, DreamConfig, EmbeddingCoord};
use ai_memory_core::{ActorContext, ApiCredentialId, NewUser, PagePath, UserRole};
use ai_memory_mcp::auth::AuthState;
use ai_memory_mcp::dream::{DreamRuntime, DreamScope};
use ai_memory_mcp::human_auth::LoginLimiter;
use ai_memory_mcp::{AdminState, HumanAuthRuntime, admin_router, require_dual_auth};
use ai_memory_store::{ReaderPool, TokenPepper, hash_token, lookup_existing_scope};
use ai_memory_wiki::Markdown;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const ROOT_BEARER: &str = "synthetic-operational-root";
const USER_BEARER: &str = "synthetic-operational-user";
const PROXY_BEARER: &str = "synthetic-operational-proxy";
const UNKNOWN_BEARER: &str = "synthetic-operational-unknown";

fn pepper() -> TokenPepper {
    TokenPepper::new("synthetic-operational-pepper")
}

fn auth(state: &AdminState, root: Option<&str>) -> AuthState {
    let auth = AuthState::new(root.map(str::to_owned))
        .with_multiuser(pepper(), state.reader.clone(), state.writer.clone())
        .with_human_runtime(
            HumanAuthRuntime {
                reader: state.reader.clone(),
                writer: state.writer.clone(),
                recovery_token_hash: None,
                root_username: "root".into(),
                root_name: None,
                root_email: None,
                reserved_passwords: Vec::new(),
                trusted_proxy_cidrs: Vec::new(),
                limiter: Arc::new(LoginLimiter::default()),
            },
            false,
        );
    if state.trusted_proxy_identity {
        auth.with_trusted_proxy_bearer(PROXY_BEARER)
            .with_root_actor(ActorContext {
                user: Some("root".into()),
                issuer: Some("https://synthetic.example".into()),
                sub: Some("synthetic-root-subject".into()),
                ..ActorContext::default()
            })
    } else {
        auth
    }
}

fn router(state: AdminState, auth: AuthState) -> Router {
    router_with_scopes(
        state,
        auth,
        vec![DreamScope {
            workspace: "default".into(),
            project: "dev".into(),
        }],
    )
}

fn router_with_scopes(state: AdminState, auth: AuthState, scopes: Vec<DreamScope>) -> Router {
    let runtime = Arc::new(DreamRuntime::new(
        state.reader.clone(),
        state.wiki.clone(),
        None,
        state.decay_params,
        0.0,
        DreamConfig {
            enabled: true,
            embedding: Some(EmbeddingCoord {
                provider: "synthetic".into(),
                model: "bag-of-words-v1".into(),
                dim: 64,
            }),
            ..DreamConfig::default()
        },
        scopes,
        ActivityClock::default(),
    ));
    admin_router(state)
        .layer(axum::Extension(runtime))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(auth),
            require_dual_auth,
        ))
}

fn requests(curation: &Value, apply: bool) -> [(&'static str, Value); 2] {
    let mut curation = curation.clone();
    curation["dry_run"] = json!(!apply);
    [
        (
            "/admin/dream",
            json!({"workspace":"default","project":"dev","apply":apply}),
        ),
        ("/admin/curate-relations", curation),
    ]
}

async fn post(
    router: &Router,
    uri: &str,
    payload: Value,
    bearer: Option<&str>,
    actor_headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    for &(name, value) in actor_headers {
        request = request.header(name, value);
    }
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[derive(Debug, PartialEq)]
struct PageSnapshot {
    versions: i64,
    document: Markdown,
}

async fn snapshot(state: &AdminState) -> PageSnapshot {
    let scope = lookup_existing_scope(&state.reader, "default", "dev")
        .await
        .unwrap();
    let document = state
        .wiki
        .read_page(
            scope.workspace_id,
            scope.project_id,
            &PagePath::new("notes/curate.md").unwrap(),
        )
        .unwrap();
    let versions = state
        .reader
        .with_conn(|conn| {
            conn.query_row("SELECT count(*) FROM pages", [], |row| row.get(0))
                .map_err(Into::into)
        })
        .await
        .unwrap();
    PageSnapshot { versions, document }
}

async fn assert_dry_controls(router: &Router, payload: &Value, bearer: Option<&str>) {
    for (uri, request) in requests(payload, false) {
        let (status, result) = post(router, uri, request, bearer, &[]).await;
        assert_eq!(status, StatusCode::OK, "dry-run control: {uri}");
        if uri == "/admin/dream" {
            assert_eq!(result["dry_run"], true);
            assert_eq!(result["scopes"].as_array().unwrap().len(), 1);
            assert_eq!(result["scopes"][0]["report"]["inference_requests"], 0);
        } else {
            assert_eq!(result["applied"], false);
            assert_eq!(result["removed"], 3);
        }
    }
}

async fn assert_refused(
    router: &Router,
    payload: &Value,
    bearer: Option<&str>,
    actor_headers: &[(&str, &str)],
    expected: StatusCode,
) {
    for (uri, request) in requests(payload, true) {
        let (status, result) = post(router, uri, request, bearer, actor_headers).await;
        assert_eq!(status, expected, "refused apply: {uri}");
        assert!(result["scopes"].is_null(), "DREAM handler ran: {uri}");
        assert!(result["applied"].is_null(), "curation handler ran: {uri}");
    }
}

// Caching the users-exist decision or dropping the shared root gate would
// admit one of the refused applies after this same router has served bootstrap.
#[tokio::test]
async fn first_user_immediately_closes_bootstrap_for_both_operational_commands() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = super::admin_relation_curation::fixture(&state, "dev").await;
    assert!(!state.reader.users_exist().await.unwrap());
    let bootstrap = router(state.clone(), auth(&state, None));
    let before = snapshot(&state).await;
    assert_dry_controls(&bootstrap, &payload, None).await;
    assert_eq!(snapshot(&state).await, before);

    // Configuring a root bearer closes the wire gate even before any user
    // exists. The no-authority router remains separate and unchanged.
    let secured = router(state.clone(), auth(&state, Some(ROOT_BEARER)));
    assert_refused(&secured, &payload, None, &[], StatusCode::UNAUTHORIZED).await;
    assert_refused(
        &secured,
        &payload,
        Some(UNKNOWN_BEARER),
        &[],
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_dry_controls(&secured, &payload, Some(ROOT_BEARER)).await;
    assert_eq!(snapshot(&state).await, before);

    let user_id = state
        .writer
        .create_human_user(
            NewUser {
                username: "api-root".into(),
                name: None,
                email: None,
            },
            UserRole::Root,
            None,
            false,
        )
        .await
        .unwrap();
    assert_refused(&bootstrap, &payload, None, &[], StatusCode::UNAUTHORIZED).await;

    state
        .writer
        .create_api_credential(
            ApiCredentialId::new(),
            user_id,
            "synthetic-native-key".into(),
            hash_token(USER_BEARER, &pepper()),
            None,
        )
        .await
        .unwrap();
    assert_refused(
        &bootstrap,
        &payload,
        Some(USER_BEARER),
        &[("x-memory-actor-user", "root")],
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_refused(&bootstrap, &payload, None, &[], StatusCode::UNAUTHORIZED).await;
    assert_refused(
        &bootstrap,
        &payload,
        Some(UNKNOWN_BEARER),
        &[("x-memory-actor-user", "root")],
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(snapshot(&state).await, before);

    // Reuse the root-authority router created before the first user as well.
    assert_dry_controls(&secured, &payload, Some(ROOT_BEARER)).await;
    assert_refused(
        &secured,
        &payload,
        Some(USER_BEARER),
        &[("x-memory-actor-user", "root")],
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_refused(
        &secured,
        &payload,
        Some(UNKNOWN_BEARER),
        &[],
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(snapshot(&state).await, before);
}

// Consulting only users_exist would let an authenticated proxy User apply
// curation while this deployment's users table remains empty.
#[tokio::test]
async fn trusted_proxy_without_database_users_is_root_only_for_both_operations() {
    let tmp = TempDir::new().unwrap();
    let mut state = super::admin_write_page::make_state(&tmp).await;
    state.trusted_proxy_identity = true;
    let payload = super::admin_relation_curation::fixture(&state, "dev").await;
    assert!(!state.reader.users_exist().await.unwrap());
    let router = router(state.clone(), auth(&state, Some(ROOT_BEARER)));
    let before = snapshot(&state).await;

    for (bearer, headers, expected) in [
        (
            None,
            vec![("x-memory-actor-user", "root")],
            StatusCode::UNAUTHORIZED,
        ),
        (
            Some(PROXY_BEARER),
            vec![("x-memory-actor-user", "alice")],
            StatusCode::FORBIDDEN,
        ),
        (
            Some(PROXY_BEARER),
            vec![("x-memory-actor-user", "root")],
            StatusCode::FORBIDDEN,
        ),
        (
            Some(UNKNOWN_BEARER),
            vec![
                ("x-memory-actor-issuer", "https://synthetic.example"),
                ("x-memory-actor-sub", "synthetic-root-subject"),
            ],
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        assert_refused(&router, &payload, bearer, &headers, expected).await;
        assert_eq!(snapshot(&state).await, before);
    }
    assert_dry_controls(&router, &payload, Some(ROOT_BEARER)).await;
    assert!(!state.reader.users_exist().await.unwrap());
    assert_eq!(snapshot(&state).await, before);

    // With no static root bearer, Anonymous reaches the live proxy gate.
    // Only the authenticating proxy can assert the configured root subject.
    let proxy_only = router_with_scopes(
        state.clone(),
        auth(&state, None),
        vec![DreamScope {
            workspace: "default".into(),
            project: "dev".into(),
        }],
    );
    assert_refused(
        &proxy_only,
        &payload,
        None,
        &[("x-memory-actor-user", "root")],
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_refused(
        &proxy_only,
        &payload,
        Some(PROXY_BEARER),
        &[("x-memory-actor-user", "root")],
        StatusCode::FORBIDDEN,
    )
    .await;
    let identity = [
        ("x-memory-actor-issuer", "https://synthetic.example"),
        ("x-memory-actor-sub", "synthetic-root-subject"),
    ];
    assert_refused(
        &proxy_only,
        &payload,
        Some(UNKNOWN_BEARER),
        &identity,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    for (uri, request) in requests(&payload, false) {
        let (status, _) = post(&proxy_only, uri, request, Some(PROXY_BEARER), &identity).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "authenticated proxy root control: {uri}"
        );
    }
    assert!(!state.reader.users_exist().await.unwrap());
    assert_eq!(snapshot(&state).await, before);
}

// An unreadable user configuration cannot be mistaken for an empty users table,
// including when the wire credential has already authenticated as Root.
#[tokio::test]
async fn operational_gate_database_failure_refuses_before_both_handlers() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = super::admin_relation_curation::fixture(&state, "dev").await;
    let before = snapshot(&state).await;
    let control = router(state.clone(), auth(&state, None));
    assert_dry_controls(&control, &payload, None).await;

    let mut unavailable = state.clone();
    unavailable.reader = ReaderPool::new(&tmp.path().join("missing-gate.sqlite"), 1).unwrap();
    for root in [None, Some(ROOT_BEARER)] {
        let router = router(unavailable.clone(), auth(&state, root));
        for (uri, request) in requests(&payload, true) {
            let (status, result) = post(&router, uri, request, root, &[]).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains("could not verify user configuration"),
                "must refuse at the operator gate: {uri}"
            );
            assert!(result["scopes"].is_null());
            assert!(result["applied"].is_null());
        }
    }
    assert_eq!(snapshot(&state).await, before);
}

// A failed API-key lookup must not acquire bootstrap or root authority. The
// configured root control skips that lookup and still reaches both handlers.
#[tokio::test]
async fn operational_credential_lookup_failure_refuses_before_both_handlers() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = super::admin_relation_curation::fixture(&state, "dev").await;
    let missing_reader =
        ReaderPool::new(&tmp.path().join("missing-credentials.sqlite"), 1).unwrap();
    let unavailable_auth = auth(&state, Some(ROOT_BEARER)).with_multiuser(
        pepper(),
        missing_reader,
        state.writer.clone(),
    );
    let router = router(state.clone(), unavailable_auth);
    let before = snapshot(&state).await;
    assert_refused(
        &router,
        &payload,
        Some(UNKNOWN_BEARER),
        &[("x-memory-actor-user", "root")],
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_dry_controls(&router, &payload, Some(ROOT_BEARER)).await;
    assert_eq!(snapshot(&state).await, before);
}

// Removing exact admission or replacing the no-create resolver with a creating
// lookup would admit the foreign scope or materialize one of the missing names.
#[tokio::test]
async fn dream_bootstrap_preserves_exact_allowlist_no_create_and_explicit_scope() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = super::admin_relation_curation::fixture(&state, "dev").await;
    let foreign = state
        .writer
        .get_or_create_workspace("foreign-workspace")
        .await
        .unwrap();
    state
        .writer
        .get_or_create_project(foreign, "dev", None)
        .await
        .unwrap();
    let router = router_with_scopes(
        state.clone(),
        auth(&state, None),
        vec![
            DreamScope {
                workspace: "default".into(),
                project: "dev".into(),
            },
            DreamScope {
                workspace: "default".into(),
                project: "missing-project".into(),
            },
            DreamScope {
                workspace: "missing-workspace".into(),
                project: "dev".into(),
            },
        ],
    );
    let before = snapshot(&state).await;

    for (request, expected) in [
        (
            json!({"workspace":"default","project":"missing-project"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"workspace":"missing-workspace","project":"dev"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"workspace":"foreign-workspace","project":"dev"}),
            StatusCode::FORBIDDEN,
        ),
        (
            json!({"workspace":"default"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (json!({"project":"dev"}), StatusCode::UNPROCESSABLE_ENTITY),
        (
            json!({"workspace":"","project":"dev"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"workspace":"default","project":" "}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _) = post(&router, "/admin/dream", request, None, &[]).await;
        assert_eq!(status, expected);
        assert_eq!(snapshot(&state).await, before);
    }
    assert!(
        state
            .reader
            .find_workspace("missing-workspace".into())
            .await
            .unwrap()
            .is_none()
    );
    let existing = lookup_existing_scope(&state.reader, "default", "dev")
        .await
        .unwrap();
    assert!(
        state
            .reader
            .find_project(existing.workspace_id, "missing-project".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_dry_controls(&router, &payload, None).await;

    let (status, result) = post(
        &router,
        "/admin/dream",
        json!({"workspace":"default","project":"dev","apply":true}),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(result["scopes"].is_null());
    assert_eq!(snapshot(&state).await, before);
}
