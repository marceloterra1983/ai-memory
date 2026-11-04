//! Run the shipped bundles against real redirect and direct-response endpoints.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::routing::any;
use serde_json::json;
use tokio::io::AsyncWriteExt;

#[derive(Debug)]
struct Request {
    method: Method,
    path: String,
    auth: Option<String>,
    body: Bytes,
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(status: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let location = format!("{url}/target");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let record = requests.clone();
        let app = Router::new().fallback(any(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let record = record.clone();
                let location = location.clone();
                async move {
                    let target = uri.path() == "/target";
                    record.lock().unwrap().push(Request {
                        method,
                        path: uri.path().into(),
                        auth: headers
                            .get("authorization")
                            .map(|v| v.to_str().unwrap().to_owned()),
                        body,
                    });
                    (
                        if target {
                            StatusCode::OK
                        } else {
                            StatusCode::from_u16(status).unwrap()
                        },
                        [
                            ("location", location),
                            ("content-type", "text/plain; charset=utf-8".into()),
                        ],
                        if target {
                            "FORGED LOGIN CONTEXT"
                        } else {
                            "legitimate handoff\nsecond line\n"
                        },
                    )
                }
            },
        ));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

fn powershell() -> Option<&'static str> {
    #[cfg(windows)]
    {
        Some(ai_memory_test_support::powershell_exe())
    }
    #[cfg(not(windows))]
    {
        match std::process::Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-Command", "exit 0"])
            .status()
        {
            Ok(status) => {
                assert!(status.success());
                Some("pwsh")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("NOT EXECUTED: PowerShell bundle runtime requires pwsh");
                None
            }
            Err(error) => panic!("cannot start pwsh: {error}"),
        }
    }
}

async fn run(root: &Path, server: &Server, shell: &str, script: &str, helper: &Path) -> String {
    let mut command = tokio::process::Command::new(shell);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("AI_MEMORY_HOME", root)
        .env("AI_MEMORY_DATA_DIR", root.join("data"))
        .env("AI_MEMORY_AUTH_TOKEN", "bundle-fixture-bearer")
        .env("AI_MEMORY_HOOK_URL", &server.url)
        .env("AI_MEMORY_TEST_HELPER", helper)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .current_dir(root)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR", "SystemDrive", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if shell == "sh" {
        command.args(["-c", script]);
    } else {
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ]);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"cwd":root,"session_id":"bundle-fixture","prompt":"private bundle event"})
                .to_string()
                .as_bytes(),
        )
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "bundle process: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.join("data/hook-spool").exists(),
        "PowerShell must not create a spool"
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn powershell_bundle_refuses_post_redirects_without_forwarding_credentials_or_event() {
    let Some(shell) = powershell() else {
        return;
    };
    for status in [302, 307, 308, 202] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::start(status).await;
        let output = run(
            root.path(),
            &server,
            shell,
            ". $env:AI_MEMORY_TEST_HELPER; Invoke-AiMemoryHook -Event 'stop' -Agent 'claude-code'",
            &repo_root().join("hooks/lib/ai-memory-hook.ps1"),
        )
        .await;
        assert!(output.is_empty(), "POST stdout for {status}: {output:?}");
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "POST followed HTTP {status}: {requests:?}"
        );
        assert_eq!(requests[0].method, Method::POST);
        assert_eq!(requests[0].path, "/hook");
        assert_eq!(
            requests[0].auth.as_deref(),
            Some("Bearer bundle-fixture-bearer")
        );
        assert!(String::from_utf8_lossy(&requests[0].body).contains("private bundle event"));
    }
}

#[tokio::test]
async fn powershell_bundle_only_injects_2xx_handoffs() {
    let Some(shell) = powershell() else {
        return;
    };
    for status in [302, 307, 308, 304, 400, 500, 200, 204] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::start(status).await;
        let output = run(root.path(), &server, shell, "$env:AI_MEMORY_CAPTURE_OWNER='external'; . $env:AI_MEMORY_TEST_HELPER; Invoke-AiMemoryHook -Event 'session-start' -Agent 'claude-code' -FetchHandoff", &repo_root().join("hooks/lib/ai-memory-hook.ps1")).await;
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "GET followed HTTP {status}: {requests:?}"
        );
        assert_eq!(requests[0].method, Method::GET);
        assert_eq!(requests[0].path, "/handoff");
        assert_eq!(
            output,
            if status == 200 {
                "legitimate handoff\nsecond line\n"
            } else {
                ""
            },
            "HTTP {status} entered context"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn shell_bundle_only_injects_2xx_handoffs() {
    for status in [302, 307, 308, 304, 400, 500, 200, 204] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::start(status).await;
        let output = run(
            root.path(),
            &server,
            "sh",
            ". \"$AI_MEMORY_TEST_HELPER\"; ai_memory_get_handoff \"$AI_MEMORY_HOOK_URL/handoff\"",
            &repo_root().join("hooks/_lib.sh"),
        )
        .await;
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "GET followed HTTP {status}: {requests:?}"
        );
        assert_eq!(requests[0].method, Method::GET);
        assert_eq!(
            output,
            if status == 200 {
                "legitimate handoff\nsecond line\n"
            } else {
                ""
            },
            "HTTP {status} entered context"
        );
    }
}

#[tokio::test]
async fn powershell_bundle_rejects_a_returned_3xx_response_object() {
    let Some(shell) = powershell() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(200).await;
    // Windows PowerShell may return a redirect object instead of throwing.
    // This supplements the real HTTP tests; it is not a native 5.1 execution.
    let driver = r#"
$env:AI_MEMORY_CAPTURE_OWNER='external'
. $env:AI_MEMORY_TEST_HELPER
function Invoke-WebRequest {
    param([switch]$UseBasicParsing, [int]$MaximumRedirection, [string]$ErrorAction, [int]$TimeoutSec, [string]$Uri, $Headers)
    return [pscustomobject]@{StatusCode=302; Content='FORGED CONTEXT'}
}
Invoke-AiMemoryHook -Event 'session-start' -Agent 'claude-code' -FetchHandoff
"#;
    let output = run(
        root.path(),
        &server,
        shell,
        driver,
        &repo_root().join("hooks/lib/ai-memory-hook.ps1"),
    )
    .await;
    assert!(
        output.is_empty(),
        "returned 302 object entered context: {output:?}"
    );
}
