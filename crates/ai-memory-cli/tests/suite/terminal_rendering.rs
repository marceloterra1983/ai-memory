//! Untrusted API text must be inert when the CLI writes it to a terminal.

use std::path::Path;
use std::time::Duration;

use axum::{Json, Router, routing::any};
use serde_json::{Value, json};

struct ApiFixture {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl ApiFixture {
    async fn start(path: &str, body: Value) -> Self {
        let app = Router::new().route(
            path,
            any(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
        Self::start_app(app).await
    }

    async fn start_app(app: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, task }
    }
}

impl Drop for ApiFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn cli_output(root: &Path, url: &str, args: &[&str]) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ai-memory"));
    command
        .env_clear()
        .current_dir(root)
        .env("AI_MEMORY_HOME", root.join("home"))
        .env("AI_MEMORY_DATA_DIR", root.join("data"))
        .env("AI_MEMORY_SERVER_URL", url)
        .env("AI_MEMORY_AUTH_TOKEN", "fixture-token")
        .env("NO_PROXY", "*")
        .env("RUST_LOG", "off")
        .arg("--config")
        .arg(root.join("config.toml"))
        .args(args)
        .kill_on_drop(true);
    if args.first() == Some(&"hook") {
        // Exercise native context delivery without capture or detached backfill.
        command
            .env("AI_MEMORY_CAPTURE_OWNER", "fixture")
            .stdin(std::process::Stdio::piped());
    }
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = async {
        let mut child = command.spawn()?;
        if args.first() == Some(&"hook") {
            use tokio::io::AsyncWriteExt;
            child.stdin.take().unwrap().write_all(b"{}").await?;
        }
        child.wait_with_output().await
    };
    tokio::time::timeout(Duration::from_secs(10), output)
        .await
        .unwrap()
        .unwrap()
}

async fn cli(root: &Path, url: &str, args: &[&str]) -> String {
    let output = cli_output(root, url, args).await;
    assert!(
        output.status.success(),
        "CLI failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn terminal_grants_neutralize_controls_and_preserve_json() {
    let root = tempfile::tempdir().unwrap();
    let rows = json!([
        {"workspace":"team\u{1b}[2J", "project":"private\nforged", "username":"evil\tadmin\u{7}", "level":"read\rwrite"},
        {"workspace":"default", "project":"café", "username":"alice", "level":"read"}
    ]);
    let server = ApiFixture::start("/admin/projects/grants", json!({"grants": rows})).await;
    let rendered = cli(root.path(), &server.url, &["user", "grants"]).await;
    assert!(
        rendered.chars().all(|c| c == '\n' || !c.is_control()),
        "terminal controls escaped the grant renderer: {rendered:?}"
    );
    assert_eq!(
        rendered.lines().count(),
        3,
        "an untrusted name cannot forge a table row"
    );
    assert!(rendered.contains("team?[2J/private?forged"), "{rendered:?}");
    assert!(rendered.contains("evil?admin?"), "{rendered:?}");
    assert!(rendered.contains("read?write"), "{rendered:?}");
    assert!(rendered.contains("default/café") && rendered.contains("alice"));
    let json_output = cli(root.path(), &server.url, &["user", "grants", "--json"]).await;
    assert_eq!(
        serde_json::from_str::<Value>(&json_output).unwrap(),
        rows,
        "JSON must preserve the original structured data"
    );
}

#[tokio::test]
async fn terminal_popped_message_neutralizes_controls_and_preserves_json() {
    let root = tempfile::tempdir().unwrap();
    let body = "before\u{1b}[31mred\r\nline\t世界\u{7}after";
    let message = json!({
        "id":"msg\u{1b}[2J", "to_workspace_id":"target", "to_project_id":"project",
        "from_workspace_id":"source\rspoof", "from_project_id":"project\u{9b}2J",
        "from_agent":"agent\tspoof", "from_owner_user":"owner\nforged",
        "subject":"subject\nforged\u{1b}]52;c;dummy\u{7}", "body":body,
        "state":"claimed", "claimed_at":null, "created_at":"today\u{8}spoof"
    });
    let server = ApiFixture::start(
        "/admin/messages/pop",
        json!({"message":message,"security_notice":"NOTICE\u{1b}[31m\r\ncontinued"}),
    )
    .await;
    let args = [
        "message",
        "pop",
        "--workspace",
        "ws",
        "--project",
        "project",
    ];
    let rendered = cli(root.path(), &server.url, &args).await;
    assert!(
        rendered
            .chars()
            .all(|c| c == '\n' || c == '\t' || !c.is_control()),
        "terminal controls escaped the message renderer: {rendered:?}"
    );
    assert!(
        rendered.contains("before?[31mred?\nline\t世界?after"),
        "{rendered:?}"
    );
    for expected in [
        "msg?[2J",
        "source?spoof",
        "project?2J",
        "agent?spoof",
        "owner?forged",
        "subject?forged?]52;c;dummy?",
        "today?spoof",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in {rendered:?}"
        );
    }
    assert!(rendered.contains("NOTICE") && rendered.contains("---- end of body ----"));
    let mut json_args = args.to_vec();
    json_args.push("--json");
    let json_output = cli(root.path(), &server.url, &json_args).await;
    assert_eq!(
        serde_json::from_str::<Value>(&json_output).unwrap()["message"],
        message,
        "JSON must preserve the original structured data"
    );
}

#[tokio::test]
async fn terminal_message_list_neutralizes_every_server_field_in_both_mailboxes() {
    let root = tempfile::tempdir().unwrap();
    let message = json!({
        "id":"id\n    id: forged\u{1b}]52;c;dummy\u{7}",
        "to_workspace_id":"to\rws", "to_project_id":"to\nproject",
        "from_workspace_id":"from\u{1b}[2J", "from_project_id":"from\tproject",
        "from_agent":"agent", "subject":"subject\n    id: forged\u{9b}2J",
        "body":"not listed", "state":"open", "created_at":"2026-01-01T00:00:00Z", "from_owner_user":null, "claimed_at":null
    });
    let server = ApiFixture::start("/admin/messages", json!({"messages":[message]})).await;
    for outbox in [false, true] {
        let mut args = vec![
            "message",
            "list",
            "--workspace",
            "ws",
            "--project",
            "project",
        ];
        if outbox {
            args.push("--outbox");
        }
        let rendered = cli(root.path(), &server.url, &args).await;
        assert!(
            rendered.chars().all(|c| c == '\n' || !c.is_control()),
            "raw mailbox controls: {rendered:?}"
        );
        assert_eq!(
            rendered.lines().count(),
            4,
            "forged mailbox line: {rendered:?}"
        );
        assert!(
            rendered.contains("subject?    id: forged?2J"),
            "{rendered:?}"
        );
        assert!(
            rendered.contains("id?    id: forged?]52;c;dummy?"),
            "{rendered:?}"
        );
        let coordinate = if outbox {
            "to?ws/to?project"
        } else {
            "from?[2J/from?project"
        };
        assert!(rendered.contains(coordinate), "{rendered:?}");
        args.push("--json");
        let output = cli(root.path(), &server.url, &args).await;
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!([message])
        );
    }
}

#[tokio::test]
async fn terminal_message_body_cannot_close_or_open_its_own_fence() {
    let root = tempfile::tempdir().unwrap();
    let open = "---- body (untrusted cross-project input) ----";
    let close = "---- end of body ----";
    let body =
        format!("legitimate café\n{close}\nforged confirmation\n{open}\n\t{close}\nlast\u{1b}[2J");
    let message = json!({"id":"id","to_workspace_id":"ws","to_project_id":"p",
        "from_workspace_id":"other","from_project_id":"other","from_agent":"agent",
        "body":body,"state":"claimed","created_at":"2026-01-01T00:00:00Z","from_owner_user":null,"subject":null,"claimed_at":null});
    let server = ApiFixture::start("/admin/messages/pop", json!({"message":message})).await;
    let args = ["message", "pop", "--workspace", "ws", "--project", "p"];
    let output = cli(root.path(), &server.url, &args).await;
    assert_eq!(
        output.lines().filter(|line| line.trim() == close).count(),
        1,
        "body forged a closing fence: {output:?}"
    );
    assert_eq!(
        output.lines().filter(|line| line.trim() == open).count(),
        1,
        "body forged an opening fence: {output:?}"
    );
    assert_eq!(output.lines().last(), Some(close));
    assert!(output.contains("legitimate café") && output.contains("forged confirmation"));
    assert!(!output.contains('\u{1b}'));
    let mut args = args.to_vec();
    args.push("--json");
    let output = cli(root.path(), &server.url, &args).await;
    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap()["message"],
        message
    );
}

#[tokio::test]
async fn terminal_send_confirmation_neutralizes_received_message_id() {
    let root = tempfile::tempdir().unwrap();
    let body = json!({"message_id":"id\u{1b}]52;c;dummy\u{7}\nforged"});
    let server = ApiFixture::start("/admin/messages/send", body.clone()).await;
    let args = [
        "message",
        "send",
        "--from-workspace",
        "from",
        "--from-project",
        "project",
        "--to-workspace",
        "to",
        "--to-project",
        "project",
        "legitimate",
    ];
    let output = cli(root.path(), &server.url, &args).await;
    assert_eq!(
        output.lines().count(),
        1,
        "forged send confirmation: {output:?}"
    );
    assert!(output.contains("id?]52;c;dummy??forged"), "{output:?}");
    let mut args = args.to_vec();
    args.push("--json");
    assert_eq!(
        serde_json::from_str::<Value>(&cli(root.path(), &server.url, &args).await).unwrap(),
        body
    );
}

#[tokio::test]
async fn terminal_grant_confirmation_neutralizes_level_and_previous_in_all_branches() {
    let root = tempfile::tempdir().unwrap();
    for (changed, previous) in [
        (false, None),
        (true, None),
        (true, Some("old\nforged\u{1b}[2J")),
    ] {
        let server = ApiFixture::start(
            "/admin/users/alice/grant",
            json!({"level":"new\nforged\u{1b}[2J", "changed":changed, "previous":previous}),
        )
        .await;
        let output = cli(
            root.path(),
            &server.url,
            &[
                "user",
                "grant",
                "--user",
                "alice",
                "--workspace",
                "ws",
                "--project",
                "p",
                "--level",
                "read",
            ],
        )
        .await;
        assert_eq!(
            output.lines().count(),
            1,
            "forged grant confirmation: {output:?}"
        );
        assert!(output.contains("new?forged?[2J"), "{output:?}");
        if previous.is_some() {
            assert!(output.contains("old?forged?[2J"), "{output:?}");
        }
    }
}

#[tokio::test]
async fn terminal_server_metadata_is_inert_across_admin_commands() {
    let root = tempfile::tempdir().unwrap();
    let bad = "café\nFORGED\u{1b}]52;c;dummy\u{7}\t\r";
    let scope = ["--workspace", "ws", "--project", "p"];
    let mut cases: Vec<(&str, Vec<&str>, Value)> = vec![
        (
            "/admin/handoffs",
            vec!["handoffs"],
            json!({"handoffs":[{"id":bad,"from_agent":bad,"to_agent":bad,"cwd":bad,"created_at_ms":0}]}),
        ),
        (
            "/admin/search",
            vec!["search", "query"],
            json!([{"path":bad,"title":bad,"snippet":bad,"rank":1.0}]),
        ),
        (
            "/admin/read-page",
            vec!["read-page", "--path", "p.md"],
            json!({"path":bad,"title":bad,"body":"legitimate body","frontmatter":{}}),
        ),
        (
            "/admin/write-page",
            vec!["write-page", "--path", "p.md", "--body", "legitimate"],
            json!({"path":bad,"page_id":"id\n\u{1b}[2J"}),
        ),
        (
            "/admin/delete-page",
            vec!["delete-page", "--path", "p.md"],
            json!({"path":bad,"deleted":true}),
        ),
        (
            "/admin/checkpoints",
            vec!["checkpoints"],
            json!([{"oid":"id","short_oid":bad,"time":0,"summary":bad}]),
        ),
        (
            "/admin/commit",
            vec!["commit", "--message", "local"],
            json!({"committed":false,"reason":bad}),
        ),
        (
            "/admin/commit",
            vec!["commit", "--message", "local"],
            json!({"committed":true,"oid":bad}),
        ),
        (
            "/admin/projects/access",
            vec!["project", "access", "--mode", "restricted"],
            json!({"mode":bad,"previous":bad,"changed":true,"without_access":[bad]}),
        ),
        (
            "/admin/projects/access",
            vec!["project", "access", "--mode", "open"],
            json!({"mode":bad,"previous":bad,"changed":false,"without_access":[]}),
        ),
        (
            "/admin/users",
            vec!["user", "list"],
            json!({"users":[{"id":"id","username":bad,"name":bad,"role":bad,"created_at":0}]}),
        ),
        (
            "/admin/api-credentials",
            vec!["api-key", "list"],
            json!({"credentials":[{"id":bad,"user_id":"id","label":bad,"preview":bad,"created_at":0}]}),
        ),
        (
            "/admin/pending-writes",
            vec!["pending-writes", "list"],
            json!([{"id":bad,"status":bad,"operation":bad,"target_path":bad,"kind":"rule","title":"title","confidence":1.0}]),
        ),
        (
            "/admin/reorg",
            vec!["reorg", "--dry-run"],
            json!({"dry_run":true,"plan":[{"session_id":bad,"new_project":bad,"cwd":bad}],"summary":{"sessions_moved":0,"observations_updated":0,"pages_graveyarded":0,"distinct_new_projects":0}}),
        ),
        (
            "/admin/move-project",
            vec![
                "move-project",
                "--from-workspace",
                "ws",
                "--project",
                "p",
                "--to-workspace",
                "to",
                "--confirm",
            ],
            json!({"pages_copied":1,"source_purged":false,"conflicts":[{"path":bad,"moved_to":bad}]}),
        ),
        (
            "/admin/move-session",
            vec![
                "move-session",
                "--from-project",
                "p",
                "--to",
                "to",
                "--confirm",
            ],
            json!({"total":1,"moved":1,"from":{"workspace":bad,"project":bad},"to":{"workspace":bad,"project":bad},"sessions":[{"session_id":bad,"page":bad,"cwd":bad}],"pre_checkpoint":bad,"checkpoint":bad}),
        ),
        (
            "/admin/purge-project",
            vec!["purge-project", "--confirm"],
            json!({"label":bad,"workstream_ids":[bad]}),
        ),
        (
            "/admin/purge-session",
            vec![
                "purge-session",
                "--session-id",
                "0192b6a1-4c2e-7d3f-8a5b-1234567890ab",
                "--confirm",
            ],
            json!({"files_deleted":[bad],"files_failed":[bad]}),
        ),
        (
            "/admin/restore-page",
            vec!["restore-page", "--path", "p.md", "--from", "HEAD"],
            json!({"path":bad,"page_id":"id\n\u{1b}[2J","restored_from":bad,"pre_checkpoint":bad,"checkpoint":bad}),
        ),
        (
            "/admin/audit-contamination",
            vec!["audit-contamination"],
            json!({"summary":{"sessions_misbucketed":1},"findings":[{"check":"session_wrong_bucket","confidence":bad,"entity_kind":"session","entity_id":bad,"landed_workspace":bad,"landed_project":bad,"expected_project":bad,"cwd":bad},{"check":bad,"confidence":bad,"entity_kind":"session","entity_id":bad,"landed_workspace":bad,"landed_project":bad}]}),
        ),
        (
            "/admin/auto-improve",
            vec!["auto-improve", "--session-id", "id"],
            json!({"run_id":bad,"approval_required":true,"approval_policy":bad,"session_id":"id","summary":"summary","warnings":[],"rejected_candidates_count":0,"proposals":[{"id":bad,"status":bad,"page_id":bad,"sidecar_path":bad},{"id":bad,"status":bad,"page_id":null,"sidecar_path":bad}],"skipped":[{"target_path":bad,"reason":bad}]}),
        ),
        (
            "/admin/curator",
            vec!["curator", "--stage"],
            json!({"run_id":bad,"proposal_ids":[bad],"sidecar_paths":[bad],"skipped":[{"target_path":bad,"reason":bad}],"report":{"workspace":"ws","project":"p","generated_at":"2026-01-01T00:00:00Z","dry_run":false,"summary":bad,"params":ai_memory_consolidate::CuratorParams::default(),"findings":[]}}),
        ),
    ];
    for (path, args, _) in &mut cases {
        if !matches!(
            *path,
            "/admin/checkpoints"
                | "/admin/commit"
                | "/admin/users"
                | "/admin/api-credentials"
                | "/admin/reorg"
                | "/admin/move-project"
                | "/admin/move-session"
        ) {
            args.extend(scope);
        }
    }
    // Collect every violation so this test identifies all unprotected renderers.
    let mut failures = Vec::new();
    for (path, args, body) in cases {
        let server = ApiFixture::start(path, body).await;
        let output = cli(root.path(), &server.url, &args).await;
        if output.chars().any(|c| c != '\n' && c.is_control())
            || output.lines().any(|line| line.starts_with("FORGED"))
        {
            failures.push(format!("{args:?}: {output:?}"));
        }
        assert!(
            output.contains("café"),
            "legitimate Unicode disappeared in {args:?}: {output:?}"
        );
    }
    assert!(
        failures.is_empty(),
        "server metadata reached terminal unchanged:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn terminal_status_neutralizes_server_paths_and_provider_health() {
    let root = tempfile::tempdir().unwrap();
    let bad = "café\nFORGED\u{1b}[2J";
    let mut providers = ai_memory_llm::ProviderHealthSnapshot::default();
    providers.llm.status = ai_memory_llm::ProviderHealthStatus::Error;
    providers.llm.provider = Some(bad.into());
    providers.llm.model = Some(bad.into());
    providers.llm.last_error_message = Some(bad.into());
    providers.llm.retry_hint = Some(bad.into());
    providers
        .llm_candidates
        .push(ai_memory_llm::CandidateHealth {
            provider: bad.into(),
            model: bad.into(),
            last_selected: true,
            last_success_at: None,
            last_error_at: None,
            last_error_status: None,
            last_error_class: Some(bad.into()),
            circuit_open_until: None,
        });
    let body = json!({"version":bad,"data_dir":bad,"db_path":bad,"bind":bad,
        "counts":{"pages_latest":0,"pages_all":0,"sessions":0,"observations":0},
        "derived":{"pages_rows":0,"pages_fts_rows":0,"observations_rows":0,"observations_fts_rows":0,"latest_pages_missing_embeddings":0,"embedding_rows":0,"embedding_triples":[{"provider":bad,"model":bad,"dim":3,"count":1}],"links_from_latest_pages":0,"unresolved_links_from_latest_pages":0,"stale_links_from_latest_pages":0,"typed_links_from_latest_pages":[[bad,1]]},
        "providers":providers,"wiki_format":{"okf_migrated":true,"backup_archive":bad,"backup_archive_bytes":1}});
    let server = ApiFixture::start("/admin/status", body).await;
    let output = cli(root.path(), &server.url, &["status"]).await;
    assert!(
        !output.contains('\u{1b}') && !output.lines().any(|line| line.starts_with("FORGED")),
        "forged status: {output:?}"
    );
    assert!(output.contains("café?FORGED?[2J"), "{output:?}");
    let output = cli(root.path(), &server.url, &["status", "--json"]).await;
    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap()["version"],
        bad
    );
}

#[tokio::test]
async fn terminal_document_bodies_preserve_lines_without_terminal_controls() {
    let root = tempfile::tempdir().unwrap();
    let body = "café\n世界\ttext\u{1b}[2J\r\u{7}";
    let server = ApiFixture::start(
        "/admin/read-page",
        json!({"path":"p.md","title":"title","body":body,"frontmatter":{}}),
    )
    .await;
    let output = cli(
        root.path(),
        &server.url,
        &[
            "read-page",
            "--path",
            "p.md",
            "--workspace",
            "ws",
            "--project",
            "p",
        ],
    )
    .await;
    assert!(
        output.contains("café\n世界\ttext?[2J??"),
        "raw document controls: {output:?}"
    );
    let server = ApiFixture::start("/admin/pending-writes/id", json!({"summary":{"id":"id","status":"pending","operation":"write","target_path":"p.md","kind":"rule","title":"title","confidence":1.0},"rationale":body,"body_markdown":body,"events":[]})).await;
    let output = cli(
        root.path(),
        &server.url,
        &[
            "pending-writes",
            "show",
            "id",
            "--workspace",
            "ws",
            "--project",
            "p",
        ],
    )
    .await;
    assert!(
        output
            .chars()
            .all(|c| c == '\n' || c == '\t' || !c.is_control()),
        "raw proposal controls: {output:?}"
    );
}

#[tokio::test]
async fn terminal_credentials_neutralize_stdout_and_stderr_without_changing_json() {
    let root = tempfile::tempdir().unwrap();
    let bad = "café\nFORGED\u{1b}]52;c;dummy\u{7}";
    let user = json!({"id":"id","username":bad,"name":bad,"email":bad,"role":bad,"created_at":0,"last_seen_at":null,"token_expired_at":null,"must_change_password":false,"disabled_at":null,"has_password":false});
    let credential = json!({"id":bad,"user_id":"id","label":bad,"preview":null,"created_at":0,"last_used_at":null,"expires_at":null,"revoked_at":null});
    for (path, args, body) in [
        (
            "/admin/users",
            vec!["user", "add", "--username", "alice"],
            json!({"user":user,"token":bad}),
        ),
        (
            "/admin/human-users",
            vec!["user", "add-human", "--username", "alice"],
            json!({"user":user,"temporary_password":bad}),
        ),
        (
            "/admin/api-credentials",
            vec![
                "api-key",
                "add",
                "--username",
                "alice",
                "--label",
                "fixture",
            ],
            json!({"credential":credential,"token":bad}),
        ),
    ] {
        let server = ApiFixture::start(path, body.clone()).await;
        let output = cli_output(root.path(), &server.url, &args).await;
        assert!(output.status.success(), "{output:?}");
        for stream in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(stream);
            assert!(
                text.chars().all(|c| c == '\n' || !c.is_control()),
                "raw credential controls: {text:?}"
            );
            assert!(
                !text.lines().any(|line| line.starts_with("FORGED")),
                "forged credential line: {text:?}"
            );
        }
        assert!(String::from_utf8_lossy(&output.stdout).contains("café?FORGED?]52;c;dummy?"));
        let mut json_args = args;
        json_args.push("--json");
        assert_eq!(
            serde_json::from_str::<Value>(&cli(root.path(), &server.url, &json_args).await)
                .unwrap(),
            body
        );
    }
}

#[tokio::test]
async fn terminal_http_error_body_cannot_forge_stderr() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/admin/search",
        any(|| async {
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "café\nFORGED\u{1b}]52;c;dummy\u{7}",
            )
        }),
    );
    let server = ApiFixture {
        url,
        task: tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    };
    let output = cli_output(
        root.path(),
        &server.url,
        &["search", "query", "--workspace", "ws", "--project", "p"],
    )
    .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("502") && stderr.contains("café?FORGED?]52;c;dummy?"),
        "{stderr:?}"
    );
    assert!(
        !stderr.contains('\u{1b}') && !stderr.lines().any(|line| line.starts_with("FORGED")),
        "{stderr:?}"
    );
}

#[tokio::test]
async fn terminal_provider_errors_cannot_forge_stderr() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(any(|| async {
        (
            axum::http::StatusCode::BAD_GATEWAY,
            "café\nFORGED\u{1b}]52;c;dummy\u{7}",
        )
    }));
    let server = ApiFixture {
        url,
        task: tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    };
    for structured in [false, true] {
        let mut args = vec![
            "llm-test",
            "--provider",
            "openai-compat",
            "--model",
            "fixture",
            "--api-key",
            "fixture",
            "--base-url",
            &server.url,
            "--prompt",
            "local",
        ];
        if structured {
            args.push("--structured");
        }
        let output = cli_output(root.path(), &server.url, &args).await;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("502") && stderr.contains("café?FORGED?]52;c;dummy?"),
            "{stderr:?}"
        );
        assert!(
            !stderr.contains('\u{1b}') && !stderr.lines().any(|line| line.starts_with("FORGED")),
            "{stderr:?}"
        );
    }
}

#[tokio::test]
async fn terminal_oidc_provider_errors_cannot_forge_stderr() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(any(|| async {
        (
            axum::http::StatusCode::BAD_GATEWAY,
            "café\nFORGED\u{1b}]52;c;dummy\u{7}",
        )
    }));
    let server = ApiFixture {
        url,
        task: tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    };
    let output = cli_output(
        root.path(),
        &server.url,
        &[
            "auth",
            "login",
            "oidc-device",
            "--issuer",
            &server.url,
            "--client-id",
            "fixture",
        ],
    )
    .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("502") && stderr.contains("café?FORGED?]52;c;dummy?"),
        "{stderr:?}"
    );
    assert!(
        !stderr.contains('\u{1b}') && !stderr.lines().any(|line| line.starts_with("FORGED")),
        "{stderr:?}"
    );
}

#[tokio::test]
async fn terminal_finalize_batch_error_cannot_forge_stderr() {
    let root = tempfile::tempdir().unwrap();
    let app = Router::new()
        .route("/admin/open-sessions", any(|| async {
            Json(json!({"sessions":[{"session_id":"0192b6a1-4c2e-7d3f-8a5b-1234567890ab","cwd":null}]}))
        }))
        .route("/hook/batch", any(|| async {
            (axum::http::StatusCode::BAD_GATEWAY, "café\nFORGED\u{1b}]52;c;dummy\u{7}")
        }));
    let server = ApiFixture::start_app(app).await;
    let output = cli_output(
        root.path(),
        &server.url,
        &["finalize-session", "--workspace", "ws", "--project", "p"],
    )
    .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("502") && stderr.contains("café?FORGED?]52;c;dummy?"),
        "{stderr:?}"
    );
    assert!(
        !stderr.chars().any(|c| c != '\n' && c.is_control()),
        "{stderr:?}"
    );
    assert!(
        !stderr.lines().any(|line| line.starts_with("FORGED")),
        "{stderr:?}"
    );
}

async fn assert_terminal_purge_refusal(command: &str, body: String) {
    let root = tempfile::tempdir().unwrap();
    let path = format!("/admin/{command}");
    let app = Router::new().route(
        &path,
        any(move || {
            let body = body.clone();
            async move { (axum::http::StatusCode::FORBIDDEN, body) }
        }),
    );
    let server = ApiFixture::start_app(app).await;
    let mut args = vec![command, "--workspace", "ws", "--project", "p"];
    if command == "purge-session" {
        args.extend(["--session-id", "0192b6a1-4c2e-7d3f-8a5b-1234567890ab"]);
    }
    let output = cli_output(root.path(), &server.url, &args).await;
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{args:?}: {stdout:?}");
    assert!(
        stdout.contains("Preview refused (403): café?FORGED?]52;c;dummy?"),
        "{args:?}: {stdout:?}"
    );
    assert!(
        stdout.chars().all(|c| c == '\n' || !c.is_control()),
        "{args:?}: {stdout:?}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("--confirm"));
}

#[tokio::test]
async fn terminal_purge_project_preview_raw_body_cannot_forge_stdout() {
    assert_terminal_purge_refusal("purge-project", "café\nFORGED\u{1b}]52;c;dummy\u{7}".into())
        .await;
}

#[tokio::test]
async fn terminal_purge_project_preview_json_error_cannot_forge_stdout() {
    assert_terminal_purge_refusal(
        "purge-project",
        json!({"error":"café\nFORGED\u{1b}]52;c;dummy\u{7}"}).to_string(),
    )
    .await;
}

#[tokio::test]
async fn terminal_purge_session_preview_raw_body_cannot_forge_stdout() {
    assert_terminal_purge_refusal("purge-session", "café\nFORGED\u{1b}]52;c;dummy\u{7}".into())
        .await;
}

#[tokio::test]
async fn terminal_purge_session_preview_json_error_cannot_forge_stdout() {
    assert_terminal_purge_refusal(
        "purge-session",
        json!({"error":"café\nFORGED\u{1b}]52;c;dummy\u{7}"}).to_string(),
    )
    .await;
}

#[tokio::test]
async fn terminal_handoff_forbidden_reason_cannot_forge_stderr() {
    let root = tempfile::tempdir().unwrap();
    let app = Router::new().route(
        "/handoff",
        any(|| async {
            (
                axum::http::StatusCode::FORBIDDEN,
                "  café\nFORGED\u{1b}]52;c;dummy\u{7}  ",
            )
        }),
    );
    let server = ApiFixture::start_app(app).await;
    let output = cli_output(
        root.path(),
        &server.url,
        &[
            "hook",
            "--agent",
            "claude-code",
            "--event",
            "session-start",
            "--server-url",
            &server.url,
        ],
    )
    .await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout, b"{}\n",
        "a refusal must not enter agent context"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("not authorized") && stderr.contains("(café?FORGED?]52;c;dummy?)"),
        "{stderr:?}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr:?}");
    assert!(
        stderr.chars().all(|c| c == '\n' || !c.is_control()),
        "{stderr:?}"
    );
}

#[tokio::test]
async fn terminal_rename_workstream_neutralizes_both_branches_and_preserves_json() {
    let root = tempfile::tempdir().unwrap();
    let bad = "café\nFORGED\u{1b}]52;c;dummy\u{7}";
    for from in ["old\nFORGED\u{1b}]52;c;dummy\u{7}", bad] {
        let response =
            json!({"workstream_id":"0192b6a1-4c2e-7d3f-8a5b-1234567890ab", "from":from, "to":bad});
        let server = ApiFixture::start("/workstream/rename", response.clone()).await;
        let args = [
            "rename-workstream",
            "--workspace",
            "ws",
            "--project",
            "p",
            "--from",
            "old",
            "--to",
            "new",
        ];
        let output = cli(root.path(), &server.url, &args).await;
        assert!(output.contains("café?FORGED?]52;c;dummy?"), "{output:?}");
        assert!(
            output.chars().all(|c| c == '\n' || !c.is_control()),
            "{output:?}"
        );
        assert_eq!(
            output.lines().count(),
            if from == bad { 1 } else { 2 },
            "{output:?}"
        );
        if from != bad {
            assert!(output.contains("old?FORGED?]52;c;dummy?"), "{output:?}");
            assert!(output.contains("    id: 0192b6a1-4c2e-7d3f-8a5b-1234567890ab"));
        }
        let mut args = args.to_vec();
        args.push("--json");
        assert_eq!(
            serde_json::from_str::<Value>(&cli(root.path(), &server.url, &args).await).unwrap(),
            response
        );
    }
}
