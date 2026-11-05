//! Synthetic compatibility fixtures for local runtime variants.

use ai_memory_core::{
    AgentKind, NewHandoff, NewPage, NewSession, PagePath, ProjectId, SessionId, Tier, WorkspaceId,
};
use ai_memory_store::Store;
use rusqlite::{Connection, params};

fn page(
    ws: WorkspaceId,
    proj: ProjectId,
    path: &str,
    title: &str,
    body: &str,
    tier: Tier,
) -> NewPage {
    NewPage {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new(path).unwrap(),
        title: title.into(),
        body: body.into(),
        tier,
        frontmatter_json: serde_json::json!({}),
        pinned: false,
        links: Vec::new(),
        author_id: None,
        expires_at: None,
        entities: Vec::new(),
        evidence: Vec::new(),
    }
}

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

/// A row that cannot be decoded must not fail SessionStart or the
/// listing, and must not be delivered as a manual handoff. Nulling a
/// bad `from_session_id` would do that: manual batons are project-wide
/// and outrank automatic ones. Lookup by id stays strict.
#[tokio::test]
async fn unreadable_open_handoff_does_not_hide_a_deliverable_baton() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "ai-memory", None)
        .await
        .unwrap();
    let session = SessionId::new();
    store
        .writer
        .begin_session(NewSession {
            occurred_at: None,
            id: session,
            workspace_id: ws,
            project_id: proj,
            agent_kind: AgentKind::ClaudeCode,
            cwd: None,
            actor_user: None,
        })
        .await
        .unwrap();
    let good = store
        .writer
        .insert_handoff(NewHandoff {
            workspace_id: ws,
            project_id: proj,
            from_session_id: None,
            from_agent: AgentKind::Codex,
            to_agent: None,
            cwd: None,
            summary: "good baton".into(),
            open_questions: Vec::new(),
            next_steps: Vec::new(),
            files_touched: Vec::new(),
            owner_user: None,
        })
        .await
        .unwrap();
    let poison = store
        .writer
        .insert_handoff(NewHandoff {
            workspace_id: ws,
            project_id: proj,
            from_session_id: Some(session),
            from_agent: AgentKind::ClaudeCode,
            to_agent: None,
            cwd: Some("/poison".into()),
            summary: "poison baton".into(),
            open_questions: Vec::new(),
            next_steps: Vec::new(),
            files_touched: Vec::new(),
            owner_user: None,
        })
        .await
        .unwrap();
    // Newest row is the corrupt one, so a `LIMIT 1` scan hits it first
    // and a full scan still has to walk past it to reach the manual baton.
    let conn = Connection::open(store.db_path()).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    let changed = conn
        .execute(
            "UPDATE handoffs SET from_session_id = ?1, created_at = ?2 WHERE id = ?3",
            params![vec![0x00u8], 2_i64, poison.as_bytes()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let changed = conn
        .execute(
            "UPDATE handoffs SET created_at = ?1 WHERE id = ?2",
            params![1_i64, good.as_bytes()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    drop(conn);

    let summary =
        |handoff: Option<ai_memory_core::Handoff>| handoff.map(|handoff| handoff.content.summary);
    let delivered = store
        .reader
        .latest_open_handoff(ws, proj, None, ai_memory_core::OwnerFilter::Any)
        .await
        .expect("one bad row must not fail delivery");
    assert_eq!(summary(delivered).as_deref(), Some("good baton"));
    let startup = store
        .reader
        .startup_handoff(
            ws,
            proj,
            None,
            ai_memory_core::OwnerFilter::Any,
            jiff::Timestamp::now(),
        )
        .await
        .expect("SessionStart must still receive the readable baton");
    assert_eq!(summary(startup).as_deref(), Some("good baton"));
    let workspace = store
        .reader
        .latest_open_handoff_for_workspace(ws, ai_memory_core::OwnerFilter::Any, None)
        .await
        .expect("the workspace scan must skip the newest unreadable row");
    assert_eq!(summary(workspace).as_deref(), Some("good baton"));
    let listed = store
        .reader
        .list_handoffs(
            ws,
            proj,
            Some(ai_memory_core::HandoffState::Open),
            ai_memory_core::OwnerFilter::Any,
            10,
        )
        .await
        .expect("listing must skip the unreadable row");
    assert_eq!(
        listed
            .iter()
            .map(|handoff| handoff.content.summary.as_str())
            .collect::<Vec<_>>(),
        vec!["good baton".to_owned()]
    );
    assert!(
        store.reader.handoff_by_id(poison).await.is_err(),
        "lookup by id stays strict and must not turn the bad origin into a manual handoff"
    );

    let conn = Connection::open(store.db_path()).unwrap();
    conn.execute(
        "UPDATE handoffs SET open_questions = ?1 WHERE id = ?2",
        params!["not-json", good.as_bytes()],
    )
    .unwrap();
    drop(conn);
    assert!(
        store
            .reader
            .latest_open_handoff(ws, proj, None, ai_memory_core::OwnerFilter::Any)
            .await
            .expect("every unreadable row skips; the call still succeeds")
            .is_none()
    );
    assert!(
        store
            .reader
            .list_handoffs(
                ws,
                proj,
                Some(ai_memory_core::HandoffState::Open),
                ai_memory_core::OwnerFilter::Any,
                10
            )
            .await
            .expect("an unreadable history still lists")
            .is_empty()
    );
    let conn = Connection::open(store.db_path()).unwrap();
    conn.execute_batch("ALTER TABLE handoffs RENAME TO unavailable_handoffs")
        .unwrap();
    assert!(
        store
            .reader
            .latest_open_handoff(ws, proj, None, ai_memory_core::OwnerFilter::Any)
            .await
            .is_err(),
        "a SQLite failure must still propagate"
    );
}

#[tokio::test]
async fn recent_summaries_bound_sql_work_and_keep_live_scope_and_order() {
    let (_tmp, store, ws, proj) = fixture().await;
    let mut ids = Vec::new();
    for name in ["old", "a", "b", "expired"] {
        ids.push(
            store
                .writer
                .upsert_page(page(
                    ws,
                    proj,
                    &format!("notes/{name}.md"),
                    name,
                    "text",
                    Tier::Semantic,
                ))
                .await
                .unwrap(),
        );
    }
    let foreign_ws = store.writer.get_or_create_workspace("other").await.unwrap();
    let foreign = store
        .writer
        .get_or_create_project(foreign_ws, "app", None)
        .await
        .unwrap();
    store
        .writer
        .upsert_page(page(
            foreign_ws,
            foreign,
            "foreign.md",
            "Foreign",
            "text",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let conn = Connection::open(store.db_path()).unwrap();
    conn.execute(
        "UPDATE pages SET updated_at = 1, title = zeroblob(1) WHERE id = ?1",
        [ids[0].as_bytes()],
    )
    .unwrap();
    for id in &ids[1..3] {
        conn.execute(
            "UPDATE pages SET updated_at = 2000000 WHERE id = ?1",
            [id.as_bytes()],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE pages SET updated_at = 3000000, expires_at = 1 WHERE id = ?1",
        [ids[3].as_bytes()],
    )
    .unwrap();
    let recent = store
        .reader
        .recent_page_summaries("default", "app", 2)
        .await
        .unwrap();
    assert_eq!(
        recent.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        ["notes/a.md", "notes/b.md"]
    );
    assert_eq!(
        store
            .reader
            .recent_page_summaries("default", "app", 1)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .reader
            .recent_page_summaries("default", "absent", 2)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .reader
            .list_unexpired_pages("default", "app")
            .await
            .is_err(),
        "the unreadable older row is a positive control: a full scan must decode it"
    );
}
