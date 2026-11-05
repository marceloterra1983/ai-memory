//! The writer's conditional metadata update, independently of the wiki precheck.
use ai_memory_core::{
    NewPage, NewUser, PageEvidence, PageEvidenceKind, PagePath, ProjectId, Tier, UserRole,
    WorkspaceId,
};
use ai_memory_store::Store;
use sha2::{Digest, Sha256};

fn page(ws: WorkspaceId, proj: ProjectId) -> NewPage {
    NewPage {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new("notes/metadata.md").unwrap(),
        title: "Synthetic attribution".into(),
        body: "Unchanged synthetic facts.\r\n".into(),
        tier: Tier::Procedural,
        frontmatter_json: serde_json::json!({
            "relations": {"fixes": ["None", "Free title"]},
            "tags": ["synthetic"], "custom": {"nested": [1, true, null]}
        }),
        pinned: true,
        links: Vec::new(),
        author_id: None,
        expires_at: Some("2099-01-01T00:00:00Z".parse().unwrap()),
        entities: vec!["synthetic".into()],
        evidence: vec![
            PageEvidence {
                kind: PageEvidenceKind::Session,
                source_id: "synthetic-session".into(),
            },
            PageEvidence {
                kind: PageEvidenceKind::Observation,
                source_id: "synthetic-observation".into(),
            },
        ],
    }
}

async fn fixture() -> (tempfile::TempDir, Store, NewPage) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("synthetic")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "metadata", None)
        .await
        .unwrap();
    (tmp, store, page(ws, proj))
}

#[tokio::test]
async fn metadata_cas_refuses_stale_foreign_and_changed_body_without_altering_heads() {
    let (_tmp, store, source) = fixture().await;
    let hash: [u8; 32] = Sha256::digest(source.body.as_bytes()).into();
    let first = store.writer.upsert_page(source.clone()).await.unwrap();
    let mut concurrent = source.clone();
    concurrent.frontmatter_json["custom"]["concurrent"] = serde_json::json!(true);
    let latest = store.writer.upsert_page(concurrent.clone()).await.unwrap();
    let foreign_proj = store
        .writer
        .get_or_create_project(source.workspace_id, "foreign", None)
        .await
        .unwrap();
    let mut foreign = source.clone();
    foreign.project_id = foreign_proj;
    let foreign_id = store.writer.upsert_page(foreign).await.unwrap();
    let mut patch = concurrent.clone();
    patch.frontmatter_json["relations"]["fixes"] = serde_json::json!(["Free title"]);
    let mut changed_body = patch.clone();
    changed_body.body = "Unauthorized body rewrite.".into();
    let changed_hash: [u8; 32] = Sha256::digest(changed_body.body.as_bytes()).into();

    for (candidate, head, expected_hash) in [
        (patch.clone(), first, hash),
        (patch.clone(), foreign_id, hash),
        (patch.clone(), latest, [0; 32]),
        (changed_body.clone(), latest, hash),
        (changed_body, latest, changed_hash),
    ] {
        assert_eq!(
            store
                .writer
                .upsert_page_metadata_if_latest(candidate, head, expected_hash)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .reader
                .latest_page_id_by_ids(
                    source.workspace_id,
                    source.project_id,
                    source.path.to_string()
                )
                .await
                .unwrap(),
            Some(latest)
        );
    }
    let stored = store
        .reader
        .page_body_by_ids(source.workspace_id, source.project_id, source.path.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.body, source.body);
    let fm: serde_json::Value = serde_json::from_str(&stored.frontmatter_json).unwrap();
    assert_eq!(
        fm["relations"]["fixes"],
        serde_json::json!(["None", "Free title"])
    );
    assert_eq!(
        store
            .reader
            .latest_page_id_by_ids(source.workspace_id, foreign_proj, source.path.to_string())
            .await
            .unwrap(),
        Some(foreign_id)
    );
    // A blanket refusal cannot satisfy the adversarial cases above.
    assert!(
        store
            .writer
            .upsert_page_metadata_if_latest(patch, latest, hash)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn metadata_cas_preserves_predecessor_attribution_and_all_evidence() {
    let (_tmp, store, mut source) = fixture().await;
    let author = store
        .writer
        .create_human_user(
            NewUser {
                username: "synthetic-author".into(),
                name: None,
                email: None,
            },
            UserRole::User,
            None,
            false,
        )
        .await
        .unwrap();
    source.author_id = Some(author);
    let hash: [u8; 32] = Sha256::digest(source.body.as_bytes()).into();
    let first = store.writer.upsert_page(source.clone()).await.unwrap();
    let mut patch = source.clone();
    patch.frontmatter_json["relations"]["fixes"] = serde_json::json!(["Free title"]);
    patch.author_id = None;
    patch.evidence.clear();
    // Older imports may have an indexed TTL without a frontmatter TTL key.
    patch.expires_at = None;
    let next = store
        .writer
        .upsert_page_metadata_if_latest(patch, first, hash)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first, next);
    store.reader.with_conn(move |conn| {
        let (actual_author, predecessor, pinned, expiry): (Vec<u8>, Vec<u8>, bool, Option<i64>) = conn.query_row(
            "SELECT author_id,supersedes,pinned,expires_at FROM pages WHERE id=?1 AND is_latest=1",
            [next.as_bytes()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        )?;
            assert_eq!(actual_author, author.as_bytes());
        assert_eq!(predecessor, first.as_bytes());
        assert!(pinned);
        assert_eq!(expiry, source.expires_at.map(|v| v.as_microsecond()));
        let evidence = |id: ai_memory_core::PageId| -> rusqlite::Result<Vec<(String,String,i64)>> {
            conn.prepare("SELECT source_kind,source_id,created_at FROM page_evidence WHERE page_id=?1 ORDER BY source_kind,source_id")?
                .query_map([id.as_bytes()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect()
        };
        let old_evidence = evidence(first)?;
        assert_eq!(old_evidence.len(), 2);
        assert_eq!(evidence(next)?, old_evidence);
        let old_latest: bool = conn.query_row("SELECT is_latest FROM pages WHERE id=?1", [first.as_bytes()], |r|r.get(0))?;
        assert!(!old_latest, "supersession retains the predecessor");
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn metadata_cas_inherits_body_and_only_unchanged_abstract_vectors() {
    let (_tmp, store, mut source) = fixture().await;
    source.frontmatter_json["abstract"] = serde_json::json!("Synthetic unchanged abstract.");
    let hash: [u8; 32] = Sha256::digest(source.body.as_bytes()).into();
    let first = store.writer.upsert_page(source.clone()).await.unwrap();
    let vector = || ai_memory_store::EmbeddingWrite {
        page_id: first,
        vector_bytes: ai_memory_store::f32_vec_to_bytes(&[1.0, 0.0]),
        provider: "synthetic".into(),
        model: "synthetic-model".into(),
        dim: 2,
    };
    store.writer.store_embeddings(vec![vector()]).await.unwrap();
    store
        .writer
        .store_abstract_embeddings(vec![vector()])
        .await
        .unwrap();
    source.frontmatter_json["relations"]["fixes"] = serde_json::json!(["Free title"]);
    let next = store
        .writer
        .upsert_page_metadata_if_latest(source.clone(), first, hash)
        .await
        .unwrap()
        .unwrap();
    store
        .reader
        .with_conn(move |c| {
            for table in ["page_embeddings", "page_abstract_embeddings"] {
                let sql = format!(
                    "SELECT vector,provider,model,dim,created_at FROM {table} WHERE page_id=?1"
                );
                type StoredVector = (Vec<u8>, String, String, u32, i64);
                let rows = |id: ai_memory_core::PageId| -> rusqlite::Result<StoredVector> {
                    c.query_row(&sql, [id.as_bytes()], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })
                };
                assert_eq!(rows(next)?, rows(first)?);
            }
            Ok(())
        })
        .await
        .unwrap();
    // A generic metadata caller changing its abstract must not inherit that vector.
    source.frontmatter_json["abstract"] = serde_json::json!("Changed synthetic abstract.");
    let changed = store
        .writer
        .upsert_page_metadata_if_latest(source, next, hash)
        .await
        .unwrap()
        .unwrap();
    store
        .reader
        .with_conn(move |c| {
            let count = |table: &str| -> rusqlite::Result<usize> {
                c.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE page_id=?1"),
                    [changed.as_bytes()],
                    |r| r.get(0),
                )
            };
            assert_eq!(count("page_abstract_embeddings")?, 0);
            assert_eq!(count("page_embeddings")?, 1);
            Ok(())
        })
        .await
        .unwrap();
}
