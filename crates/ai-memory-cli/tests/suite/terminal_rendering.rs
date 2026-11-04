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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            path,
            any(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
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

async fn cli(root: &Path, url: &str, args: &[&str]) -> String {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ai-memory"));
    command
        .env_clear()
        .current_dir(root)
        .env("AI_MEMORY_HOME", root.join("home"))
        .env("AI_MEMORY_DATA_DIR", root.join("data"))
        .env("AI_MEMORY_SERVER_URL", url)
        .env("AI_MEMORY_AUTH_TOKEN", "fixture-token")
        .env("NO_PROXY", "*")
        .arg("--config")
        .arg(root.join("config.toml"))
        .args(args)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap();
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
