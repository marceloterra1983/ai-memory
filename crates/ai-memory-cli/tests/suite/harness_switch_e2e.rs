//! Switching harnesses after an abrupt stop, end to end.
//!
//! The reported failure: an OpenCode session ran out of quota mid-task, so it
//! never ended and left no handoff. The user resumed an older Claude Code
//! session and asked where they had left off. The only open handoff was that
//! Claude session's own baton from days earlier, and the agent continued
//! from it while the real last task sat in the OpenCode session's captured
//! observations.
//!
//! This drives the shipped binary against a real server the way the two
//! harnesses do (the OpenCode plugin posts to `/hook`; Claude Code and Codex
//! run `ai-memory hook`), then follows the session-start notice to the cut-off
//! session over MCP, as the next agent would, and checks that what it reads is
//! exactly where the first harness stopped.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ai_memory_core::SessionId;
use serde_json::{Value, json};

use crate::e2e_support::{ServerGuard, hermetic, start_serve};

const BIN: &str = env!("CARGO_BIN_EXE_ai-memory");
const TOKEN: &str = "harness-switch-local-test-token";
const WORKSPACE: &str = "switch-test";
const PROJECT: &str = "harness-switch";

/// The cut-off OpenCode session's native id, and its last prompt and tool
/// result: where the work stopped.
const OPENCODE_SESSION: &str = "ses_cut_off_by_quota";
const LAST_PROMPT: &str = "how do we disable router advertisements on the BE800?";
const LAST_TOOL_OUTPUT: &str = "default via fe80::42ae:30ff:fe6e:7380 dev enp16s0";

/// The Claude Code session the user resumes, and its days-old work.
const CLAUDE_SESSION: &str = "0f3c8a52-1d4e-4b7a-9c2e-5a6b7c8d9e01";
const OLD_CLAUDE_PROMPT: &str = "which step remains to pair the 8BitDo controller?";

struct Fixture {
    _server: ServerGuard,
    root: PathBuf,
    client: reqwest::Client,
    base: String,
    // Last field: drops after the server has stopped using the tree.
    _guard: tempfile::TempDir,
}

fn command(root: &Path) -> Command {
    let mut cmd = hermetic(BIN);
    cmd.env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("AI_MEMORY_HOME", root.join("home"))
        .env("AI_MEMORY_DATA_DIR", root.join("data"))
        .env("AI_MEMORY_AUTH_TOKEN", TOKEN)
        .env("AI_MEMORY_EMBEDDING_PROVIDER", "none")
        .env("AI_MEMORY_BACKFILL_ON_START", "false")
        .env("AI_MEMORY_CONSOLIDATE_ON_SESSION_END", "false")
        .env("AI_MEMORY_AUTO_IMPROVE__SCHEDULER__ENABLED", "false")
        .env("RUST_LOG", "off")
        .current_dir(root.join("project"));
    cmd
}

impl Fixture {
    async fn start() -> Self {
        let guard = tempfile::tempdir().unwrap();
        let root = guard.path().to_path_buf();
        for dir in ["home", "data", "project"] {
            std::fs::create_dir(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("project/.ai-memory.toml"),
            format!("workspace = \"{WORKSPACE}\"\nproject = \"{PROJECT}\"\n"),
        )
        .unwrap();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let (server, base) = start_serve(&client, &root.join("serve.log"), |port| {
            let mut cmd = command(&root);
            cmd.args([
                "serve",
                "--transport",
                "http",
                "--bind",
                &format!("127.0.0.1:{port}"),
                "--no-watcher",
            ]);
            cmd
        })
        .await;
        Self {
            _server: server,
            root,
            client,
            base,
            _guard: guard,
        }
    }

    fn project_dir(&self) -> PathBuf {
        self.root.join("project")
    }

    /// One OpenCode lifecycle event, posted with the field names its
    /// generated plugin uses (`sessionID`, `tool`, `callID`, `output`).
    ///
    /// The server acknowledges a hook before storing it, so events sent
    /// microseconds apart could be stored out of order; a real harness's
    /// events are seconds apart. Each one is stored before the next.
    async fn opencode(&self, event: &str, body: Value) {
        let id = SessionId::from_native(OPENCODE_SESSION).to_string();
        let before = self.observation_total(&id).await;
        let mut body = body;
        body["sessionID"] = json!(OPENCODE_SESSION);
        body["cwd"] = json!(self.project_dir());
        let response = self
            .client
            .post(format!("{}/hook", self.base))
            .query(&[
                ("event", event),
                ("agent", "open-code"),
                ("workspace", WORKSPACE),
                ("project", PROJECT),
            ])
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        self.wait_for_observations(&id, before + 1).await;
    }

    /// How many observations a session has stored, 0 before its first.
    async fn observation_total(&self, session_id: &str) -> u64 {
        let response = self
            .client
            .post(format!("{}/mcp", self.base))
            .bearer_auth(TOKEN)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "memory_read_session_observations",
                        "arguments": {
                            "session_id": session_id,
                            "limit": 1,
                            "workspace": WORKSPACE,
                            "project": PROJECT,
                        },
                    },
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        response
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|page| page["total"].as_u64())
            .unwrap_or(0)
    }

    /// One native `ai-memory hook` run from inside the project, as the
    /// harness's installed hook runs it. Returns its stdout.
    fn native_hook(&self, agent: &str, event: &str, body: Value) -> String {
        let mut child = command(&self.root)
            .args([
                "hook",
                "--agent",
                agent,
                "--event",
                event,
                "--server-url",
                &self.base,
                "--auth-token",
                TOKEN,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn claude(&self, event: &str, session: &str, extra: Value) -> String {
        let mut body = json!({ "session_id": session, "cwd": self.project_dir() });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            body.extend(extra.clone());
        }
        self.native_hook("claude-code", event, body)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Value {
        self.call_tool_with_headers(name, arguments, &[]).await
    }

    async fn call_tool_with_headers(
        &self,
        name: &str,
        arguments: Value,
        headers: &[(&str, &str)],
    ) -> Value {
        let mut req = self
            .client
            .post(format!("{}/mcp", self.base))
            .bearer_auth(TOKEN)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let text = req
            .body(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": { "name": name, "arguments": arguments },
                })
                .to_string(),
            )
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let reply: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("non-JSON mcp reply: {text}: {e}"));
        assert!(reply.get("error").is_none(), "JSON-RPC error: {text}");
        let content = reply
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("missing tool text: {text}"));
        assert_ne!(
            reply.pointer("/result/isError"),
            Some(&json!(true)),
            "{text}"
        );
        serde_json::from_str(content).unwrap_or_else(|e| panic!("{content}: {e}"))
    }

    /// The newest captured observations of one session, newest first.
    async fn newest_observations(&self, session_id: &str, limit: u64) -> Value {
        self.call_tool(
            "memory_read_session_observations",
            json!({
                "session_id": session_id,
                "order": "desc",
                "limit": limit,
                "workspace": WORKSPACE,
                "project": PROJECT,
            }),
        )
        .await
    }

    /// Hooks are acknowledged before they are stored; wait until the
    /// session has `count` observations.
    async fn wait_for_observations(&self, session_id: &str, count: u64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.observation_total(session_id).await < count {
            assert!(
                Instant::now() < deadline,
                "session {session_id} never reached {count} observations"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A session end writes its handoff after the hook is acknowledged; wait
    /// until `count` handoffs are open, so later work is newer than them.
    async fn wait_for_open_handoffs(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let listed = self
                .call_tool(
                    "memory_handoff_list",
                    json!({ "workspace": WORKSPACE, "project": PROJECT }),
                )
                .await;
            let open = listed["handoffs"].as_array().map_or(0, Vec::len);
            if open >= count {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "only {open} of {count} handoffs opened: {listed}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The session id a session-start notice points at, if any.
fn pointed_session(context: &str) -> Option<String> {
    let start = context.find("🧭")?;
    let rest = &context[start..];
    let id = rest.split("session `").nth(1)?.split('`').next()?;
    Some(id.to_owned())
}

fn additional_context(stdout: &str) -> String {
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("session-start stdout is not JSON: {stdout}: {e}"));
    envelope
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn the_next_harness_is_pointed_at_where_a_cut_off_harness_stopped() {
    let fx = Fixture::start().await;
    let claude_id = SessionId::from_native(CLAUDE_SESSION).to_string();
    let opencode_id = SessionId::from_native(OPENCODE_SESSION).to_string();

    // Days ago: a Claude Code session worked and ended cleanly, leaving
    // its automatic handoff behind.
    fx.claude(
        "session-start",
        CLAUDE_SESSION,
        json!({ "source": "startup" }),
    );
    fx.claude(
        "user-prompt",
        CLAUDE_SESSION,
        json!({ "prompt": OLD_CLAUDE_PROMPT }),
    );
    fx.claude("session-end", CLAUDE_SESSION, json!({ "reason": "exit" }));
    fx.wait_for_observations(&claude_id, 2).await;
    fx.wait_for_open_handoffs(1).await;

    // Today: OpenCode works on the router, then hits its quota mid-task.
    // No stop, no session end, no handoff.
    fx.opencode("session-start", json!({})).await;
    fx.opencode(
        "user-prompt",
        json!({ "prompt": "check the router IPv6 advertisement" }),
    )
    .await;
    fx.opencode(
        "post-tool-use",
        json!({
            "tool": "bash",
            "callID": "call_0e9372fb",
            "args": { "command": "ip -6 route" },
            "output": LAST_TOOL_OUTPUT,
        }),
    )
    .await;
    fx.opencode("user-prompt", json!({ "prompt": LAST_PROMPT }))
        .await;
    fx.wait_for_observations(&opencode_id, 3).await;

    // The user resumes the old Claude Code session.
    let context = additional_context(&fx.claude(
        "session-start",
        CLAUDE_SESSION,
        json!({ "source": "resume" }),
    ));
    assert_eq!(
        pointed_session(&context).as_deref(),
        Some(opencode_id.as_str()),
        "the resumed session must be pointed at the cut-off one:\n{context}"
    );
    assert!(context.contains("`open-code`"), "{context}");
    let notice = context
        .lines()
        .find(|line| line.starts_with("🧭 ai-memory:"))
        .expect("the upstream continuity notice remains present");
    let history = context
        .split_once("<!-- ai-memory:untrusted-history:start -->")
        .and_then(|(_, tail)| tail.split_once("<!-- ai-memory:untrusted-history:end -->"))
        .map(|(history, _)| history)
        .expect("the fork recovery snapshot must delimit captured text");
    for captured in [LAST_PROMPT, LAST_TOOL_OUTPUT] {
        assert!(
            !notice.contains(captured),
            "the notice carries no captured text:\n{context}"
        );
        assert!(
            history.contains(captured),
            "recovery evidence missing: {context}"
        );
    }

    // Following the notice the way the agent would: the newest captured
    // activity is the prompt it was cut off on, then the tool result
    // before it.
    let read = fx.newest_observations(&opencode_id, 5).await;
    assert_eq!(read["session"]["agent_kind"], "open-code", "{read}");
    assert_eq!(read["observations"][0]["kind"], "user-prompt", "{read}");
    assert!(
        read["observations"][0]["body"]
            .as_str()
            .unwrap_or_default()
            .contains(LAST_PROMPT),
        "{read}"
    );
    assert!(
        read["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["body"]
                .as_str()
                .is_some_and(|body| body.contains(LAST_TOOL_OUTPUT))),
        "{read}"
    );
    // The native OpenCode id reads the same session.
    let by_native = fx.newest_observations(OPENCODE_SESSION, 1).await;
    assert_eq!(by_native["session"]["session_id"], opencode_id);

    // An agent that asks instead of reading the notice finds the same
    // session: newest in the briefing, and named by the handoff list as
    // newer than every open handoff.
    let briefing = fx
        .call_tool(
            "memory_briefing",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
        )
        .await;
    assert_eq!(
        briefing["recent_sessions"][0]["session_id"], opencode_id,
        "{briefing}"
    );
    assert_eq!(briefing["recent_sessions"][0]["ended"], false);
    let handoffs = fx
        .call_tool(
            "memory_handoff_list",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
        )
        .await;
    assert_eq!(
        handoffs["newer_unended_session"]["session_id"], opencode_id,
        "{handoffs}"
    );

    // A different harness starting fresh is pointed there too.
    let codex = fx.native_hook(
        "codex",
        "session-start",
        json!({
            "session_id": "019a0000-0000-7000-8000-00000000c0de",
            "cwd": fx.project_dir(),
            "source": "startup",
        }),
    );
    assert!(codex.contains(&opencode_id), "{codex}");

    // Once OpenCode resumes and ends cleanly, nothing is cut off any
    // more, and the next session start names no session.
    fx.opencode("stop", json!({})).await;
    fx.opencode("session-end", json!({})).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let listed = fx
            .call_tool(
                "memory_briefing",
                json!({ "workspace": WORKSPACE, "project": PROJECT }),
            )
            .await;
        if listed["recent_sessions"][0]["ended"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "{listed}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let context = additional_context(&fx.claude(
        "session-start",
        "5d1e2f3a-4b5c-4d6e-8f70-81920a1b2c3d",
        json!({ "source": "startup" }),
    ));
    assert_eq!(pointed_session(&context), None, "{context}");
}

/// Real-server PT-BR cross-harness handoff: writer (`claude-code`) and
/// readers (`codex` / `open-code`) are distinct harnesses on an isolated temp
/// `ai-memory serve`. Covers automatic `session-end` handoff + `GET /handoff`
/// claim + `consumed_by_hook` identity + single-use exclusivity + explicit
/// `next_steps` extraction & negative scope/harness controls.
#[tokio::test]
async fn ptbr_cross_harness_handoff_real_server_claims_exclusively_and_proves_next_step() {
    let fx = Fixture::start().await;
    let cc_session = "cc-ptbr-handoff-writer-01";
    let cc_id = SessionId::from_native(cc_session).to_string();
    let codex_session = "019a0000-0000-7000-8000-00000000a301";
    let opencode_session = "ses-opencode-ptbr-reader-02";

    // 1. Writer harness (`claude-code`) records a PT-BR unresolved question
    //    (superseding an older `pkcs11_hardware` attempt) and ends cleanly.
    fx.claude("session-start", cc_session, json!({ "source": "startup" }));
    fx.claude(
        "user-prompt",
        cc_session,
        json!({
            "prompt": "Qual o próximo passo obrigatório para o assinador_xml após descartar HSM_TOKEN_MODE=pkcs11_hardware: rodar cargo test --test assinatura_a3_offline com HSM_TOKEN_MODE=simulado antes da tag v2.4.1?"
        }),
    );
    fx.wait_for_observations(&cc_id, 1).await;
    fx.claude("session-end", cc_session, json!({ "reason": "exit" }));
    fx.wait_for_open_handoffs(1).await;

    let listed = fx
        .call_tool(
            "memory_handoff_list",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
        )
        .await;
    assert_eq!(listed["handoffs"][0]["from_agent"], "claude-code");

    // 2. Reader harness (`codex`) claims the baton via its real SessionStart
    //    hook path (`GET /handoff?agent=codex`).
    let codex_ctx = additional_context(&fx.native_hook(
        "codex",
        "session-start",
        json!({
            "session_id": codex_session,
            "cwd": fx.project_dir(),
            "source": "startup",
        }),
    ));
    assert!(
        codex_ctx.contains("pending handoff from previous session"),
        "codex must receive the claimed handoff block:\n{codex_ctx}"
    );
    assert!(codex_ctx.contains("from `claude-code`"), "{codex_ctx}");
    assert!(
        codex_ctx.contains("cargo test --test assinatura_a3_offline")
            && codex_ctx.contains("HSM_TOKEN_MODE=simulado")
            && codex_ctx.contains("v2.4.1"),
        "codex handoff must carry the PT-BR next step:\n{codex_ctx}"
    );

    // 3. Exclusivity & identity proof: open list is now empty; only `codex`'s
    //    exact session sees `consumed_by_hook`; another harness (`open-code`)
    //    receives `none_pending` and an empty `GET /handoff`.
    let after_claim = fx
        .call_tool(
            "memory_handoff_list",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
        )
        .await;
    assert_eq!(after_claim["handoffs"].as_array().unwrap().len(), 0);

    let codex_confirm = fx
        .call_tool_with_headers(
            "memory_handoff_accept",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
            &[("x-memory-actor-session-id", codex_session)],
        )
        .await;
    assert_eq!(
        codex_confirm["status"], "consumed_by_hook",
        "{codex_confirm}"
    );
    assert!(codex_confirm["handoff"].is_null());

    let outsider_mcp = fx
        .call_tool_with_headers(
            "memory_handoff_accept",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
            &[("x-memory-actor-session-id", opencode_session)],
        )
        .await;
    assert_eq!(outsider_mcp["status"], "none_pending", "{outsider_mcp}");
    assert!(outsider_mcp["handoff"].is_null());

    let outsider_hook = fx
        .client
        .get(format!("{}/handoff", fx.base))
        .query(&[
            ("agent", "open-code"),
            ("workspace", WORKSPACE),
            ("project", PROJECT),
            ("session_id", opencode_session),
            ("cwd", fx.project_dir().to_str().unwrap()),
        ])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !outsider_hook.contains("pending handoff"),
        "already-claimed baton must stay exclusive: {outsider_hook}"
    );

    // 4. Explicit structured `next_steps` handoff: negative control on another
    //    project, then `open-code` claims over `GET /handoff` and parses the
    //    exact `**Next steps**` action deterministically.
    let expected_cmd = "cargo test --test assinatura_a3_offline -- --exact valida_lote_rps";
    let expected_env = "HSM_TOKEN_MODE=simulado";
    let begun = fx
        .call_tool(
            "memory_handoff_begin",
            json!({
                "workspace": WORKSPACE,
                "project": PROJECT,
                "summary": "Parser RPS do assinador_xml concluído; validação offline pendente.",
                "open_questions": ["Confirmar tag v2.4.1 após validação offline"],
                "next_steps": [expected_cmd, expected_env],
                "files_touched": ["crates/assinador_xml/src/lote.rs"],
                "shared": true,
            }),
        )
        .await;
    assert!(begun["handoff_id"].as_str().is_some(), "{begun}");

    // Negative control: wrong project must neither receive nor consume it.
    let wrong_project_hook = fx
        .client
        .get(format!("{}/handoff", fx.base))
        .query(&[
            ("agent", "open-code"),
            ("workspace", WORKSPACE),
            ("project", "projeto-errado"),
            ("session_id", "wrong-proj-session"),
        ])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !wrong_project_hook.contains(expected_cmd),
        "handoff leaked to unrelated project: {wrong_project_hook}"
    );

    // Reader (`open-code`) claims over the real `GET /handoff` route.
    let oc_claimed = fx
        .client
        .get(format!("{}/handoff", fx.base))
        .query(&[
            ("agent", "open-code"),
            ("workspace", WORKSPACE),
            ("project", PROJECT),
            ("session_id", opencode_session),
            ("cwd", fx.project_dir().to_str().unwrap()),
        ])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let history_block = oc_claimed
        .split_once("<!-- ai-memory:untrusted-history:start -->")
        .and_then(|(_, tail)| tail.split_once("<!-- ai-memory:untrusted-history:end -->"))
        .map(|(inner, _)| inner)
        .expect("claimed handoff must delimit untrusted history block");
    let next_steps_section = history_block
        .split("**Next steps**\n")
        .nth(1)
        .and_then(|tail| tail.split("\n**").next())
        .expect("claimed handoff must contain **Next steps** section");
    let parsed_steps: Vec<&str> = next_steps_section
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .collect();
    assert_eq!(parsed_steps, vec![expected_cmd, expected_env]);
    assert!(
        !next_steps_section.contains("pkcs11_hardware"),
        "discarded mode must not appear as next step"
    );

    // Deterministic application check: the parsed action maps to the exact
    // command + env var, and `codex` cannot re-claim what `open-code` took.
    let applied_env = parsed_steps
        .iter()
        .find_map(|s| s.strip_prefix("HSM_TOKEN_MODE="))
        .unwrap();
    assert_eq!(applied_env, "simulado");
    assert_eq!(parsed_steps[0], expected_cmd);

    let oc_confirm = fx
        .call_tool_with_headers(
            "memory_handoff_accept",
            json!({
                "workspace": WORKSPACE,
                "project": PROJECT,
                "handoff_id": begun["handoff_id"],
            }),
            &[("x-memory-actor-session-id", opencode_session)],
        )
        .await;
    assert_eq!(oc_confirm["status"], "consumed_by_hook", "{oc_confirm}");
    let codex_steal = fx
        .call_tool_with_headers(
            "memory_handoff_accept",
            json!({
                "workspace": WORKSPACE,
                "project": PROJECT,
                "handoff_id": begun["handoff_id"],
            }),
            &[("x-memory-actor-session-id", codex_session)],
        )
        .await;
    assert_eq!(codex_steal["status"], "none_pending", "{codex_steal}");
    let fresh_codex = fx
        .call_tool_with_headers(
            "memory_handoff_accept",
            json!({ "workspace": WORKSPACE, "project": PROJECT }),
            &[("x-memory-actor-session-id", "codex-fresh-session-04")],
        )
        .await;
    assert_eq!(fresh_codex["status"], "none_pending", "{fresh_codex}");
}

/// Replays the pinned `ptbr-v1.json` corpus against a real isolated
/// `ai-memory serve` binary (`POST /hook/batch` + `memory_query`), proving both
/// retrieval (`answer_session_ids` in top-10) and the retrieval-vs-use
/// boundary (required current facts vs stale/single-session negative controls).
#[tokio::test]
async fn ptbr_corpus_real_server_distinguishes_retrieval_from_evidence_use() {
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/fixtures/retrieval/ptbr-v1.json");
    let cases: Vec<Value> =
        serde_json::from_slice(&std::fs::read(&fixture_path).expect("read ptbr-v1.json"))
            .expect("parse ptbr-v1.json");
    assert_eq!(cases.len(), 4);

    let fx = Fixture::start().await;
    let workspace = "ptbr-eval-ws";

    for case in &cases {
        let qid = case["question_id"].as_str().unwrap();
        let project = format!("ptbr-{qid}");
        let cwd = format!("/tmp/ptbr/{project}");
        let dates = case["haystack_dates"].as_array().unwrap();
        let sids = case["haystack_session_ids"].as_array().unwrap();
        let sessions = case["haystack_sessions"].as_array().unwrap();

        let mut batch = Vec::new();
        let mut raw_to_uuid = std::collections::HashMap::new();
        for ((sid_v, date_v), turns_v) in sids.iter().zip(dates).zip(sessions) {
            let raw_sid = sid_v.as_str().unwrap();
            let date = date_v.as_str().unwrap();
            let stored = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{project}\n{raw_sid}").as_bytes(),
            )
            .to_string();
            raw_to_uuid.insert(raw_sid.to_string(), stored.clone());

            let push_item = |event: &str, extra: Value| {
                let mut url = format!(
                    "/hook?event={event}&agent=claude-code&workspace={workspace}&project={project}&session_id={stored}"
                );
                if event == "stop" {
                    url.push_str("&capture_assistant=1");
                }
                let mut body = json!({ "session_id": stored, "cwd": cwd });
                if let Some(obj) = extra.as_object() {
                    body.as_object_mut().unwrap().extend(obj.clone());
                }
                json!({ "url": url, "body": body })
            };

            batch.push(push_item(
                "session-start",
                json!({ "hook_event_name": "SessionStart", "source": "startup" }),
            ));
            for turn in turns_v.as_array().unwrap() {
                let role = turn["role"].as_str().unwrap();
                let content = turn["content"].as_str().unwrap();
                let stamped = format!("[session date: {date}] {content}");
                if role == "user" {
                    batch.push(push_item(
                        "user-prompt-submit",
                        json!({ "hook_event_name": "UserPromptSubmit", "prompt": stamped }),
                    ));
                } else if role == "assistant" {
                    batch.push(push_item(
                        "stop",
                        json!({
                            "hook_event_name": "Stop",
                            "_ai_memory_assistant": { "version": 1, "excerpt": stamped },
                        }),
                    ));
                }
            }
            batch.push(push_item(
                "session-end",
                json!({ "hook_event_name": "SessionEnd", "reason": "exit" }),
            ));
        }

        // Retry until the server accepts every batch item (mirrors evals ingest).
        let mut pending = batch;
        for pass in 0..30 {
            if pending.is_empty() {
                break;
            }
            let resp = fx
                .client
                .post(format!("{}/hook/batch", fx.base))
                .bearer_auth(TOKEN)
                .json(&pending)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap();
            let accepted: std::collections::HashSet<usize> = resp["accepted_indices"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_u64().map(|n| n as usize))
                        .collect()
                })
                .unwrap_or_else(|| {
                    let n = resp["accepted"].as_u64().unwrap_or(0) as usize;
                    (0..n).collect()
                });
            pending = pending
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !accepted.contains(i))
                .map(|(_, item)| item)
                .collect();
            if !pending.is_empty() {
                assert!(pass < 29, "{qid}: hook batch still pending");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }

        let query_resp = fx
            .call_tool(
                "memory_query",
                json!({
                    "query": case["question"].as_str().unwrap(),
                    "workspace": workspace,
                    "project": project,
                    "limit": 10,
                    "pin_first": false,
                }),
            )
            .await;
        let hits = query_resp["hits"].as_array().unwrap();
        let hit_paths: Vec<&str> = hits.iter().filter_map(|h| h["path"].as_str()).collect();

        // 1. Retrieval check: every expected evidence session appears in top-10.
        let mut retrieved_uuids = Vec::new();
        for ans_sid in case["answer_session_ids"].as_array().unwrap() {
            let sid = ans_sid.as_str().unwrap();
            let stored_uuid = &raw_to_uuid[sid];
            let expected_path = format!("sessions/{stored_uuid}.md");
            let _hit = hits
                .iter()
                .find(|h| h["path"].as_str() == Some(expected_path.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "{qid}: missing evidence session {sid} ({expected_path}) in {hit_paths:?}"
                    )
                });
            retrieved_uuids.push(stored_uuid.clone());
        }

        // 2. Evidence-use check vs. retrieval: `memory_query` snippets are
        //    bounded previews, so consuming the retrieved session means
        //    reading its stored observations (`memory_read_session_observations`)
        //    and verifying all required facts are present, while negative
        //    controls (stale/general or single-session slice) fail the use
        //    contract even when retrieved.
        let mut evidence_bodies = Vec::new();
        for stored_uuid in &retrieved_uuids {
            let obs_resp = fx
                .call_tool(
                    "memory_read_session_observations",
                    json!({
                        "session_id": stored_uuid,
                        "order": "asc",
                        "limit": 20,
                        "workspace": workspace,
                        "project": project,
                    }),
                )
                .await;
            let bodies: Vec<&str> = obs_resp["observations"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|o| o["body"].as_str())
                .collect();
            evidence_bodies.push(bodies.join("\n"));
        }
        let combined_evidence = evidence_bodies.join("\n").to_lowercase();
        let required: Vec<&str> = case["evidence_contract"]["required_facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for fact in &required {
            assert!(
                combined_evidence.contains(&fact.to_lowercase()),
                "{qid}: retrieved session evidence missing required fact {fact:?}: {combined_evidence}"
            );
        }

        if case["evidence_contract"]["requires_multi_session_synthesis"].as_bool() == Some(true) {
            assert_eq!(evidence_bodies.len(), 2);
            let first_only = evidence_bodies[0].to_lowercase();
            assert!(
                !required
                    .iter()
                    .all(|f| first_only.contains(&f.to_lowercase())),
                "{qid}: single retrieved session should not satisfy multi-session evidence use"
            );
        } else {
            let stale_sid = case["evidence_contract"]["supersedes_session_id"]
                .as_str()
                .unwrap();
            let stale_idx = sids
                .iter()
                .position(|v| v.as_str() == Some(stale_sid))
                .unwrap();
            let stale_text = sessions[stale_idx][0]["content"]
                .as_str()
                .unwrap()
                .to_lowercase();
            assert!(
                !required
                    .iter()
                    .all(|f| stale_text.contains(&f.to_lowercase())),
                "{qid}: stale/general session must not satisfy required current facts"
            );
        }
    }
}
