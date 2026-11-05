//! Synthetic compatibility fixtures for local runtime variants.

use ai_memory_core::{
    AgentKind, NewHandoff, NewPage, NewSession, PagePath, ProjectId, SessionId, Tier, WorkspaceId,
};
use ai_memory_store::{Store, f32_vec_to_bytes};
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

#[tokio::test]
async fn topical_title_survives_long_body_and_bounded_candidate_window() {
    let (_tmp, store, ws, proj) = fixture().await;
    let topic = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "procedures/connection.md",
            "Restore connection",
            &format!(
                "{}Reconnect using the existing session transport.",
                "background ".repeat(300)
            ),
            Tier::Procedural,
        ))
        .await
        .unwrap();
    // More than one normal candidate window: increasing its size alone must
    // not be the reason the page topic finally becomes a visible result.
    for index in 0..24 {
        store
            .writer
            .upsert_page(page(
                ws,
                proj,
                &format!("notes/status-{index}.md"),
                &format!("Status {index}"),
                &"restore connection ".repeat(20),
                Tier::Semantic,
            ))
            .await
            .unwrap();
    }
    let query = "restore connection".to_string();
    let scoped = store
        .reader
        .search_pages_for_project(ws, proj, query.clone(), 3, None)
        .await
        .unwrap();
    assert_eq!(scoped.len(), 3);
    assert!(
        scoped.iter().any(|hit| hit.id == topic),
        "the maintained page's topic must fit in three hits: {scoped:?}"
    );
    let global = store
        .reader
        .search_pages(query.clone(), 3, None)
        .await
        .unwrap();
    assert_eq!(global.len(), 3);
    assert!(global.iter().any(|hit| hit.id == topic));
    let with_meta = store
        .reader
        .search_pages_with_meta(query, 3, None, None, ai_memory_store::PageFilter::default())
        .await
        .unwrap();
    assert_eq!(with_meta.len(), 3);
    assert!(
        with_meta
            .iter()
            .any(|hit| hit.path.as_str() == "procedures/connection.md")
    );
}

#[tokio::test]
async fn equal_body_scores_allow_canonical_authority_to_promote_last_rule() {
    let (_tmp, store, ws, proj) = fixture().await;
    for index in 0..20 {
        store
            .writer
            .upsert_page(page(
                ws,
                proj,
                &format!("notes/background-{index}.md"),
                "Inventory background",
                "Ordinary inventory background",
                Tier::Semantic,
            ))
            .await
            .unwrap();
    }
    // Equal-length paths, titles and bodies give exactly tied BM25 scores.
    // The maintained rule is inserted last, after the low-authority sessions.
    for index in 0..5 {
        store
            .writer
            .upsert_page(page(
                ws,
                proj,
                &format!("sessions/match-{index}.md"),
                "Inventory record",
                "quartz recovery evidence",
                Tier::Episodic,
            ))
            .await
            .unwrap();
    }
    let mut rule = page(
        ws,
        proj,
        "_rules/match-5.md",
        "Inventory record",
        "quartz recovery evidence",
        Tier::Semantic,
    );
    rule.pinned = true;
    rule.frontmatter_json = serde_json::json!({"tags": ["canonical"]});
    let rule_id = store.writer.upsert_page(rule).await.unwrap();
    let raw_scores = store.reader.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT pages_fts.rank FROM pages_fts WHERE pages_fts MATCH 'quartz' ORDER BY pages_fts.rank")?;
        let scores = stmt.query_map([], |row| row.get::<_, f64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(scores)
    }).await.unwrap();
    assert_eq!(raw_scores.len(), 6);
    assert!(raw_scores[0] < -0.1);
    assert!(
        raw_scores.iter().all(|score| *score == raw_scores[0]),
        "the fixture must contain real BM25 ties"
    );
    let hits = store
        .reader
        .search_pages_for_project(ws, proj, "quartz".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(
        hits[0].id, rule_id,
        "equal body evidence must leave room for authority: {hits:?}"
    );
}

#[tokio::test]
async fn strongest_body_evidence_survives_session_authority_penalty() {
    let (_tmp, store, ws, proj) = fixture().await;
    // With the topic in less than half the corpus, BM25 has real IDF signal;
    // a corpus made entirely of matching pages supplies only its IDF floor.
    for index in 0..20 {
        store
            .writer
            .upsert_page(page(
                ws,
                proj,
                &format!("notes/background-{index}.md"),
                &format!("Inventory background {index}"),
                "Ordinary inventory background without a query topic.",
                Tier::Semantic,
            ))
            .await
            .unwrap();
    }
    for index in 0..6 {
        let mut noise = page(
            ws,
            proj,
            &format!("_rules/status-{index}.md"),
            &format!("Compiler review status {index}"),
            &format!(
                "{}{}",
                "compiler review linker ".repeat(10),
                "status ".repeat(10)
            ),
            Tier::Semantic,
        );
        noise.pinned = true;
        noise.frontmatter_json = serde_json::json!({"tags": ["canonical"]});
        store.writer.upsert_page(noise).await.unwrap();
    }
    let evidence = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "sessions/evidence.md",
            "Session evidence",
            &format!(
                "{}The author cannot approve their own work. A separate reviewer judges it.",
                "compiler review linker ".repeat(20)
            ),
            Tier::Episodic,
        ))
        .await
        .unwrap();
    let hits = store
        .reader
        .search_pages_for_project(ws, proj, "compiler review linker".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert!(
        hits.iter().any(|hit| hit.id == evidence),
        "authority must not crowd out the strongest body evidence: {hits:?}"
    );
    assert!(
        hits.iter()
            .any(|hit| hit.path.as_str().starts_with("_rules/"))
    );
}

#[tokio::test]
async fn explicit_body_column_does_not_promote_title_only_matches() {
    let (_tmp, store, ws, proj) = fixture().await;
    let title_only = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/title.md",
            "Connection",
            "unrelated inventory",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let body_only = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/body.md",
            "Inventory",
            "connection is available",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let hits = store
        .reader
        .search_pages_for_project(ws, proj, "body:connection".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, body_only);
    assert!(!hits.iter().any(|hit| hit.id == title_only));
    let title_hits = store
        .reader
        .search_pages_for_project(ws, proj, "title:connection".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(title_hits.len(), 1);
    assert_eq!(title_hits[0].id, title_only);
}

#[tokio::test]
async fn zero_title_scores_cannot_promote_weak_body_matches() {
    let (_tmp, store, ws, proj) = fixture().await;
    // It is the oldest matching row, so a zero-score title stream ordered by
    // row order would incorrectly give it the strongest title rank.
    let weak = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/weak.md",
            "Inventory archive",
            &format!("connection {}", "background ".repeat(300)),
            Tier::Semantic,
        ))
        .await
        .unwrap();
    for index in 0..4 {
        store
            .writer
            .upsert_page(page(
                ws,
                proj,
                &format!("notes/current-{index}.md"),
                &format!("Current inventory {index}"),
                "connection is available",
                Tier::Semantic,
            ))
            .await
            .unwrap();
    }
    let hits = store
        .reader
        .search_pages_for_project(ws, proj, "connection".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert!(
        hits.iter().all(|hit| hit.id != weak),
        "a title with no matching term supplies no ranking evidence: {hits:?}"
    );
    assert!(
        hits.iter()
            .all(|hit| hit.path.as_str().starts_with("notes/current-"))
    );
}

#[tokio::test]
async fn weak_entity_and_vector_votes_cannot_crowd_out_strong_lexical_evidence() {
    let (_tmp, store, ws, proj) = fixture().await;
    let lexical = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "sessions/evidence.md",
            "Session evidence",
            "compiler independent reviewer: somebody other than the author assesses the changes",
            Tier::Episodic,
        ))
        .await
        .unwrap();
    for index in 0..5 {
        let mut noise = page(
            ws,
            proj,
            &format!("_rules/inventory-{index}.md"),
            &format!("Inventory status {index}"),
            "Operational inventory estimates and historical counts.",
            Tier::Semantic,
        );
        noise.pinned = true;
        noise.frontmatter_json = serde_json::json!({"tags": ["canonical"]});
        noise.entities = vec!["compiler".into()];
        let noise_id = store.writer.upsert_page(noise).await.unwrap();
        store
            .writer
            .store_embedding(
                noise_id,
                f32_vec_to_bytes(&[0.25, 0.9375_f32.sqrt()]),
                "synthetic".into(),
                "two-dimensional".into(),
                2,
            )
            .await
            .unwrap();
    }
    // A vector can carry strong evidence even when the phrasing has no
    // lexical overlap. Reducing weak votes must keep this legitimate route.
    let semantic = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/assessor.md",
            "Assessor selection",
            "An assessor evaluates work created by somebody else.",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            semantic,
            f32_vec_to_bytes(&[1.0, 0.0]),
            "synthetic".into(),
            "two-dimensional".into(),
            2,
        )
        .await
        .unwrap();
    let query = "compiler independent reviewer".to_string();
    let fts = store
        .reader
        .search_pages_for_project(ws, proj, query.clone(), 3, None)
        .await
        .unwrap();
    assert_eq!(fts.len(), 1, "only the evidence matches the query text");
    assert_eq!(fts[0].id, lexical);
    let hits = store
        .reader
        .hybrid_search(
            ws,
            proj,
            query.clone(),
            Some(vec![1.0, 0.0]),
            "synthetic".into(),
            "two-dimensional".into(),
            2,
            3,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert!(
        hits.iter().any(|hit| hit.id == lexical),
        "several weak votes must not displace strong lexical evidence: {hits:?}"
    );
    assert!(
        hits.iter().any(|hit| hit.id == semantic),
        "strong semantic evidence remains useful without a lexical match"
    );
    let explained = store
        .reader
        .hybrid_search_explained(
            ws,
            proj,
            query,
            Some(vec![1.0, 0.0]),
            "synthetic".into(),
            "two-dimensional".into(),
            2,
            10,
            None,
            false,
        )
        .await
        .unwrap();
    let (_, weak) = explained
        .iter()
        .find(|(hit, _)| hit.path.as_str() == "_rules/inventory-0.md")
        .unwrap();
    assert!(weak.fts_rank.is_none());
    assert!(weak.graph_rank.is_none());
    assert!((weak.cosine.unwrap() - 0.25).abs() < 1e-6);
    assert!((weak.entity_weight.unwrap() - 0.2).abs() < f64::EPSILON);
    assert!(weak.rrf.vector < 1.0 / (60.0 + weak.vector_rank.unwrap() as f64));
    assert!(weak.rrf.entity < 1.0 / (60.0 + weak.entity_rank.unwrap() as f64));
    for (hit, details) in explained {
        let contributions = details.rrf.fts
            + details.rrf.entity
            + details.rrf.vector
            + details.rrf.abstract_vector
            + details.rrf.graph;
        assert!((details.fused - contributions).abs() < 1e-12);
        assert!((hit.rank + details.fused * details.authority.unwrap()).abs() < 1e-12);
    }
}

#[tokio::test]
async fn exact_entity_matches_outrank_partial_matches_without_hiding_them() {
    let (_tmp, store, ws, proj) = fixture().await;
    let mut compound = page(
        ws,
        proj,
        "notes/a-compound.md",
        "Item A",
        "Ordinary background item information.",
        Tier::Semantic,
    );
    compound.entities = vec!["compiler profiling inventory capacity".into()];
    let compound_id = store.writer.upsert_page(compound).await.unwrap();
    let mut exact = page(
        ws,
        proj,
        "notes/z-exact.md",
        "Item Z",
        "Different background item information.",
        Tier::Semantic,
    );
    exact.entities = vec!["compiler".into()];
    let exact_id = store.writer.upsert_page(exact).await.unwrap();
    let fts = store
        .reader
        .search_pages_for_project(ws, proj, "compiler".into(), 3, None)
        .await
        .unwrap();
    assert!(
        fts.is_empty(),
        "the entity-only fixture has no lexical evidence"
    );
    let hits = store
        .reader
        .entity_hits_for_project(ws, proj, "compiler", 3, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2, "a partial name remains discoverable");
    assert_eq!(
        hits[0].hit.id, exact_id,
        "equal rarity must not turn a partial prefix into a full-name match: {hits:?}"
    );
    assert_eq!(hits[1].hit.id, compound_id);
    assert!(hits[0].weight > hits[1].weight);
    let full = store
        .reader
        .entity_hits_for_project(ws, proj, "compiler profiling inventory capacity", 3, None)
        .await
        .unwrap();
    assert_eq!(
        full[0].hit.id, compound_id,
        "the complete name keeps its full match"
    );
    assert!(full.iter().any(|hit| hit.hit.id == exact_id));
}

#[tokio::test]
async fn foreign_entity_homonyms_cannot_cross_scope() {
    let (_tmp, store, ws, proj) = fixture().await;
    let foreign_ws = store
        .writer
        .get_or_create_workspace("foreign")
        .await
        .unwrap();
    let foreign_proj = store
        .writer
        .get_or_create_project(foreign_ws, "app", None)
        .await
        .unwrap();
    let mut local = page(
        ws,
        proj,
        "notes/local.md",
        "Local item",
        "Local background.",
        Tier::Semantic,
    );
    local.entities = vec!["compiler".into()];
    let local_id = store.writer.upsert_page(local).await.unwrap();
    let mut foreign = page(
        foreign_ws,
        foreign_proj,
        "notes/foreign.md",
        "Foreign item",
        "Foreign background.",
        Tier::Semantic,
    );
    foreign.entities = vec!["compiler".into()];
    let foreign_id = store.writer.upsert_page(foreign).await.unwrap();
    for (scope_ws, scope_proj, expected) in
        [(ws, proj, local_id), (foreign_ws, foreign_proj, foreign_id)]
    {
        let hits = store
            .reader
            .entity_hits_for_project(scope_ws, scope_proj, "compiler", 3, None)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "a homonym in a different scope cannot influence this lookup: {hits:?}"
        );
        assert_eq!(hits[0].hit.id, expected);
    }
    let mismatched = store
        .reader
        .entity_hits_for_project(foreign_ws, proj, "compiler", 3, None)
        .await
        .unwrap();
    assert!(
        mismatched.is_empty(),
        "the workspace and project pair governs entity matches too"
    );
}

#[tokio::test]
async fn overlapping_entity_prefixes_do_not_amplify_escaped_compound_names() {
    let (_tmp, store, ws, proj) = fixture().await;
    let mut compound = page(
        ws,
        proj,
        "notes/item.md",
        "Item",
        "Ordinary background information.",
        Tier::Semantic,
    );
    compound.entities = vec![r#"compiler "status\queue""#.into()];
    let compound_id = store.writer.upsert_page(compound).await.unwrap();
    let single = store
        .reader
        .entity_hits_for_project(ws, proj, "comp", 3, None)
        .await
        .unwrap();
    let overlapping = store
        .reader
        .entity_hits_for_project(ws, proj, "comp com comp", 3, None)
        .await
        .unwrap();
    assert_eq!(single.len(), 1);
    assert_eq!(overlapping.len(), 1);
    assert_eq!(single[0].hit.id, compound_id);
    assert_eq!(overlapping[0].hit.id, compound_id);
    assert!(single[0].weight.is_finite() && single[0].weight > 0.0);
    assert!(
        (single[0].weight - overlapping[0].weight).abs() < f64::EPSILON,
        "repeating a prefix or adding a shorter overlapping prefix supplies no new name coverage"
    );
}

#[tokio::test]
async fn explicit_compound_query_covers_short_name_components() {
    let (_tmp, store, ws, proj) = fixture().await;
    for (index, name) in ["ai-memory provider", "ai-js provider"]
        .into_iter()
        .enumerate()
    {
        let mut item = page(
            ws,
            proj,
            &format!("notes/item-{index}.md"),
            &format!("Item {index}"),
            "Ordinary background information.",
            Tier::Semantic,
        );
        item.entities = vec![name.into()];
        let id = store.writer.upsert_page(item).await.unwrap();
        let hits = store
            .reader
            .entity_hits_for_project(ws, proj, name, 3, None)
            .await
            .unwrap();
        let exact = hits.iter().find(|hit| hit.hit.id == id).unwrap();
        assert!(
            (exact.weight - 1.0).abs() < f64::EPSILON,
            "a fully written compound name covers its short components too: {exact:?}"
        );
    }
    let partial = store
        .reader
        .entity_hits_for_project(ws, proj, "memory provider", 3, None)
        .await
        .unwrap();
    assert_eq!(partial.len(), 2);
    assert!(
        partial
            .iter()
            .all(|hit| hit.weight > 0.0 && hit.weight < 1.0)
    );
    let bare_short = store
        .reader
        .entity_hits_for_project(ws, proj, "ai", 3, None)
        .await
        .unwrap();
    assert!(
        bare_short.is_empty(),
        "coverage expansion must not admit bare two-character query tokens"
    );
}

mod graph {
    //! Graph expansion must preserve its anchor's relevance and project scope.

    use ai_memory_core::{LinkTarget, NewPage, PageId, PagePath, ProjectId, Tier, WorkspaceId};
    use ai_memory_store::{Store, f32_vec_to_bytes};

    fn page(ws: WorkspaceId, proj: ProjectId, path: &str, title: &str, body: &str) -> NewPage {
        NewPage {
            workspace_id: ws,
            project_id: proj,
            path: PagePath::new(path).unwrap(),
            title: title.into(),
            body: body.into(),
            tier: Tier::Semantic,
            frontmatter_json: serde_json::json!({}),
            pinned: false,
            links: Vec::new(),
            author_id: None,
            expires_at: None,
            entities: Vec::new(),
            evidence: Vec::new(),
        }
    }

    fn link(workspace: Option<&str>, project: Option<&str>, path: &str) -> LinkTarget {
        LinkTarget {
            workspace: workspace.map(str::to_owned),
            project: project.map(str::to_owned),
            path: PagePath::new(path).unwrap(),
            relation: None,
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        store: Store,
        ws: WorkspaceId,
        proj: ProjectId,
        foreign_ws: WorkspaceId,
        anchor: PageId,
        answer: PageId,
        neighbors: Vec<PageId>,
        foreign: Vec<PageId>,
    }

    async fn fixture(cosine: f32) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let ws = store
            .writer
            .get_or_create_workspace("primary")
            .await
            .unwrap();
        let proj = store
            .writer
            .get_or_create_project(ws, "app", None)
            .await
            .unwrap();
        let foreign_ws = store
            .writer
            .get_or_create_workspace("foreign")
            .await
            .unwrap();
        let foreign_proj = store
            .writer
            .get_or_create_project(foreign_ws, "app", None)
            .await
            .unwrap();

        let mut answer = page(
            ws,
            proj,
            "sessions/answer.md",
            "orchard conduit",
            "orchard conduit instructions",
        );
        answer.tier = Tier::Episodic;
        let answer = store.writer.upsert_page(answer).await.unwrap();

        let mut neighbors = Vec::new();
        let mut links = Vec::new();
        for number in 0..4 {
            let path = format!("_rules/neighbor-{number}.md");
            let mut neighbor = page(
                ws,
                proj,
                &path,
                "maintained guidance",
                "unrelated archive guidance",
            );
            neighbor.frontmatter_json = serde_json::json!({"tags": ["canonical"]});
            neighbor.pinned = true;
            neighbors.push(store.writer.upsert_page(neighbor).await.unwrap());
            links.push(link(None, None, &path));
        }
        let foreign_out = store
            .writer
            .upsert_page(page(
                foreign_ws,
                foreign_proj,
                "notes/outgoing.md",
                "foreign outgoing",
                "unrelated foreign archive",
            ))
            .await
            .unwrap();
        links.push(link(Some("foreign"), Some("app"), "notes/outgoing.md"));
        let mut anchor = page(
            ws,
            proj,
            "notes/anchor.md",
            "vector anchor",
            "distant archive material",
        );
        anchor.links = links;
        let anchor = store.writer.upsert_page(anchor).await.unwrap();
        store
            .writer
            .store_embedding(
                anchor,
                f32_vec_to_bytes(&[cosine, (1.0 - cosine * cosine).sqrt()]),
                "fixture".into(),
                "two-dimensional".into(),
                2,
            )
            .await
            .unwrap();

        let mut foreign_in = page(
            foreign_ws,
            foreign_proj,
            "notes/incoming.md",
            "foreign incoming",
            "unrelated foreign archive",
        );
        foreign_in.links = vec![link(Some("primary"), Some("app"), "notes/anchor.md")];
        let foreign_in = store.writer.upsert_page(foreign_in).await.unwrap();
        Fixture {
            _tmp: tmp,
            store,
            ws,
            proj,
            foreign_ws,
            anchor,
            answer,
            neighbors,
            foreign: vec![foreign_out, foreign_in],
        }
    }

    async fn search(
        f: &Fixture,
        ws: WorkspaceId,
        limit: usize,
    ) -> Vec<(ai_memory_store::PageHit, ai_memory_store::SearchExplain)> {
        f.store
            .reader
            .hybrid_search_explained(
                ws,
                f.proj,
                "orchard conduit".into(),
                Some(vec![1.0, 0.0]),
                "fixture".into(),
                "two-dimensional".into(),
                2,
                limit,
                None,
                false,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn weak_vector_anchor_does_not_displace_a_strong_lexical_answer() {
        let f = fixture(0.1).await;
        let diagnostic = search(&f, f.ws, 10).await;
        assert!(diagnostic.len() >= 6);
        let (_, anchor) = diagnostic
            .iter()
            .find(|(hit, _)| hit.id == f.anchor)
            .unwrap();
        assert!((anchor.cosine.unwrap() - 0.1).abs() < 0.00001);
        assert!(anchor.fts_rank.is_none());
        assert!(anchor.entity_rank.is_none());
        let (_, answer) = diagnostic
            .iter()
            .find(|(hit, _)| hit.id == f.answer)
            .unwrap();
        assert_eq!(answer.fts_rank, Some(1));
        assert!(answer.graph_rank.is_none());

        let hits = search(&f, f.ws, 3).await;
        assert!(
            hits.iter().any(|(hit, _)| hit.id == f.answer),
            "a weak vector-only anchor must not lend full relevance to four pinned graph neighbors: {:?}",
            hits.iter()
                .map(|(hit, _)| hit.path.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn strong_vector_anchor_keeps_a_useful_graph_neighbor() {
        let f = fixture(1.0).await;
        let hits = search(&f, f.ws, 3).await;
        assert!(
            hits.iter()
                .any(|(hit, details)| f.neighbors.contains(&hit.id) && details.graph_rank.is_some()),
            "a relevant vector anchor should still contribute its linked guidance"
        );
    }

    #[tokio::test]
    async fn adding_weak_fts_link_preserves_strong_graph_origin() {
        let f = fixture(1.0).await;
        for number in 0..2 {
            f.store
                .writer
                .upsert_page(page(
                    f.ws,
                    f.proj,
                    &format!("notes/direct-{number}.md"),
                    "orchard conduit",
                    "orchard conduit orchard conduit orchard conduit",
                ))
                .await
                .unwrap();
        }
        let weak_page = page(
            f.ws,
            f.proj,
            "notes/weak.md",
            "distant reference",
            "orchard archive material",
        );
        let weak = f.store.writer.upsert_page(weak_page.clone()).await.unwrap();
        let target = f.neighbors[3];
        let diagnostic = search(&f, f.ws, 10).await;
        assert!(
            !diagnostic
                .iter()
                .any(|(hit, _)| f.foreign.contains(&hit.id)),
            "prioritizing the strong graph origin must still refuse its foreign links"
        );
        let (_, weak_details) = diagnostic.iter().find(|(hit, _)| hit.id == weak).unwrap();
        assert!(weak_details.fts_rank.is_some());
        assert!(-weak_details.fts_score.unwrap() < 0.5);
        let before = search(&f, f.ws, 3).await;
        let (_, before_details) = before.iter().find(|(hit, _)| hit.id == target).unwrap();
        assert_eq!(
            before_details.graph_via.as_ref().unwrap().seed_path,
            "notes/anchor.md"
        );

        let mut weak_page = weak_page;
        weak_page.links = vec![link(None, None, "_rules/neighbor-3.md")];
        weak_page.frontmatter_json = serde_json::json!({"links": ["_rules/neighbor-3.md"]});
        let linked_weak = f.store.writer.upsert_page(weak_page).await.unwrap();
        assert_ne!(
            linked_weak, weak,
            "the link metadata must create a new version"
        );
        let after = search(&f, f.ws, 3).await;
        let (_, after_details) = after
            .iter()
            .find(|(hit, _)| hit.id == target)
            .expect("adding a weak FTS link must not evict a neighbor of the strong vector seed");
        assert_eq!(
            after_details.graph_via.as_ref().unwrap().seed_path,
            "notes/anchor.md",
            "the strongest origin must supply the retained graph provenance"
        );
        assert_eq!(after_details.rrf.graph, before_details.rrf.graph);
    }

    #[tokio::test]
    async fn graph_expansion_refuses_foreign_links_and_wrong_workspace_pair() {
        let f = fixture(1.0).await;
        let hits = search(&f, f.ws, 10).await;
        assert!(
            hits.iter()
                .any(|(hit, details)| f.neighbors.contains(&hit.id) && details.graph_rank.is_some())
        );
        assert!(
            !hits.iter().any(|(hit, _)| f.foreign.contains(&hit.id)),
            "outgoing and incoming foreign links must stay outside project search"
        );
        assert!(
            search(&f, f.foreign_ws, 10).await.is_empty(),
            "a real project id paired with the wrong workspace must not expose its FTS, vector, or graph hits"
        );
    }
}

#[tokio::test]
async fn multiple_scopes_keep_strong_body_evidence_with_observation_stream_on_and_off() {
    let (_tmp, mut store, ws, _) = fixture().await;
    let mut scopes = Vec::new();
    let mut answer = None;
    for name in ["answer", "weak-one", "weak-two", "weak-three", "background"] {
        let proj = store
            .writer
            .get_or_create_project(ws, name, None)
            .await
            .unwrap();
        if name == "background" {
            for index in 0..20 {
                store
                    .writer
                    .upsert_page(page(
                        ws,
                        proj,
                        &format!("notes/background-{index}.md"),
                        "Inventory",
                        "common ordinary reference background",
                        Tier::Semantic,
                    ))
                    .await
                    .unwrap();
            }
            continue;
        }
        scopes.push((ws, proj));
        let item = if name == "answer" {
            page(
                ws,
                proj,
                "answer.md",
                "Useful answer",
                "common rare",
                Tier::Semantic,
            )
        } else {
            let mut rule = page(
                ws,
                proj,
                "rule.md",
                "Common",
                "routine reference",
                Tier::Semantic,
            );
            rule.pinned = true;
            rule.frontmatter_json = serde_json::json!({"kind":"rule","tags":["canonical"]});
            rule
        };
        let id = store.writer.upsert_page(item).await.unwrap();
        if name == "answer" {
            answer = Some(id);
        }
    }
    for observation_stream in [false, true] {
        store
            .reader
            .set_retrieval_tuning(ai_memory_store::RetrievalTuning {
                observation_stream,
                ..Default::default()
            });
        let (hits, _) = store
            .reader
            .hybrid_search_scopes(
                &scopes,
                ai_memory_store::HybridSearch {
                    query: "common rare",
                    vector: None,
                    limit: 3,
                    expiry_cutoff_us: None,
                    include_superseded: false,
                    explain: true,
                    owner: &ai_memory_core::OwnerFilter::Any,
                    filter: ai_memory_store::PageFilter::default(),
                },
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0].0.id,
            answer.unwrap(),
            "strong evidence must remain comparable across scopes: {hits:?}"
        );
        let (none, _) = store
            .reader
            .hybrid_search_scopes(
                &[],
                ai_memory_store::HybridSearch {
                    query: "common rare",
                    vector: None,
                    limit: 3,
                    expiry_cutoff_us: None,
                    include_superseded: false,
                    explain: false,
                    owner: &ai_memory_core::OwnerFilter::Any,
                    filter: ai_memory_store::PageFilter::default(),
                },
            )
            .await
            .unwrap();
        assert!(none.is_empty());
    }
}
