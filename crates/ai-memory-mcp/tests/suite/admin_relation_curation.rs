//! The curator may remove exact sentinels, never rewrite knowledge or a stale head.
use ai_memory_core::{ActorContext, AuthLevel, PageEvidence, PageEvidenceKind, PagePath, Tier};
use ai_memory_mcp::{AdminState, admin_router};
use ai_memory_wiki::WritePageRequest;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

async fn fixture(state: &AdminState, project: &str) -> Value {
    let ws = state
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = state
        .writer
        .get_or_create_project(ws, project, None)
        .await
        .unwrap();
    let path = PagePath::new("notes/curate.md").unwrap();
    let id = state.wiki.write_page(WritePageRequest {
        workspace_id: ws, project_id: proj, path,
        frontmatter: json!({"relations":{"contradicts":["None","none.md","Free title"],"fixes":["N/A"]},
            "tags":["keep"],"pinned":true,"expires_at":"2099-01-01","custom":{"all":"keep"},"origin":{"session_id":"synthetic"}}),
        body: "# Synthetic\r\n\r\nAll facts and whitespace stay.\r\n".into(),
        tier: Tier::Procedural, pinned: true, title: None, admission_ctx: None,
        actor: ActorContext::anonymous(), author_id: None,
        evidence: vec![PageEvidence{kind:PageEvidenceKind::Session,source_id:"synthetic".into()}],
    }).await.unwrap();
    let hash = state
        .reader
        .with_conn(move |c| {
            c.query_row(
                "SELECT hex(body_sha256) FROM pages WHERE id=?1",
                [id.as_bytes()],
                |r| r.get::<_, String>(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    json!({"workspace":"default","project":project,"path":"notes/curate.md","expected_head_id":id.to_string(),"expected_body_sha256":hash})
}

async fn post(state: &AdminState, payload: Value, level: AuthLevel) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/admin/curate-relations")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    req.extensions_mut().insert(level);
    let response = admin_router(state.clone()).oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn relation_curation_is_dry_by_default_and_preserves_every_other_field_and_evidence() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = fixture(&state, "dev").await;
    let scope = ai_memory_store::lookup_existing_scope(&state.reader, "default", "dev")
        .await
        .unwrap();
    let before = state
        .wiki
        .read_page(
            scope.workspace_id,
            scope.project_id,
            &PagePath::new("notes/curate.md").unwrap(),
        )
        .unwrap();
    let (status, preview) = post(&state, payload.clone(), AuthLevel::Root).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["removed"], 3);
    assert_eq!(preview["applied"], false);
    assert_eq!(
        state
            .wiki
            .read_page(
                scope.workspace_id,
                scope.project_id,
                &PagePath::new("notes/curate.md").unwrap()
            )
            .unwrap(),
        before
    );
    let mut apply = payload.clone();
    apply["dry_run"] = json!(false);
    let (status, result) = post(&state, apply.clone(), AuthLevel::Root).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["applied"], true);
    assert!(result["before_checkpoint"].is_string());
    assert!(result["checkpoint"].is_string());
    let after = state
        .wiki
        .read_page(
            scope.workspace_id,
            scope.project_id,
            &PagePath::new("notes/curate.md").unwrap(),
        )
        .unwrap();
    assert_eq!(after.body, before.body);
    let mut expected = before.frontmatter.clone();
    expected["relations"]["contradicts"] = json!(["Free title"]);
    expected["relations"]["fixes"] = json!([]);
    assert_eq!(after.frontmatter, expected);
    let old_id = payload["expected_head_id"]
        .as_str()
        .unwrap()
        .parse::<ai_memory_core::PageId>()
        .unwrap();
    let new_id = result["page_id"]
        .as_str()
        .unwrap()
        .parse::<ai_memory_core::PageId>()
        .unwrap();
    assert_ne!(old_id, new_id);
    state
        .reader
        .with_conn(move |c| {
            let (previous, latest): (Vec<u8>, bool) = c.query_row(
                "SELECT supersedes,is_latest FROM pages WHERE id=?1",
                [new_id.as_bytes()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(previous, old_id.as_bytes());
            assert!(latest);
            let evidence: i64 = c.query_row(
                "SELECT count(*) FROM page_evidence WHERE page_id=?1",
                [new_id.as_bytes()],
                |r| r.get(0),
            )?;
            assert_eq!(evidence, 1);
            let old: i64 = c.query_row(
                "SELECT count(*) FROM pages WHERE id=?1",
                [old_id.as_bytes()],
                |r| r.get(0),
            )?;
            assert_eq!(old, 1);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        post(&state, apply, AuthLevel::Root).await.0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn relation_curation_refuses_foreign_heads_bad_hashes_unknown_fields_and_missing_scopes() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = fixture(&state, "dev").await;
    let foreign = fixture(&state, "other").await;
    let mut forged = payload.clone();
    forged["expected_head_id"] = foreign["expected_head_id"].clone();
    assert_eq!(
        post(&state, forged, AuthLevel::Root).await.0,
        StatusCode::CONFLICT
    );
    let mut bad_hash = payload.clone();
    bad_hash["expected_body_sha256"] = json!("00".repeat(32));
    assert_eq!(
        post(&state, bad_hash, AuthLevel::Root).await.0,
        StatusCode::CONFLICT
    );
    let mut unknown = payload.clone();
    unknown["body"] = json!("replace knowledge");
    assert_eq!(
        post(&state, unknown, AuthLevel::Root).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut missing = payload.clone();
    missing["workspace"] = json!("does-not-exist");
    assert_eq!(
        post(&state, missing, AuthLevel::Root).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(
        state
            .reader
            .find_workspace("does-not-exist".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        post(&state, payload, AuthLevel::Root).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn relation_curation_is_root_only_even_on_a_single_operator_install() {
    let tmp = TempDir::new().unwrap();
    let state = super::admin_write_page::make_state(&tmp).await;
    let payload = fixture(&state, "dev").await;
    for (level, status) in [
        (AuthLevel::Anonymous, StatusCode::UNAUTHORIZED),
        (AuthLevel::User, StatusCode::FORBIDDEN),
    ] {
        assert_eq!(post(&state, payload.clone(), level).await.0, status);
    }
    assert_eq!(
        post(&state, payload, AuthLevel::Root).await.0,
        StatusCode::OK
    );
}
