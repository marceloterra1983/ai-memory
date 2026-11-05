//! Synthetic compatibility fixtures for local runtime variants.

use ai_memory_core::{AgentKind, NewHandoff, NewSession, ProjectId, SessionId, WorkspaceId};
use ai_memory_store::Store;
use rusqlite::{Connection, params};

async fn fixture() -> (tempfile::TempDir, Store, WorkspaceId, ProjectId) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "app", None)
        .await
        .unwrap();
    (tmp, store, ws, proj)
}

fn handoff(ws: WorkspaceId, proj: ProjectId, session: Option<SessionId>, cwd: &str) -> NewHandoff {
    NewHandoff {
        workspace_id: ws,
        project_id: proj,
        from_session_id: session,
        from_agent: AgentKind::ClaudeCode,
        to_agent: None,
        cwd: Some(cwd.into()),
        summary: "Pending work".into(),
        open_questions: vec![],
        next_steps: vec![],
        files_touched: vec![],
        owner_user: None,
    }
}

async fn session(store: &Store, ws: WorkspaceId, proj: ProjectId) -> SessionId {
    let id = SessionId::new();
    store
        .writer
        .begin_session(NewSession {
            id,
            workspace_id: ws,
            project_id: proj,
            occurred_at: None,
            agent_kind: AgentKind::ClaudeCode,
            cwd: None,
            actor_user: None,
        })
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn open_handoffs_expose_origin_degrade_bad_origin_and_keep_scope() {
    let (_tmp, store, ws, proj) = fixture().await;
    let session = session(&store, ws, proj).await;
    let bad = store
        .writer
        .insert_handoff(handoff(ws, proj, Some(session), "/bad"))
        .await
        .unwrap();
    let auto = store
        .writer
        .insert_handoff(handoff(ws, proj, Some(session), "/auto"))
        .await
        .unwrap();
    let manual = store
        .writer
        .insert_handoff(handoff(ws, proj, None, "/manual"))
        .await
        .unwrap();
    let foreign_ws = store
        .writer
        .get_or_create_workspace("foreign")
        .await
        .unwrap();
    let foreign = store
        .writer
        .get_or_create_project(foreign_ws, "app", None)
        .await
        .unwrap();
    store
        .writer
        .insert_handoff(handoff(foreign_ws, foreign, None, "/foreign"))
        .await
        .unwrap();
    let sibling = store
        .writer
        .get_or_create_project(ws, "sibling", None)
        .await
        .unwrap();
    store
        .writer
        .insert_handoff(handoff(ws, sibling, None, "/sibling"))
        .await
        .unwrap();
    let conn = Connection::open(store.db_path()).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    conn.execute(
        "UPDATE handoffs SET from_session_id = ?1, created_at = 1 WHERE id = ?2",
        params![vec![0_u8; 17], bad.as_bytes()],
    )
    .unwrap();
    let listed = store
        .reader
        .open_handoffs_for_project(ws, proj, 50)
        .await
        .unwrap();
    assert_eq!(listed.len(), 3, "neither foreign scope can enter the list");
    let json = |id: ai_memory_core::HandoffId| {
        serde_json::to_value(listed.iter().find(|h| h.id == id.to_string()).unwrap()).unwrap()
    };
    assert_eq!(json(auto)["from_session_id"], session.to_string());
    for id in [bad, manual] {
        assert!(
            json(id).get("from_session_id").is_some_and(|v| v.is_null()),
            "{id}"
        );
    }
    let oldest = store
        .reader
        .open_handoffs_for_project(ws, proj, 1)
        .await
        .unwrap();
    assert_eq!(
        oldest.len(),
        1,
        "bad origin must not hide the oldest handoff"
    );
    assert_eq!(oldest[0].id, bad.to_string());
}
