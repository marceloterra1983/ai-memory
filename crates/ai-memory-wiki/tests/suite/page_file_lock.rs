//! Two stores and two processes must not let a losing page write restore its
//! snapshot over the winner. Jev does not apply: the decision is file
//! existence, UUID equality, and an OS file lock.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ai_memory_core::{
    ActorContext, PageEvidence, PageEvidenceKind, PagePath, ProjectId, Tier, WorkspaceId,
};
use ai_memory_store::{PageWriteConflictReason, PageWriteGuard, Store, StoreError};
use ai_memory_wiki::{Wiki, WikiError, WritePageRequest};
use fs2::FileExt;
use rusqlite::OpenFlags;

fn request(
    ws: WorkspaceId,
    proj: ProjectId,
    path: &str,
    body: &str,
    evidence: &str,
) -> WritePageRequest {
    WritePageRequest {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new(path).unwrap(),
        body: body.into(),
        frontmatter: serde_json::json!({ "title": path }),
        tier: Tier::Semantic,
        pinned: false,
        title: None,
        admission_ctx: None,
        author_id: None,
        actor: ActorContext::anonymous(),
        evidence: vec![PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: evidence.into(),
        }],
    }
}

fn count(store: &Store, sql: &str, params: &[&dyn rusqlite::ToSql]) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(store.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("readonly sqlite");
    conn.query_row(sql, params, |row| row.get(0)).expect(sql)
}

/// Break: the loser of two `Store::open` handles restores a stale snapshot
/// over the winner's bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_stores_keep_one_create_only_winner_and_do_not_roll_back_over_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store_a = Store::open(tmp.path()).unwrap();
    let store_b = Store::open(tmp.path()).unwrap();
    let ws = store_a
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = store_a
        .writer
        .get_or_create_project(ws, "scratch", None)
        .await
        .unwrap();
    let wiki_a = Wiki::new(tmp.path(), store_a.writer.clone()).unwrap();
    let wiki_b = Wiki::new(tmp.path(), store_b.writer.clone()).unwrap();

    for n in 0..8 {
        let path = format!("notes/race-{n}.md");
        let left = request(
            ws,
            proj,
            &path,
            &format!("# Race\n\nWINALPHA {n} [[notes/alpha-link-{n}.md]]"),
            &format!("alpha-{n}"),
        );
        let right = request(
            ws,
            proj,
            &path,
            &format!("# Race\n\nWINBETA {n} [[notes/beta-link-{n}.md]]"),
            &format!("beta-{n}"),
        );
        let (left_res, right_res) = tokio::join!(
            wiki_a.write_page_guarded(left, PageWriteGuard::CreateOnly),
            wiki_b.write_page_guarded(right, PageWriteGuard::CreateOnly),
        );
        let (winner, loser) = match (left_res, right_res) {
            (Ok(id), Err(err)) => (id, err),
            (Err(err), Ok(id)) => (id, err),
            (Ok(a), Ok(b)) => panic!("both create_only writes landed: {a} and {b}"),
            (Err(a), Err(b)) => panic!("both create_only writes lost: {a} | {b}"),
        };
        let WikiError::Store(StoreError::PageWriteConflict {
            reason,
            current_page_id,
        }) = loser
        else {
            panic!("loser was not a page write conflict: {loser}");
        };
        assert_eq!(reason, PageWriteConflictReason::AlreadyExists);
        assert_eq!(current_page_id, Some(winner));

        let file =
            std::fs::read_to_string(wiki_a.abs_path(ws, proj, &PagePath::new(&path).unwrap()))
                .unwrap_or_else(|e| panic!("winner file missing after rollback: {e}"));
        let body = store_a
            .reader
            .page_body_by_ids(ws, proj, &path)
            .await
            .unwrap()
            .unwrap()
            .body;
        let winner_token = if body.contains("WINALPHA") {
            "WINALPHA"
        } else if body.contains("WINBETA") {
            "WINBETA"
        } else {
            panic!("db body is neither writer: {body}");
        };
        let loser_token = if winner_token == "WINALPHA" {
            "WINBETA"
        } else {
            "WINALPHA"
        };
        assert!(file.contains(winner_token), "file lost the winner: {file}");
        assert!(
            !file.contains(loser_token),
            "loser bytes survived on the winner: {file}"
        );
        assert!(
            body.contains(winner_token) && !body.contains(loser_token),
            "{body}"
        );
        assert_eq!(
            count(
                &store_a,
                "SELECT COUNT(*) FROM pages WHERE workspace_id=?1 AND project_id=?2 AND path=?3",
                &[&ws.as_bytes(), &proj.as_bytes(), &path.as_str()],
            ),
            1,
            "iteration {n}"
        );
        assert_eq!(
            count(
                &store_a,
                "SELECT COUNT(*) FROM page_evidence WHERE page_id IN (\
                    SELECT id FROM pages WHERE workspace_id=?1 AND project_id=?2 AND path=?3)",
                &[&ws.as_bytes(), &proj.as_bytes(), &path.as_str()],
            ),
            1,
            "loser evidence must not land, iteration {n}"
        );
        let loser_link = format!(
            "notes/{}-link-{n}.md",
            if winner_token == "WINALPHA" {
                "beta"
            } else {
                "alpha"
            }
        );
        assert_eq!(
            count(
                &store_a,
                "SELECT COUNT(*) FROM links WHERE to_path=?1",
                &[&loser_link.as_str()],
            ),
            0,
            "iteration {n}"
        );
    }
}

fn hold_page_lock_until_parent_closes_stdin() {
    let path = PathBuf::from(std::env::var("AI_MEMORY_PAGE_LOCK_PATH").expect("lock path"));
    let ready = PathBuf::from(std::env::var("AI_MEMORY_PAGE_LOCK_READY").expect("ready path"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    file.lock_exclusive().expect("child flock");
    std::fs::write(&ready, b"ready").unwrap();
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 16];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) | Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => panic!("child stdin: {err}"),
        }
    }
    drop(file);
}

/// Break: the page critical section is only a handle-local mutex, so a
/// process that holds the page's OS file lock does not delay the write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_process_page_flock_blocks_the_writer_until_release() {
    if std::env::var("AI_MEMORY_PAGE_LOCK_CHILD").is_ok() {
        hold_page_lock_until_parent_closes_stdin();
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "scratch", None)
        .await
        .unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).unwrap();
    let page = PagePath::new("notes/locked.md").unwrap();
    let lock_path = crate::wiki::page_file_lock_path(tmp.path(), ws, proj, &page);
    let ready = tmp.path().join("child-ready");
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .arg(
            "integration::page_file_lock::cross_process_page_flock_blocks_the_writer_until_release",
        )
        .arg("--exact")
        .env("AI_MEMORY_PAGE_LOCK_CHILD", "1")
        .env("AI_MEMORY_PAGE_LOCK_PATH", &lock_path)
        .env("AI_MEMORY_PAGE_LOCK_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lock holder");
    let mut stdin = child.stdin.take().expect("child stdin");

    let started = Instant::now();
    while !ready.exists() {
        if started.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            let output = child.wait_with_output().expect("child output");
            panic!(
                "child did not lock: status {:?} stdout {} stderr {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(
        probe.try_lock_exclusive().is_err(),
        "child lock is not a real OS flock"
    );

    let writer = wiki.clone();
    let write_path = page.clone();
    let mut write = tokio::spawn(async move {
        writer
            .write_page(request(
                ws,
                proj,
                write_path.as_str(),
                "# Locked\n\nSHOULDWAIT [[notes/locked-link.md]]",
                "held-lock",
            ))
            .await
    });
    let blocked = tokio::time::timeout(Duration::from_secs(2), &mut write).await;
    assert!(
        blocked.is_err(),
        "write finished while another process held the page flock: {blocked:?}"
    );
    assert!(
        probe.try_lock_exclusive().is_err(),
        "page flock vanished while the write was still blocked"
    );
    let page_file = wiki.abs_path(ws, proj, &page);
    assert!(
        !page_file.exists(),
        "page bytes landed while the other process held the lock"
    );

    stdin.write_all(b"\n").unwrap();
    drop(stdin);
    let id = tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .expect("write stayed blocked after the flock was released")
        .expect("write task")
        .expect("write after release");
    let text = std::fs::read_to_string(&page_file).unwrap();
    assert!(text.contains("SHOULDWAIT"), "{text}");
    let head = store
        .reader
        .latest_head_by_ids(ws, proj, page.as_str())
        .await
        .unwrap()
        .expect("head");
    assert_eq!(head.id, id);
    let status = child.wait().expect("child exit");
    assert!(status.success(), "lock holder failed: {status}");
}
