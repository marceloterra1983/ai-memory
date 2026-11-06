//! Conditional writes must protect the source file as well as the SQL head.
use ai_memory_core::{PagePath, Tier};
use ai_memory_store::{PagePrecondition, Store};
use ai_memory_wiki::{ConditionalWrite, Wiki, WritePageRequest};
use serde_json::json;

async fn fixture() -> (tempfile::TempDir, Store, Wiki, WritePageRequest) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("synthetic")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "contract", None)
        .await
        .unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone())
        .unwrap()
        .with_store_reader(store.reader.clone());
    let req = WritePageRequest {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new("notes/control.md").unwrap(),
        title: None,
        body: "original".into(),
        tier: Tier::Semantic,
        pinned: false,
        frontmatter: json!({}),
        actor: Default::default(),
        admission_ctx: None,
        author_id: None,
        evidence: vec![],
    };
    (tmp, store, wiki, req)
}

#[tokio::test]
async fn unindexed_file_is_never_overwritten_by_create_only() {
    let (_tmp, _store, wiki, req) = fixture().await;
    let abs = wiki.abs_path(req.workspace_id, req.project_id, &req.path);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, "unindexed source").unwrap();
    assert!(matches!(
        wiki.write_page_if(req.clone(), PagePrecondition::Absent)
            .await
            .unwrap(),
        ConditionalWrite::Mismatch { current: None }
    ));
    assert_eq!(std::fs::read_to_string(abs).unwrap(), "unindexed source");
    // A different absent path remains a legitimate create.
    let mut control = req;
    control.path = PagePath::new("notes/new.md").unwrap();
    assert!(matches!(
        wiki.write_page_if(control, PagePrecondition::Absent)
            .await
            .unwrap(),
        ConditionalWrite::Written(_)
    ));
}

#[tokio::test]
async fn edited_tail_refuses_expected_version_without_truncated_comparison() {
    let (_tmp, _store, wiki, mut req) = fixture().await;
    req.body = "x".repeat(70_000);
    let id = wiki.write_page(req.clone()).await.unwrap();
    let abs = wiki.abs_path(req.workspace_id, req.project_id, &req.path);
    let mut md = wiki
        .read_page(req.workspace_id, req.project_id, &req.path)
        .unwrap();
    md.body.push_str("external tail");
    std::fs::write(&abs, ai_memory_wiki::emit(&md).unwrap()).unwrap();
    let before = std::fs::read(&abs).unwrap();
    req.body = "replacement".into();
    assert!(
        matches!(wiki.write_page_if(req, PagePrecondition::Latest(id)).await.unwrap(),
        ConditionalWrite::Mismatch { current: Some(current) } if current == id)
    );
    assert_eq!(std::fs::read(abs).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_wiki_waits_for_the_process_file_lock() {
    let (tmp, store, wiki, req) = fixture().await;
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::new()
        .chain_update(req.workspace_id.as_bytes())
        .chain_update([0])
        .chain_update(req.project_id.as_bytes())
        .chain_update([0])
        .chain_update(req.path.as_str().as_bytes())
        .finalize();
    let lock_path = tmp.path().join("page-locks").join(format!(
        "pool-v1-{:04}",
        u16::from_be_bytes([digest[0], digest[1]]) % 1024
    ));
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(lock_path)
        .unwrap();
    fs2::FileExt::lock_exclusive(&file).unwrap();
    let other = Wiki::new(tmp.path(), store.writer.clone())
        .unwrap()
        .with_store_reader(store.reader.clone());
    let task = tokio::spawn(async move { other.write_page(req).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let blocked = !task.is_finished();
    fs2::FileExt::unlock(&file).unwrap();
    assert!(
        blocked,
        "the independent Wiki wrote while another process held its file lock"
    );
    task.await.unwrap().unwrap();
    drop(wiki);
}

#[tokio::test]
async fn distinct_page_prefixes_and_long_names_have_independent_locks() {
    let (_tmp, _store, wiki, mut req) = fixture().await;
    for path in [
        "foo.md".to_string(),
        "foo.md.lock/bar.md".to_string(),
        format!("{}.md", "x".repeat(250)),
    ] {
        req.path = PagePath::new(path).unwrap();
        wiki.write_page(req.clone()).await.unwrap();
    }
}

#[tokio::test]
async fn bare_wiki_verifies_source_through_the_writer() {
    let (tmp, store, _wiki, req) = fixture().await;
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).unwrap();
    let id = match wiki
        .write_page_if(req.clone(), PagePrecondition::Absent)
        .await
        .unwrap()
    {
        ConditionalWrite::Written(id) => id,
        other => panic!("{other:?}"),
    };
    let abs = wiki.abs_path(req.workspace_id, req.project_id, &req.path);
    std::fs::write(&abs, "external source").unwrap();
    assert!(
        matches!(wiki.write_page_if(req, PagePrecondition::Latest(id)).await.unwrap(), ConditionalWrite::Mismatch { current: Some(current) } if current == id)
    );
    assert_eq!(std::fs::read_to_string(abs).unwrap(), "external source");
}

#[tokio::test]
async fn missing_page_reads_use_a_bounded_file_lock_pool() {
    let (tmp, store, wiki, req) = fixture().await;
    let legacy = tmp.path().join("page-locks").join("a".repeat(64));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"legacy lock stays inert").unwrap();
    for index in 0..1100 {
        let path = PagePath::new(format!("missing/{index}.md")).unwrap();
        assert!(
            wiki.read_page_version(req.workspace_id, req.project_id, &path, &store.reader)
                .await
                .unwrap()
                .is_none()
        );
    }
    let files = std::fs::read_dir(tmp.path().join("page-locks"))
        .unwrap()
        .count();
    assert!(
        files <= 1025,
        "1100 distinct reads created {files} files; pool is bounded to 1024 plus one legacy file"
    );
    assert_eq!(std::fs::read(legacy).unwrap(), b"legacy lock stays inert");
}
