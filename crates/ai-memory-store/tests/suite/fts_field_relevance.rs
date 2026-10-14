//! Page topics and body evidence must both survive a three-hit FTS budget.
//! Fixtures are synthetic; ranking assertions describe useful results rather
//! than reproducing the scorer's formula.

use ai_memory_core::{NewPage, NewUser, PagePath, ProjectId, Tier, UserId, WorkspaceId};
use ai_memory_store::{AccessMode, GrantLevel, Store, f32_vec_to_bytes};

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

async fn user(store: &Store, name: &str, token_byte: u8) -> UserId {
    store
        .writer
        .create_user(
            NewUser {
                username: name.into(),
                name: None,
                email: None,
            },
            [token_byte; ai_memory_store::TOKEN_HASH_LEN],
        )
        .await
        .unwrap()
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
        .search_pages_with_meta(query, 3, None, None)
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
async fn body_only_matches_remain_visible_without_admitting_unmatched_pages() {
    let (_tmp, store, ws, proj) = fixture().await;
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
    let ghost = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/ghost.md",
            "Other inventory",
            "no relevant terms",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let hits = store
        .reader
        .search_pages_for_project(ws, proj, "connection".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, body_only);
    assert!(!hits.iter().any(|hit| hit.id == ghost));
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
async fn field_candidates_cannot_cross_scope_or_resurrect_hidden_pages() {
    let (_tmp, store, ws, proj) = fixture().await;
    let other_proj = store
        .writer
        .get_or_create_project(ws, "other", None)
        .await
        .unwrap();
    let other_ws = store.writer.get_or_create_workspace("other").await.unwrap();
    let foreign_proj = store
        .writer
        .get_or_create_project(other_ws, "app", None)
        .await
        .unwrap();
    for (foreign_ws, foreign_project) in [(ws, other_proj), (other_ws, foreign_proj)] {
        let foreign = store
            .writer
            .upsert_page(page(
                foreign_ws,
                foreign_project,
                "procedures/control.md",
                "Connection",
                "foreign inventory",
                Tier::Procedural,
            ))
            .await
            .unwrap();
        let own = store
            .reader
            .search_pages_for_project(foreign_ws, foreign_project, "connection".into(), 3, None)
            .await
            .unwrap();
        assert!(
            own.iter().any(|hit| hit.id == foreign),
            "the foreign page exists and matches in its own scope"
        );
    }
    let superseded = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/version.md",
            "Connection",
            "old transport",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let latest = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/version.md",
            "Inventory",
            "new transport",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    assert_ne!(superseded, latest);
    let mut expired = page(
        ws,
        proj,
        "procedures/expired.md",
        "Connection",
        "expired transport",
        Tier::Procedural,
    );
    expired.expires_at = Some(jiff::Timestamp::from_microsecond(1).unwrap());
    let expired_id = store.writer.upsert_page(expired).await.unwrap();
    let legitimate = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/current.md",
            "Inventory",
            "connection is available",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let hits = store
        .reader
        .search_pages_for_project(ws, proj, "connection".into(), 3, None)
        .await
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "only a current, live page in the requested scope is readable: {hits:?}"
    );
    assert_eq!(hits[0].id, legitimate);
    assert!(
        !hits
            .iter()
            .any(|hit| hit.id == superseded || hit.id == expired_id)
    );
    let audit = store
        .reader
        .search_pages_for_project(ws, proj, "connection".into(), 3, Some(i64::MIN))
        .await
        .unwrap();
    assert!(
        audit.iter().any(|hit| hit.id == expired_id),
        "explicit expiry override proves the expired fixture matches"
    );
    let mismatched = store
        .reader
        .search_pages_for_project(other_ws, proj, "connection".into(), 3, None)
        .await
        .unwrap();
    assert!(
        mismatched.is_empty(),
        "a project id offered with the wrong workspace must fail closed"
    );
}

#[tokio::test]
async fn title_candidates_obey_access_grants_without_filtering_page_authors() {
    let (_tmp, store, ws, proj) = fixture().await;
    let author = user(&store, "author", 1).await;
    let reader = user(&store, "reader", 2).await;
    let outsider = user(&store, "outsider", 3).await;
    store
        .writer
        .set_access_mode(proj, AccessMode::Restricted)
        .await
        .unwrap();
    store
        .writer
        .grant_memory(reader, proj, GrantLevel::Read, None)
        .await
        .unwrap();
    let mut shared = page(
        ws,
        proj,
        "procedures/shared.md",
        "Connection",
        "shared inventory",
        Tier::Procedural,
    );
    shared.author_id = Some(author);
    let shared_id = store.writer.upsert_page(shared).await.unwrap();
    for viewer in [None, Some(reader)] {
        let hits = store
            .reader
            .search_pages("connection".into(), 3, viewer)
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "root and the granted teammate can read shared evidence"
        );
        assert_eq!(hits[0].id, shared_id);
        let with_meta = store
            .reader
            .search_pages_with_meta("connection".into(), 3, None, viewer)
            .await
            .unwrap();
        assert_eq!(with_meta.len(), 1);
        assert_eq!(with_meta[0].path.as_str(), "procedures/shared.md");
    }
    let refused = store
        .reader
        .search_pages("connection".into(), 3, Some(outsider))
        .await
        .unwrap();
    assert!(
        refused.is_empty(),
        "title matches must not leak restricted pages"
    );
    let refused_with_meta = store
        .reader
        .search_pages_with_meta("connection".into(), 3, None, Some(outsider))
        .await
        .unwrap();
    assert!(
        refused_with_meta.is_empty(),
        "metadata search keeps the same read boundary"
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
async fn strongest_foreign_vector_cannot_cross_the_query_scope() {
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
    let local = store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "notes/local.md",
            "Local inventory",
            "local inventory",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    let foreign = store
        .writer
        .upsert_page(page(
            foreign_ws,
            foreign_proj,
            "notes/foreign.md",
            "Foreign inventory",
            "foreign inventory",
            Tier::Semantic,
        ))
        .await
        .unwrap();
    for (id, vector) in [(local, [0.8, 0.6]), (foreign, [1.0, 0.0])] {
        store
            .writer
            .store_embedding(
                id,
                f32_vec_to_bytes(&vector),
                "synthetic".into(),
                "two-dimensional".into(),
                2,
            )
            .await
            .unwrap();
    }
    for (scope_ws, scope_proj, expected) in [(ws, proj, local), (foreign_ws, foreign_proj, foreign)]
    {
        let hits = store
            .reader
            .hybrid_search(
                scope_ws,
                scope_proj,
                "unmatched".into(),
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
        assert_eq!(
            hits.len(),
            1,
            "the better foreign cosine cannot leak across scope: {hits:?}"
        );
        assert_eq!(hits[0].id, expected);
    }
    let mismatched = store
        .reader
        .hybrid_search(
            foreign_ws,
            proj,
            "unmatched".into(),
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
    assert!(
        mismatched.is_empty(),
        "a project id under the wrong workspace cannot expose its vector"
    );
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
