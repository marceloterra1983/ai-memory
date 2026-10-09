//! Adversarial single-writer compare-and-swap and bounded dream reads.

use ai_memory_core::{NewPage, PagePath, ProjectId, Tier, WorkspaceId};
use ai_memory_store::Store;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn page(ws: WorkspaceId, proj: ProjectId, path: &str, body: &str) -> NewPage {
    NewPage {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new(path).unwrap(),
        title: path.into(),
        body: body.into(),
        tier: Tier::Episodic,
        frontmatter_json: serde_json::json!({}),
        pinned: false,
        links: vec![],
        author_id: None,
        expires_at: None,
        entities: vec![],
        evidence: vec![],
    }
}

async fn scope(store: &Store) -> (WorkspaceId, ProjectId) {
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
    (ws, proj)
}

#[tokio::test]
async fn dream_heads_reject_cap_plus_one_without_sampling_and_isolate_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    store
        .writer
        .upsert_pages_batch(vec![
            page(ws, proj, "a.md", "a"),
            page(ws, proj, "b.md", "b"),
            page(ws, proj, "c.md", "c"),
        ])
        .await
        .unwrap();
    assert!(store.reader.dream_heads_bounded(ws, proj, 2).await.is_err());
    assert_eq!(
        store
            .reader
            .dream_heads_bounded(ws, proj, 3)
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(
        store
            .reader
            .dream_heads_bounded(WorkspaceId::new(), proj, 3)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn dream_sql_cas_refuses_stale_and_foreign_sources_and_rolls_back_all_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    store
        .writer
        .upsert_pages_batch(vec![
            page(ws, proj, "a.md", "source a"),
            page(ws, proj, "b.md", "source b"),
        ])
        .await
        .unwrap();
    let heads = store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap();
    store
        .writer
        .upsert_page(page(ws, proj, "b.md", "concurrent exception"))
        .await
        .unwrap();
    let requests = vec![
        page(ws, proj, "a.md", "merge a"),
        page(ws, proj, "b.md", "merge b"),
    ];
    assert!(
        store
            .writer
            .upsert_dream_pages_batch(requests.clone(), heads, Arc::new(|| false))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "a.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "source a"
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "b.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "concurrent exception"
    );
    let fresh = store.reader.dream_heads_bounded(ws, proj, 2).await.unwrap();
    let mut foreign = fresh.clone();
    foreign[0].workspace_id = WorkspaceId::new();
    assert!(
        store
            .writer
            .upsert_dream_pages_batch(requests.clone(), foreign, Arc::new(|| false))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .writer
            .upsert_dream_pages_batch(requests, fresh, Arc::new(|| false))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn dream_sql_cancel_at_commit_rolls_back_even_after_upserts() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    store
        .writer
        .upsert_page(page(ws, proj, "a.md", "original exception"))
        .await
        .unwrap();
    let heads = store.reader.dream_heads_bounded(ws, proj, 1).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    assert!(
        store
            .writer
            .upsert_dream_pages_batch(
                vec![page(ws, proj, "a.md", "candidate")],
                heads,
                Arc::new(move || seen.fetch_add(1, Ordering::SeqCst) > 0)
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "a.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "original exception"
    );
    assert!(calls.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn dream_sql_cas_refuses_access_change_with_unchanged_id_and_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    let id = store
        .writer
        .upsert_page(page(ws, proj, "a.md", "original"))
        .await
        .unwrap();
    let heads = store.reader.dream_heads_bounded(ws, proj, 1).await.unwrap();
    store
        .writer
        .bump_access_for_actor(
            vec![id],
            Some(ai_memory_core::IdentityKey::User("other-operator".into())),
        )
        .await
        .unwrap();
    assert!(
        store
            .writer
            .upsert_dream_pages_batch(
                vec![page(ws, proj, "a.md", "candidate")],
                heads,
                Arc::new(|| false)
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "a.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "original"
    );
    let fresh = store.reader.dream_heads_bounded(ws, proj, 1).await.unwrap();
    assert_eq!(fresh[0].candidate.id, id);
    assert_eq!(fresh[0].access_breadth, 1);
    store
        .writer
        .upsert_dream_pages_batch(
            vec![page(ws, proj, "a.md", "candidate")],
            fresh,
            Arc::new(|| false),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn dream_sql_cas_refuses_expired_source_even_when_snapshot_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    let mut input = page(ws, proj, "a.md", "expiring fact");
    input.expires_at = Some(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1));
    store.writer.upsert_page(input).await.unwrap();
    let heads = store.reader.dream_heads_bounded(ws, proj, 1).await.unwrap();
    assert!(
        store
            .writer
            .upsert_dream_pages_batch(
                vec![page(ws, proj, "a.md", "candidate")],
                heads,
                Arc::new(|| false)
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .reader
            .page_body_by_ids(ws, proj, "a.md")
            .await
            .unwrap()
            .unwrap()
            .body,
        "expiring fact"
    );
}

#[tokio::test]
async fn dream_vector_read_refuses_cap_plus_one_and_malformed_length() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    let ids = store
        .writer
        .upsert_pages_batch(vec![
            page(ws, proj, "a.md", "a"),
            page(ws, proj, "b.md", "b"),
        ])
        .await
        .unwrap();
    for id in &ids {
        store
            .writer
            .store_embedding(
                *id,
                vec![0_u8; 8],
                "synthetic".into(),
                "synthetic".into(),
                2,
            )
            .await
            .unwrap();
    }
    assert!(
        store
            .reader
            .load_embeddings_bounded(ws, proj, "synthetic".into(), "synthetic".into(), 2, 1)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .reader
            .load_embeddings_bounded(ws, proj, "synthetic".into(), "synthetic".into(), 2, 2)
            .await
            .unwrap()
            .len(),
        2
    );
    store
        .writer
        .store_embedding(
            ids[0],
            vec![0_u8; 9],
            "synthetic".into(),
            "synthetic".into(),
            2,
        )
        .await
        .unwrap();
    assert!(
        store
            .reader
            .load_embeddings_bounded(ws, proj, "synthetic".into(), "synthetic".into(), 2, 2)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dream_vector_dimension_cap_refuses_oversized_blob_with_boundary_control() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (ws, proj) = scope(&store).await;
    let id = store
        .writer
        .upsert_page(page(ws, proj, "a.md", "a"))
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            id,
            vec![0_u8; 8193 * 4],
            "synthetic".into(),
            "synthetic".into(),
            8193,
        )
        .await
        .unwrap();
    assert!(
        store
            .reader
            .load_embeddings_bounded(ws, proj, "synthetic".into(), "synthetic".into(), 8193, 1)
            .await
            .is_err(),
        "an exact-length blob beyond the dimension ceiling must refuse before materialization"
    );
    store
        .writer
        .store_embedding(
            id,
            vec![0_u8; 8192 * 4],
            "synthetic".into(),
            "synthetic".into(),
            8192,
        )
        .await
        .unwrap();
    let vectors = store
        .reader
        .load_embeddings_bounded(ws, proj, "synthetic".into(), "synthetic".into(), 8192, 1)
        .await
        .unwrap();
    assert_eq!(vectors.len(), 1);
    assert_eq!(vectors[0].vector.len(), 8192);
}
