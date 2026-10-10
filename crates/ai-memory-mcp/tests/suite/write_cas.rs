//! Conditional MCP page writes and the head id returned with a path read.
//!
//! A dropped guard lets a retried client outbox overwrite a concurrent edit,
//! create a second version, or publish a page id that does not belong to the
//! body the caller just read.

use ai_memory_core::{
    ActorContext, NewPage, PageEvidence, PageEvidenceKind, PageId, PagePath, Tier, WorkspaceId,
};
use ai_memory_mcp::AiMemoryServer;
use ai_memory_store::Store;
use ai_memory_wiki::{Wiki, WritePageRequest};
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rusqlite::OpenFlags;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tempfile::TempDir;
use tower::ServiceExt;

const WS_NAME: &str = "default";
const PROJ_NAME: &str = "scratch";

struct Harness {
    router: Router,
    store: Store,
    tmp: TempDir,
    ws: WorkspaceId,
    proj: ai_memory_core::ProjectId,
}

fn mount(server: AiMemoryServer) -> Router {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true),
    );
    Router::new().nest_service("/mcp", service)
}

async fn harness() -> Harness {
    let tmp = TempDir::new().expect("tempdir");
    let store = Store::open(tmp.path()).expect("store");
    let ws = store
        .writer
        .get_or_create_workspace(WS_NAME)
        .await
        .expect("ws");
    let proj = store
        .writer
        .get_or_create_project(ws, PROJ_NAME, None)
        .await
        .expect("proj");
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).expect("wiki");
    let server =
        AiMemoryServer::new(store.reader.clone(), store.writer.clone(), ws, proj).with_wiki(wiki);
    Harness {
        router: mount(server),
        store,
        tmp,
        ws,
        proj,
    }
}

async fn rpc(router: &Router, name: &str, arguments: Value) -> Value {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    });
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(body.to_string()))
        .expect("mcp req");
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let bytes = axum::body::to_bytes(resp.into_body(), 4_000_000)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("non-JSON: {text}: {e}"))
}

fn tool_json(envelope: &Value) -> Value {
    if let Some(err) = envelope.get("error") {
        panic!("expected tool success, got {err}");
    }
    let joined = envelope
        .pointer("/result/content")
        .and_then(|c| c.as_array())
        .unwrap_or_else(|| panic!("missing result.content: {envelope}"))
        .iter()
        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&joined).unwrap_or_else(|e| panic!("tool text not JSON: {joined}: {e}"))
}

fn conflict(envelope: &Value, reason: &str) -> Option<String> {
    let err = envelope
        .get("error")
        .unwrap_or_else(|| panic!("expected page write conflict, got {envelope}"));
    assert_eq!(
        err["code"], -32009,
        "conflict must not look like a generic fault: {err}"
    );
    assert_eq!(err["data"]["code"], "page_write_conflict", "{err}");
    assert_eq!(err["data"]["reason"], reason, "{err}");
    match &err["data"]["current_page_id"] {
        Value::Null => None,
        Value::String(id) => {
            PageId::from_str(id).unwrap_or_else(|_| panic!("current_page_id is not a UUID: {id}"));
            Some(id.clone())
        }
        other => panic!("current_page_id must be a UUID string or null, got {other}"),
    }
}

fn write_args(path: &str, body: &str, extra: Value) -> Value {
    let mut args = json!({
        "workspace": WS_NAME,
        "project": PROJ_NAME,
        "path": path,
        "body": body,
    });
    if let Some(map) = extra.as_object() {
        for (key, value) in map {
            args[key] = value.clone();
        }
    }
    args
}

async fn write(router: &Router, path: &str, body: &str, extra: Value) -> Value {
    tool_json(&rpc(router, "memory_write_page", write_args(path, body, extra)).await)
}

async fn read(router: &Router, path: &str, project: &str) -> Value {
    tool_json(
        &rpc(
            router,
            "memory_read_page",
            json!({ "workspace": WS_NAME, "project": project, "path": path }),
        )
        .await,
    )
}

fn page_file(tmp: &Path, ws: WorkspaceId, proj: ai_memory_core::ProjectId, rel: &str) -> PathBuf {
    tmp.join("wiki")
        .join(ws.to_string())
        .join(proj.to_string())
        .join(rel)
}

fn count(store: &Store, sql: &str, params: &[&dyn rusqlite::ToSql]) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(store.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("readonly sqlite");
    conn.query_row(sql, params, |row| row.get(0)).expect(sql)
}

fn version_count(
    store: &Store,
    ws: WorkspaceId,
    proj: ai_memory_core::ProjectId,
    path: &str,
) -> i64 {
    count(
        store,
        "SELECT COUNT(*) FROM pages WHERE workspace_id = ?1 AND project_id = ?2 AND path = ?3",
        &[&ws.as_bytes(), &proj.as_bytes(), &path],
    )
}

fn evidence_count(
    store: &Store,
    ws: WorkspaceId,
    proj: ai_memory_core::ProjectId,
    path: &str,
) -> i64 {
    count(
        store,
        "SELECT COUNT(*) FROM page_evidence WHERE page_id IN (\
            SELECT id FROM pages WHERE workspace_id = ?1 AND project_id = ?2 AND path = ?3)",
        &[&ws.as_bytes(), &proj.as_bytes(), &path],
    )
}

fn link_count(store: &Store, to_path: &str) -> i64 {
    count(
        store,
        "SELECT COUNT(*) FROM links WHERE to_path = ?1",
        &[&to_path],
    )
}

fn workspace_count(store: &Store, name: &str) -> i64 {
    count(
        store,
        "SELECT COUNT(*) FROM workspaces WHERE name = ?1",
        &[&name],
    )
}

fn file_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Break: a path read returns a body without the head id that produced it,
/// so a later conditional write cannot name that snapshot.
#[tokio::test]
async fn path_read_returns_the_head_page_id_of_the_returned_body() {
    let h = harness().await;
    let path = "notes/head.md";
    let first = write(&h.router, path, "# Head\n\nfirst body ALPHAHEAD", json!({})).await;
    let first_id = first["page_id"]
        .as_str()
        .expect("write page_id")
        .to_string();
    PageId::from_str(&first_id).expect("write page_id uuid");

    let seen = read(&h.router, path, PROJ_NAME).await;
    assert_eq!(seen["body"], "# Head\n\nfirst body ALPHAHEAD");
    assert_eq!(seen["page_id"], first_id);
    assert!(seen.get("path").is_some());
    assert!(seen.get("title").is_some());
    assert!(seen.get("frontmatter").is_some());
    assert!(seen.get("served_from").is_none());

    let second = write(
        &h.router,
        path,
        "# Head\n\nsecond body BETAHEAD",
        json!({ "expected_page_id": first_id }),
    )
    .await;
    let second_id = second["page_id"].as_str().expect("second page_id");
    assert_ne!(second_id, first_id);
    let seen = read(&h.router, path, PROJ_NAME).await;
    assert_eq!(seen["body"], "# Head\n\nsecond body BETAHEAD");
    assert_eq!(seen["page_id"], second_id);
}

/// Break: db-fallback attaches an id from a different version than the body.
#[tokio::test]
async fn db_fallback_page_id_matches_the_body_row_only() {
    let h = harness().await;
    let path = "notes/fallback.md";
    let first = write(
        &h.router,
        path,
        "# Fallback\n\nold fallback body",
        json!({}),
    )
    .await;
    let old_id = first["page_id"].as_str().unwrap().to_string();
    let second = write(
        &h.router,
        path,
        "# Fallback\n\nnew fallback body",
        json!({}),
    )
    .await;
    let new_id = second["page_id"].as_str().unwrap().to_string();
    let file = page_file(h.tmp.path(), h.ws, h.proj, path);
    std::fs::remove_file(&file).unwrap();

    let seen = read(&h.router, path, PROJ_NAME).await;
    assert_eq!(seen["served_from"], "db-fallback");
    assert_eq!(seen["body"], "# Fallback\n\nnew fallback body");
    assert_eq!(seen["page_id"], new_id);
    assert_ne!(seen["page_id"], old_id);
}

/// Break: a disk body that is not the indexed head is still labeled with that head.
#[tokio::test]
async fn path_read_omits_page_id_when_disk_body_is_not_the_head() {
    let h = harness().await;
    let path = "notes/diverge.md";
    let written = write(
        &h.router,
        path,
        "# Disk\n\nstored body DELTAHEAD",
        json!({}),
    )
    .await;
    let head = written["page_id"].as_str().unwrap().to_string();
    let file = page_file(h.tmp.path(), h.ws, h.proj, path);
    std::fs::write(&file, "# Disk\n\ntampered body NOTTHEHEAD\n").unwrap();

    let seen = read(&h.router, path, PROJ_NAME).await;
    assert_eq!(seen["body"], "# Disk\n\ntampered body NOTTHEHEAD\n");
    assert!(seen.get("served_from").is_none(), "{seen}");
    assert_ne!(
        seen.get("page_id").and_then(|v| v.as_str()),
        Some(head.as_str())
    );
    assert!(
        seen.get("page_id").is_none(),
        "mismatched disk body must not carry a head id: {seen}"
    );
}

/// Break: create_only overwrites an existing path, including under two callers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_only_admits_one_writer_and_keeps_the_other_off_disk() {
    let h = harness().await;
    let path = "notes/create-only.md";
    let router_a = h.router.clone();
    let router_b = h.router.clone();
    let (left, right) = tokio::join!(
        rpc(
            &router_a,
            "memory_write_page",
            write_args(
                path,
                "# Create\n\nCREATEALPHA [[notes/should-not-link.md]]",
                json!({ "create_only": true })
            )
        ),
        rpc(
            &router_b,
            "memory_write_page",
            write_args(
                path,
                "# Create\n\nCREATEBETA",
                json!({ "create_only": true })
            )
        ),
    );
    let outcomes = [left, right];
    let wins: Vec<&Value> = outcomes
        .iter()
        .filter(|v| v.get("error").is_none())
        .collect();
    let losses: Vec<&Value> = outcomes
        .iter()
        .filter(|v| v.get("error").is_some())
        .collect();
    assert_eq!(wins.len(), 1, "exactly one create_only wins: {outcomes:?}");
    assert_eq!(
        losses.len(),
        1,
        "exactly one create_only conflicts: {outcomes:?}"
    );
    let current = conflict(losses[0], "already_exists");
    let winner = tool_json(wins[0]);
    let winner_id = winner["page_id"].as_str().unwrap();
    assert_eq!(current.as_deref(), Some(winner_id));
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), 1);
    let body = h
        .store
        .reader
        .page_body_by_ids(h.ws, h.proj, path)
        .await
        .unwrap()
        .unwrap()
        .body;
    let winner_body = if body.contains("CREATEALPHA") {
        "CREATEALPHA"
    } else if body.contains("CREATEBETA") {
        "CREATEBETA"
    } else {
        panic!("winner body missing: {body}");
    };
    let loser_body = if winner_body == "CREATEALPHA" {
        "CREATEBETA"
    } else {
        "CREATEALPHA"
    };
    let file = file_text(&page_file(h.tmp.path(), h.ws, h.proj, path));
    assert!(file.contains(winner_body), "{file}");
    assert!(!file.contains(loser_body), "{file}");
    assert!(!body.contains(loser_body), "{body}");
    assert_eq!(
        link_count(&h.store, "notes/should-not-link.md"),
        i64::from(winner_body == "CREATEALPHA")
    );
    let again = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Create\n\nCREATEALPHA",
            json!({ "create_only": true }),
        ),
    )
    .await;
    conflict(&again, "already_exists");
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), 1);
    assert_eq!(
        h.store
            .reader
            .page_body_by_ids(h.ws, h.proj, path)
            .await
            .unwrap()
            .unwrap()
            .body,
        body
    );
}

/// Break: a stale expected head still appends a version and replaces the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expected_page_id_allows_one_successor_and_a_stale_peer_changes_nothing() {
    let h = harness().await;
    let path = "notes/cas.md";
    let origin = write(&h.router, path, "# Cas\n\norigin body ORIGIN", json!({})).await;
    let origin_id = origin["page_id"].as_str().unwrap().to_string();
    let before_file = std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap();
    let router_a = h.router.clone();
    let router_b = h.router.clone();
    let expected = json!({ "expected_page_id": origin_id });
    let (left, right) = tokio::join!(
        rpc(
            &router_a,
            "memory_write_page",
            write_args(
                path,
                "# Cas\n\nSTALEALPHA [[notes/stale-link.md]]",
                expected.clone()
            )
        ),
        rpc(
            &router_b,
            "memory_write_page",
            write_args(path, "# Cas\n\nSTALEBETA", expected)
        ),
    );
    let outcomes = [left, right];
    let wins: Vec<&Value> = outcomes
        .iter()
        .filter(|v| v.get("error").is_none())
        .collect();
    let losses: Vec<&Value> = outcomes
        .iter()
        .filter(|v| v.get("error").is_some())
        .collect();
    assert_eq!(wins.len(), 1, "{outcomes:?}");
    assert_eq!(losses.len(), 1, "{outcomes:?}");
    let winner = tool_json(wins[0]);
    let winner_id = winner["page_id"].as_str().unwrap().to_string();
    assert_ne!(winner_id, origin_id);
    assert_eq!(
        conflict(losses[0], "stale_head").as_deref(),
        Some(winner_id.as_str())
    );
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), 2);
    let body = h
        .store
        .reader
        .page_body_by_ids(h.ws, h.proj, path)
        .await
        .unwrap()
        .unwrap()
        .body;
    let (winner_token, loser_token) = if body.contains("STALEALPHA") {
        ("STALEALPHA", "STALEBETA")
    } else if body.contains("STALEBETA") {
        ("STALEBETA", "STALEALPHA")
    } else {
        panic!("unexpected body {body}");
    };
    let file = file_text(&page_file(h.tmp.path(), h.ws, h.proj, path));
    assert!(file.contains(winner_token), "{file}");
    assert!(!file.contains(loser_token), "{file}");
    assert_eq!(
        link_count(&h.store, "notes/stale-link.md"),
        i64::from(winner_token == "STALEALPHA")
    );
    let stale = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Cas\n\nLOSTWRITE [[notes/should-not-link.md]]",
            json!({ "expected_page_id": origin_id }),
        ),
    )
    .await;
    let versions_before_loss = version_count(&h.store, h.ws, h.proj, path);
    let evidence_before_loss = evidence_count(&h.store, h.ws, h.proj, path);
    let file_before_loss = std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap();
    assert_eq!(
        conflict(&stale, "stale_head").as_deref(),
        Some(winner_id.as_str())
    );
    assert_eq!(
        version_count(&h.store, h.ws, h.proj, path),
        versions_before_loss
    );
    assert_eq!(
        evidence_count(&h.store, h.ws, h.proj, path),
        evidence_before_loss
    );
    assert_eq!(
        std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap(),
        file_before_loss
    );
    assert!(!file_text(&page_file(h.tmp.path(), h.ws, h.proj, path)).contains("LOSTWRITE"));
    assert_eq!(link_count(&h.store, "notes/should-not-link.md"), 0);
    let hits = h
        .store
        .reader
        .search_pages_for_project(h.ws, h.proj, "LOSTWRITE".into(), 5, None)
        .await
        .unwrap();
    assert!(
        hits.is_empty(),
        "rejected body must stay out of the index: {hits:?}"
    );
    assert_ne!(before_file, file_before_loss);
}

/// Break: a direct store upsert between the client's read and write is overwritten.
#[tokio::test]
async fn stale_expected_head_rolls_back_when_the_store_lock_is_bypassed() {
    let h = harness().await;
    let path = "notes/bypass.md";
    let origin = write(
        &h.router,
        path,
        "# Bypass\n\noriginal bypass body",
        json!({}),
    )
    .await;
    let origin_id = origin["page_id"].as_str().unwrap().to_string();
    h.store
        .writer
        .upsert_page(NewPage {
            workspace_id: h.ws,
            project_id: h.proj,
            path: PagePath::new(path).unwrap(),
            title: "Bypass".into(),
            body: "concurrent store body BYPASSHEAD".into(),
            tier: Tier::Semantic,
            frontmatter_json: serde_json::json!({ "title": "Bypass" }),
            pinned: false,
            links: Vec::new(),
            author_id: None,
            expires_at: None,
            entities: Vec::new(),
            evidence: vec![PageEvidence {
                kind: PageEvidenceKind::Session,
                source_id: "bypass-evidence".into(),
            }],
        })
        .await
        .unwrap();
    let versions = version_count(&h.store, h.ws, h.proj, path);
    let evidence = evidence_count(&h.store, h.ws, h.proj, path);
    let file_before = std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap();
    assert!(evidence >= 1, "seed evidence missing");
    let rejected = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Bypass\n\nLOSTBYPASS [[notes/should-not-link.md]]",
            json!({ "expected_page_id": origin_id }),
        ),
    )
    .await;
    let current = conflict(&rejected, "stale_head");
    assert_ne!(current.as_deref(), Some(origin_id.as_str()));
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), versions);
    assert_eq!(evidence_count(&h.store, h.ws, h.proj, path), evidence);
    assert_eq!(
        std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap(),
        file_before
    );
    assert_eq!(
        h.store
            .reader
            .page_body_by_ids(h.ws, h.proj, path)
            .await
            .unwrap()
            .unwrap()
            .body,
        "concurrent store body BYPASSHEAD"
    );
    assert_eq!(link_count(&h.store, "notes/should-not-link.md"), 0);
}

/// Break: another Wiki instance commits the same path and the conditional write clobbers it.
#[tokio::test]
async fn another_wiki_commit_wins_over_a_stale_expected_head() {
    let h = harness().await;
    let path = "notes/other-wiki.md";
    let origin = write(&h.router, path, "# Other\n\nfirst wiki body", json!({})).await;
    let origin_id = origin["page_id"].as_str().unwrap().to_string();
    let other = Wiki::new(h.tmp.path(), h.store.writer.clone()).unwrap();
    other
        .write_page(WritePageRequest {
            workspace_id: h.ws,
            project_id: h.proj,
            path: PagePath::new(path).unwrap(),
            frontmatter: serde_json::json!({ "title": "Other" }),
            body: "second wiki body OTHERWIKI".into(),
            tier: Tier::Semantic,
            pinned: false,
            title: Some("Other".into()),
            admission_ctx: None,
            author_id: None,
            actor: ActorContext::anonymous(),
            evidence: Vec::new(),
        })
        .await
        .unwrap();
    let versions = version_count(&h.store, h.ws, h.proj, path);
    let file_before = std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap();
    assert!(String::from_utf8_lossy(&file_before).contains("OTHERWIKI"));
    let rejected = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Other\n\nLOSTOTHER",
            json!({ "expected_page_id": origin_id }),
        ),
    )
    .await;
    conflict(&rejected, "stale_head");
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), versions);
    assert_eq!(
        std::fs::read(page_file(h.tmp.path(), h.ws, h.proj, path)).unwrap(),
        file_before
    );
    assert_eq!(
        h.store
            .reader
            .page_body_by_ids(h.ws, h.proj, path)
            .await
            .unwrap()
            .unwrap()
            .body,
        "second wiki body OTHERWIKI"
    );
}

/// Break: the same path in another project is treated as the caller's head.
#[tokio::test]
async fn guards_stay_inside_the_named_workspace_and_project() {
    let h = harness().await;
    let path = "notes/scoped.md";
    let alpha = write(
        &h.router,
        path,
        "# Scope\n\nalpha body SCOPEDALPHA",
        json!({}),
    )
    .await;
    let alpha_id = alpha["page_id"].as_str().unwrap().to_string();
    let beta = write(
        &h.router,
        path,
        "# Scope\n\nbeta body SCOPEDBETA",
        json!({ "project": "beta", "create_only": true }),
    )
    .await;
    let beta_id = beta["page_id"].as_str().unwrap().to_string();
    assert_ne!(alpha_id, beta_id);
    assert_eq!(
        read(&h.router, path, PROJ_NAME).await["body"],
        "# Scope\n\nalpha body SCOPEDALPHA"
    );
    assert_eq!(read(&h.router, path, "beta").await["page_id"], beta_id);

    let cross = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Scope\n\ncross body SCOPEDGAMMA",
            json!({ "project": "beta", "expected_page_id": alpha_id }),
        ),
    )
    .await;
    conflict(&cross, "stale_head");
    assert_eq!(
        read(&h.router, path, "beta").await["body"],
        "# Scope\n\nbeta body SCOPEDBETA"
    );
    assert_eq!(
        read(&h.router, path, PROJ_NAME).await["body"],
        "# Scope\n\nalpha body SCOPEDALPHA"
    );
    assert_eq!(read(&h.router, path, PROJ_NAME).await["page_id"], alpha_id);

    let duplicate = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Scope\n\nbeta again",
            json!({ "project": "beta", "create_only": true }),
        ),
    )
    .await;
    assert_eq!(
        conflict(&duplicate, "already_exists").as_deref(),
        Some(beta_id.as_str())
    );
    assert_eq!(
        read(&h.router, path, "beta").await["body"],
        "# Scope\n\nbeta body SCOPEDBETA"
    );
}

/// Break: invalid guard arguments create a scope or a page before refusal.
#[tokio::test]
async fn invalid_guard_arguments_do_not_mutate() {
    let h = harness().await;
    let fresh = "cas-should-not-exist";
    assert_eq!(workspace_count(&h.store, fresh), 0);
    let both = rpc(
        &h.router,
        "memory_write_page",
        json!({
            "workspace": fresh,
            "project": "nope",
            "path": "notes/invalid.md",
            "body": "# Invalid\n\nshould not land",
            "create_only": true,
            "expected_page_id": "11111111-1111-1111-1111-111111111111"
        }),
    )
    .await;
    let err = both.get("error").expect("combined guards must be refused");
    assert_eq!(err["code"], -32602, "{err}");
    assert_eq!(workspace_count(&h.store, fresh), 0);
    assert!(!page_file(h.tmp.path(), h.ws, h.proj, "notes/invalid.md").exists());

    let bad_uuid = rpc(
        &h.router,
        "memory_write_page",
        json!({
            "workspace": fresh,
            "project": "nope",
            "path": "notes/invalid.md",
            "body": "# Invalid\n\nstill should not land",
            "expected_page_id": "not-a-uuid"
        }),
    )
    .await;
    let err = bad_uuid.get("error").expect("bad uuid must be refused");
    assert_eq!(err["code"], -32602, "{err}");
    assert_eq!(workspace_count(&h.store, fresh), 0);

    let missing = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            "notes/absent.md",
            "# Absent\n\nno such head",
            json!({ "expected_page_id": "22222222-2222-2222-2222-222222222222" }),
        ),
    )
    .await;
    assert!(conflict(&missing, "stale_head").is_none());
    assert_eq!(version_count(&h.store, h.ws, h.proj, "notes/absent.md"), 0);
    assert!(!page_file(h.tmp.path(), h.ws, h.proj, "notes/absent.md").exists());
}

/// Break: `create_only` replaces an on-disk page the index has not seen.
///
/// An external file with no live head is a real state (watcher/reconcile).
/// Refusal is `already_exists` with `current_page_id: null`, and the bytes,
/// index, links, and evidence stay as they were.
#[tokio::test]
async fn create_only_refuses_an_unindexed_file_without_changing_it() {
    let h = harness().await;
    let path = "notes/unindexed.md";
    let file = page_file(h.tmp.path(), h.ws, h.proj, path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let original = "# External\n\nunindexed body KEEPME [[notes/external-link.md]]\n";
    std::fs::write(&file, original).unwrap();
    let before = std::fs::read(&file).unwrap();
    #[cfg(unix)]
    let inode = file_inode(&file);
    let versions = version_count(&h.store, h.ws, h.proj, path);
    let evidence = evidence_count(&h.store, h.ws, h.proj, path);
    let external_links = link_count(&h.store, "notes/external-link.md");
    assert_eq!(versions, 0);
    assert_eq!(evidence, 0);
    assert_eq!(external_links, 0);

    let seen = read(&h.router, path, PROJ_NAME).await;
    assert!(seen["body"].as_str().unwrap().contains("KEEPME"), "{seen}");
    assert!(
        seen.get("page_id").is_none(),
        "an unindexed file has no head to name: {seen}"
    );

    let rejected = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# External\n\nCREATECLOBBER [[notes/create-should-not-link.md]]",
            json!({ "create_only": true }),
        ),
    )
    .await;
    assert!(
        conflict(&rejected, "already_exists").is_none(),
        "no live row means current_page_id null: {rejected}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), before, "bytes changed");
    #[cfg(unix)]
    assert_eq!(file_inode(&file), inode, "file was replaced");
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), 0);
    assert_eq!(evidence_count(&h.store, h.ws, h.proj, path), 0);
    assert_eq!(link_count(&h.store, "notes/external-link.md"), 0);
    assert_eq!(link_count(&h.store, "notes/create-should-not-link.md"), 0);
    let still = read(&h.router, path, PROJ_NAME).await;
    assert!(
        still["body"].as_str().unwrap().contains("KEEPME"),
        "{still}"
    );
    assert!(still.get("page_id").is_none(), "{still}");
}

/// Break: `expected_page_id` overwrites disk bytes that `memory_read_page`
/// cannot attach to the live head. Those bytes are an unseen edit.
#[tokio::test]
async fn expected_page_id_refuses_a_divergent_file_it_cannot_link_to_the_head() {
    let h = harness().await;
    let path = "notes/diverge-cas.md";
    let written = write(
        &h.router,
        path,
        "# Disk\n\nstored body DELTAHEAD",
        json!({}),
    )
    .await;
    let head = written["page_id"].as_str().unwrap().to_string();
    let file = page_file(h.tmp.path(), h.ws, h.proj, path);
    let tampered = "# Disk\n\ntampered body NOTINDEXED [[notes/tamper-link.md]]\n";
    std::fs::write(&file, tampered).unwrap();
    let before = std::fs::read(&file).unwrap();
    #[cfg(unix)]
    let inode = file_inode(&file);
    let versions = version_count(&h.store, h.ws, h.proj, path);
    let evidence = evidence_count(&h.store, h.ws, h.proj, path);
    assert_eq!(versions, 1);

    let seen = read(&h.router, path, PROJ_NAME).await;
    assert!(
        seen["body"].as_str().unwrap().contains("NOTINDEXED"),
        "{seen}"
    );
    assert!(
        seen.get("page_id").is_none(),
        "divergent disk must not carry the head id: {seen}"
    );

    let rejected = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Disk\n\nOVERWRITE [[notes/overwrite-link.md]]",
            json!({ "expected_page_id": head }),
        ),
    )
    .await;
    assert_eq!(
        conflict(&rejected, "stale_head").as_deref(),
        Some(head.as_str()),
        "{rejected}"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        before,
        "divergent bytes changed"
    );
    #[cfg(unix)]
    assert_eq!(file_inode(&file), inode, "divergent file was replaced");
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), versions);
    assert_eq!(evidence_count(&h.store, h.ws, h.proj, path), evidence);
    assert_eq!(link_count(&h.store, "notes/tamper-link.md"), 0);
    assert_eq!(link_count(&h.store, "notes/overwrite-link.md"), 0);
    assert_eq!(
        h.store
            .reader
            .page_body_by_ids(h.ws, h.proj, path)
            .await
            .unwrap()
            .unwrap()
            .body,
        "# Disk\n\nstored body DELTAHEAD"
    );
    let still = read(&h.router, path, PROJ_NAME).await;
    assert!(
        still["body"].as_str().unwrap().contains("NOTINDEXED"),
        "{still}"
    );
    assert!(still.get("page_id").is_none(), "{still}");
}

/// Break: a well-formed `expected_page_id` with no live row still swaps an
/// unindexed file and only restores it afterwards.
#[tokio::test]
async fn expected_page_id_leaves_an_unindexed_file_with_no_head_untouched() {
    let h = harness().await;
    let path = "notes/no-head.md";
    let file = page_file(h.tmp.path(), h.ws, h.proj, path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let original = "# Orphan\n\nunseen orphan KEEPFILE [[notes/orphan-link.md]]\n";
    std::fs::write(&file, original).unwrap();
    let before = std::fs::read(&file).unwrap();
    #[cfg(unix)]
    let inode = file_inode(&file);

    let rejected = rpc(
        &h.router,
        "memory_write_page",
        write_args(
            path,
            "# Orphan\n\nORPHANWRITE [[notes/orphan-should-not-link.md]]",
            json!({ "expected_page_id": "33333333-3333-3333-3333-333333333333" }),
        ),
    )
    .await;
    assert!(conflict(&rejected, "stale_head").is_none(), "{rejected}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    #[cfg(unix)]
    assert_eq!(file_inode(&file), inode, "unindexed file was replaced");
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), 0);
    assert_eq!(evidence_count(&h.store, h.ws, h.proj, path), 0);
    assert_eq!(link_count(&h.store, "notes/orphan-link.md"), 0);
    assert_eq!(link_count(&h.store, "notes/orphan-should-not-link.md"), 0);
}

#[cfg(unix)]
fn file_inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("metadata {}: {e}", path.display()))
        .ino()
}

/// Break: omitting the new arguments stops unconditional supersession.
#[tokio::test]
async fn omitted_guards_keep_the_unconditional_write() {
    let h = harness().await;
    let path = "notes/plain.md";
    let first = write(&h.router, path, "# Plain\n\nplain one", json!({})).await;
    let second = write(&h.router, path, "# Plain\n\nplain two", json!({})).await;
    assert_ne!(first["page_id"], second["page_id"]);
    assert_eq!(
        read(&h.router, path, PROJ_NAME).await["body"],
        "# Plain\n\nplain two"
    );
    assert_eq!(version_count(&h.store, h.ws, h.proj, path), 2);
}

/// Break: the published tool schema hides the guard arguments from the other client.
#[tokio::test]
async fn tool_schemas_publish_the_guard_and_keep_existing_fields() {
    let h = harness().await;
    let envelope = rpc_method(&h.router, "tools/list", json!({})).await;
    let tools = envelope["result"]["tools"].as_array().expect("tools");
    let write = tools
        .iter()
        .find(|tool| tool["name"] == "memory_write_page")
        .expect("memory_write_page");
    let props = &write["inputSchema"]["properties"];
    assert!(props.get("path").is_some());
    assert!(props.get("body").is_some());
    assert!(props.get("create_only").is_some(), "{props}");
    assert!(props.get("expected_page_id").is_some(), "{props}");
    let read = tools
        .iter()
        .find(|tool| tool["name"] == "memory_read_page")
        .expect("memory_read_page");
    assert!(read["inputSchema"]["properties"].get("path").is_some());
    for key in ["anyOf", "oneOf", "allOf"] {
        assert!(write["inputSchema"].get(key).is_none(), "{key}");
        assert!(read["inputSchema"].get(key).is_none(), "{key}");
    }
}

async fn rpc_method(router: &Router, method: &str, params: Value) -> Value {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(body.to_string()))
        .expect("mcp req");
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let bytes = axum::body::to_bytes(resp.into_body(), 4_000_000)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("non-JSON: {text}: {e}"))
}
