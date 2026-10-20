//! Document-only budgets through the real factory and restricted transport.

use std::time::Duration;

use ai_memory_llm::{EmbedderChoice, EmbedderConfig, LlmError, build_embedder};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(endpoint: &str, local_only: bool, seconds: u64) -> EmbedderConfig {
    EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "synthetic".into(),
        dim: 2,
        api_key: SecretString::from("synthetic-key-must-not-leak-in-dream"),
        base_url: Some(endpoint.into()),
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
        query_base_url: None,
        document_timeout_secs: seconds,
        local_only,
    }
}

#[test]
fn factory_refuses_invalid_document_timeout_and_incompatible_providers() {
    for local in [false, true] {
        for seconds in [0, 1801, u64::MAX] {
            let result = build_embedder(config("http://127.0.0.1:9/v1", local, seconds));
            assert!(
                matches!(result, Err(LlmError::NotConfigured(message)) if message.contains("AI_MEMORY_EMBEDDING_DOCUMENT_TIMEOUT_SECS"))
            );
        }
    }
    let providers = [
        EmbedderChoice::OpenAi,
        EmbedderChoice::Voyage,
        EmbedderChoice::Google,
        EmbedderChoice::Copilot,
        #[cfg(feature = "local-embeddings")]
        EmbedderChoice::Local,
    ];
    for provider in providers {
        let result = build_embedder(EmbedderConfig {
            provider,
            ..config("http://127.0.0.1:9/v1", false, 1800)
        });
        assert!(
            matches!(result, Err(LlmError::NotConfigured(message)) if message.contains("AI_MEMORY_EMBEDDING_DOCUMENT_TIMEOUT_SECS") && message.contains("openai-compat")),
            "provider {provider:?} silently ignored the setting"
        );
    }
    for seconds in [1, 120, 1800] {
        assert!(build_embedder(config("http://127.0.0.1:9/v1", false, seconds)).is_ok());
    }
    assert!(
        build_embedder(EmbedderConfig {
            provider: EmbedderChoice::OpenAi,
            ..config("http://127.0.0.1:9/v1", false, 120)
        })
        .is_ok()
    );
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn factory_applies_document_timeout_to_embed_and_embed_document(#[case] local: bool) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(1100))
                .set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]})),
        )
        .mount(&server)
        .await;
    let short = build_embedder(config(&server.uri(), local, 1)).unwrap();
    for document_method in [false, true] {
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            if document_method {
                short.embed_document("synthetic").await
            } else {
                short.embed("synthetic").await
            }
        })
        .await
        .expect("the configured one-second deadline must end the request");
        assert!(result.is_err(), "the factory ignored the document timeout");
    }
    let default = build_embedder(config(&server.uri(), local, 120)).unwrap();
    assert_eq!(
        default.embed_document("synthetic").await.unwrap(),
        vec![1.0, 0.0]
    );
    assert_eq!(
        (default.provider(), default.model(), default.dim()),
        ("openai-compat", "synthetic", 2)
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3, "timeouts must not retry");
    if local {
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("authorization"))
        );
    }
}

#[tokio::test]
async fn dream_document_timeout_override_refuses_unsafe_budgets_before_http() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .mount(&server)
        .await;
    let client = ai_memory_llm::DreamLocalClient::new(&server.uri()).unwrap();
    for budget in [Duration::ZERO, Duration::from_secs(1801), Duration::MAX] {
        let result = client
            .post_embedding_document_json(&json!({"input":"synthetic"}), budget)
            .await;
        assert!(matches!(result, Err(LlmError::NotConfigured(_))));
    }
    assert_eq!(client.request_count(), 0);
    assert_eq!(
        client
            .post_embedding_document_json(&json!({"input":"synthetic"}), Duration::from_secs(1800))
            .await
            .unwrap(),
        json!({"ok":true})
    );
    assert_eq!(client.request_count(), 1);
}

#[tokio::test]
async fn dream_extended_document_timeout_keeps_redirect_retry_wire_and_response_guards() {
    let target = MockServer::start().await;
    for (status, body) in [
        (307, "synthetic-private-redirect".to_string()),
        (429, "synthetic-private-rate-limit".to_string()),
        (
            200,
            format!("{{\"synthetic-private\":\"{}\"}}", "a".repeat(128 * 1024)),
        ),
    ] {
        let source = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", target.uri())
                    .set_body_string(body),
            )
            .mount(&source)
            .await;
        let embedder = build_embedder(config(&source.uri(), true, 1800)).unwrap();
        let error = embedder.embed_document("synthetic").await.unwrap_err();
        assert!(!error.to_string().contains("synthetic-private"));
        assert_eq!(source.received_requests().await.unwrap().len(), 1);
        assert!(target.received_requests().await.unwrap().is_empty());
    }
    let control = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]})),
        )
        .mount(&control)
        .await;
    let embedder = build_embedder(config(&control.uri(), true, 1800)).unwrap();
    assert!(embedder.embed_query(&"q".repeat(8192)).await.is_err());
    assert!(control.received_requests().await.unwrap().is_empty());
    assert_eq!(
        embedder.embed_document(&"d".repeat(9000)).await.unwrap(),
        vec![1.0, 0.0]
    );
    let requests = control.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(
        |request| request.body.len() <= 8192 && !request.headers.contains_key("authorization")
    ));
    for endpoint in [
        "http://localhost:8000/v1",
        "http://user:private@127.0.0.1:8000/v1",
        "http://127.0.0.1:8789/v1",
        "https://external.invalid/v1",
    ] {
        assert!(
            matches!(build_embedder(config(endpoint, true, 1800)), Err(LlmError::NotConfigured(message)) if !message.contains("private"))
        );
    }
}

#[tokio::test]
async fn dream_extended_document_timeout_never_uses_proxy_or_credentials() {
    const CHILD: &str = "AI_MEMORY_TEST_DOCUMENT_TIMEOUT_PROXY_CHILD";
    if let Some(endpoint) = std::env::var_os(CHILD) {
        let embedder = build_embedder(config(endpoint.to_str().unwrap(), true, 1800)).unwrap();
        assert_eq!(
            embedder.embed_document("synthetic").await.unwrap(),
            vec![1.0, 0.0]
        );
        return;
    }
    let target = MockServer::start().await;
    let proxy = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]})),
        )
        .mount(&target)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "integration::embedding_document_timeout::dream_extended_document_timeout_never_uses_proxy_or_credentials", "--nocapture"])
        .env_clear().env("HOME", tmp.path()).env(CHILD, target.uri())
        .env("HTTP_PROXY", proxy.uri()).env("http_proxy", proxy.uri())
        .env("ALL_PROXY", proxy.uri()).env("all_proxy", proxy.uri())
        .env("NO_PROXY", "").env("no_proxy", "")
        .output().await.unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(proxy.received_requests().await.unwrap().is_empty());
    let requests = target.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("authorization"));
}

// Opt-in wall-clock proof, entirely synthetic: no production server, model,
// credentials, configuration or data is opened. Both ordinary and DREAM transports use real HTTP.
#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
#[ignore = "manual 125-second real HTTP deadline proof"]
async fn manual_125s_document_timeout(#[case] local: bool) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(125))
                .set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]})),
        )
        .mount(&server)
        .await;
    let old = build_embedder(config(&server.uri(), local, 120)).unwrap();
    let extended = build_embedder(config(&server.uri(), local, 1800)).unwrap();
    let start = std::time::Instant::now();
    let (old_document, document, bare, query) =
        tokio::time::timeout(Duration::from_secs(150), async {
            tokio::join!(
                old.embed_document("synthetic"),
                extended.embed_document("synthetic"),
                extended.embed("synthetic"),
                extended.embed_query("synthetic")
            )
        })
        .await
        .expect("the isolated HTTP proof must finish within 150 real seconds");
    assert!(old_document.is_err(), "the old 120-second budget must fail");
    assert_eq!(document.unwrap(), vec![1.0, 0.0]);
    assert_eq!(bare.unwrap(), vec![1.0, 0.0]);
    assert!(
        query.is_err(),
        "the query must retain 120 seconds without an endpoint override"
    );
    assert!(start.elapsed() >= Duration::from_secs(125));
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    println!(
        "local={local} real_elapsed={:?} old_document_120=ERR document_1800=OK embed_1800=OK query_120=ERR",
        start.elapsed()
    );
}
