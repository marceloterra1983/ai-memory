//! A project's own name is scope context, not a useful entity match.

use ai_memory_core::{NewPage, PagePath, Tier};
use ai_memory_store::Store;

fn page(
    ws: ai_memory_core::WorkspaceId,
    proj: ai_memory_core::ProjectId,
    path: &str,
    body: &str,
) -> NewPage {
    NewPage {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new(path).unwrap(),
        title: path.into(),
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

#[tokio::test]
async fn entity_stream_does_not_vote_for_the_scoped_project_name() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default".to_string())
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "QLMED".to_string(), None)
        .await
        .unwrap();

    let mut migration = page(ws, proj, "decisions/migration.md", "The project moved.");
    migration.entities = vec!["qlmed".to_string()];
    store.writer.upsert_page(migration).await.unwrap();

    let mut database = page(ws, proj, "docs/database.md", "Postgres stores invoices.");
    database.entities = vec!["postgres".to_string()];
    store.writer.upsert_page(database).await.unwrap();

    let project_name_hits = store
        .reader
        .entity_hits_for_project_at(ws, proj, "QLMED", 10, None, None, false)
        .await
        .unwrap();
    assert!(
        project_name_hits.is_empty(),
        "the scoped project's own name must not add an entity vote: {project_name_hits:?}"
    );

    let ordinary_entity_hits = store
        .reader
        .entity_hits_for_project_at(ws, proj, "Postgres", 10, None, None, false)
        .await
        .unwrap();
    assert_eq!(ordinary_entity_hits.len(), 1, "{ordinary_entity_hits:?}");
    assert_eq!(
        ordinary_entity_hits[0].hit.path.as_str(),
        "docs/database.md"
    );
}
