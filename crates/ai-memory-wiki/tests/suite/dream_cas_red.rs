//! Reproduction of the stale-source overwrite in the original dream apply path.
use ai_memory_core::{ActorContext, PagePath, Tier};
use ai_memory_store::Store;
use ai_memory_wiki::{Wiki, WritePageRequest};

#[tokio::test]
async fn dream_stale_source_overwrite_reproduction() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("synthetic")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "dream", None)
        .await
        .unwrap();
    let request = |body: &str| WritePageRequest {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new("a.md").unwrap(),
        body: body.into(),
        frontmatter: serde_json::json!({}),
        tier: Tier::Episodic,
        pinned: false,
        title: None,
        admission_ctx: None,
        author_id: None,
        actor: ActorContext::anonymous(),
        evidence: vec![],
    };
    wiki.write_page(request("original fact\n")).await.unwrap();
    // Original dream reads a source, performs inference, and writes the result.
    let sources = wiki
        .snapshot_dream_sources(
            store.reader.dream_heads_bounded(ws, proj, 1).await.unwrap(),
            6000,
        )
        .await
        .unwrap();
    wiki.write_page(request("concurrent exception\n"))
        .await
        .unwrap();
    let result = wiki
        .apply_dream_batch(
            vec![request("model draft from original fact\n")],
            &sources,
            std::sync::Arc::new(|| false),
        )
        .await;
    assert!(
        result.is_err(),
        "a draft derived from a stale source must be refused"
    );
    assert_eq!(
        wiki.read_page(ws, proj, &PagePath::new("a.md").unwrap())
            .unwrap()
            .body,
        "concurrent exception\n"
    );
}
