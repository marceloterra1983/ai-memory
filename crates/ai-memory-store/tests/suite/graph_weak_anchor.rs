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
