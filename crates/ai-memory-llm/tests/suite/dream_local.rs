//! Restricted local inference boundary for dream.

use ai_memory_llm::{ChatRequest, DreamLocalClient};
use ai_memory_llm::{DreamLocalProvider, LlmProvider, LlmResult};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn dream_provider(endpoint: &str) -> LlmResult<Box<dyn LlmProvider>> {
    DreamLocalProvider::new(endpoint, "synthetic")
        .map(|provider| Box::new(provider) as Box<dyn LlmProvider>)
}

#[test]
fn dream_refuses_remote_dns_credentials_and_forbidden_port() {
    for endpoint in [
        "https://external.invalid/v1",
        "http://localhost:8000/v1",
        "http://127.0.0.1:8789/v1",
        "http://user:private@127.0.0.1:8000/v1",
        "http://127.0.0.1:8000/v1?token=private",
        "http://127.1:8000/v1",
        "http://2130706433:8000/v1",
        "http://127.0.0.2:8000/v1",
        "http://[::ffff:127.0.0.1]:8000/v1",
        "http://127.0.0.1:8000/v1#fragment",
    ] {
        assert!(dream_provider(endpoint).is_err(), "accepted {endpoint}");
    }
    for endpoint in ["http://127.0.0.1:8000/v1", "http://[::1]:8000/v1"] {
        assert!(dream_provider(endpoint).is_ok(), "refused local control");
    }
}

fn completion(content: &str) -> Value {
    json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}})
}

#[tokio::test]
async fn dream_refuses_redirect_without_contacting_target_and_redacts_errors() {
    let source = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("ok")))
        .expect(0)
        .mount(&target)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/v1/chat/completions", target.uri()))
                .set_body_string("sensitive synthetic response"),
        )
        .expect(1)
        .mount(&source)
        .await;
    let provider = dream_provider(&source.uri()).unwrap();
    let error = provider
        .complete(ChatRequest::user_prompt("synthetic"))
        .await
        .unwrap_err();
    assert_eq!(error.http_status(), Some(307));
    assert!(!error.to_string().contains("sensitive synthetic response"));
    assert!(target.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn dream_compatibility_parser_uses_one_response_without_retry_or_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion(
            "<think>private reasoning {not payload}</think>\n```json\n{\"ok\":true}\n```",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let provider = dream_provider(&server.uri()).unwrap();
    let actual = provider
        .complete_structured_raw(
            ChatRequest::user_prompt("synthetic"),
            json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}),
        )
        .await
        .unwrap();
    assert_eq!(actual, json!({"ok":true}));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("authorization"));
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["response_format"]["type"], "json_schema");
}

#[tokio::test]
async fn dream_invalid_schema_response_and_rate_limit_never_retry() {
    for (status, body) in [
        (200, completion("sensitive synthetic malformed {")),
        (429, json!({"error":"sensitive synthetic response"})),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let provider = dream_provider(&server.uri()).unwrap();
        let error = provider
            .complete_structured_raw(
                ChatRequest::user_prompt("synthetic"),
                json!({"type":"object"}),
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("sensitive synthetic"));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn dream_wire_budget_rejects_cap_plus_one_before_admission() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .expect(1)
        .mount(&server)
        .await;
    let client = DreamLocalClient::new(&server.uri()).unwrap();
    assert_eq!(client.request_count(), 0);
    let at_cap = json!({"text":"x".repeat(8181)});
    assert_eq!(serde_json::to_vec(&at_cap).unwrap().len(), 8192);
    assert!(client.post_json(&at_cap).await.is_ok());
    assert_eq!(client.request_count(), 1);
    let over_cap = json!({"text":"x".repeat(8182)});
    assert!(client.post_json(&over_cap).await.is_err());
    assert_eq!(client.request_count(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn dream_ignores_primary_provider_credentials_and_proxy_environment() {
    // Run the wire-level success control in a clean child: environment mutation
    // in the parallel parent test harness would race unrelated provider tests.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "integration::dream_local::dream_compatibility_parser_uses_one_response_without_retry_or_auth",
            "--nocapture",
        ])
        .env_clear()
        .env("TMPDIR", std::env::var_os("TMPDIR").unwrap())
        .env("OPENAI_API_KEY", "synthetic-primary-secret")
        .env("AI_MEMORY_LLM_API_KEY", "synthetic-primary-secret")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "dream inherited a provider credential or proxy"
    );
}

#[tokio::test]
async fn dream_response_limit_refuses_oversized_body_without_echoing_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"private_synthetic": "x".repeat(128 * 1024)})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = DreamLocalClient::new(&server.uri()).unwrap();
    let error = client
        .post_json(&json!({"synthetic":true}))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private_synthetic"));
    assert_eq!(client.request_count(), 1);
}

#[tokio::test]
async fn dream_incomplete_or_over_budget_completion_is_refused() {
    for body in [
        json!({"choices":[]}),
        json!({"choices":[{"message":{"content":"ok"},"finish_reason":"length"}]}),
        json!({"choices":[{"message":{"content":" "},"finish_reason":"stop"}]}),
        json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":8193,"completion_tokens":1}}),
        json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1025}}),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let provider = dream_provider(&server.uri()).unwrap();
        assert!(
            provider
                .complete(ChatRequest::user_prompt("synthetic"))
                .await
                .is_err()
        );
        assert_eq!(provider.request_count(), Some(1));
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("ok")))
        .expect(1)
        .mount(&server)
        .await;
    let provider = dream_provider(&server.uri()).unwrap();
    for max_tokens in [0, 4001] {
        assert!(
            provider
                .complete(ChatRequest::user_prompt("synthetic").with_max_tokens(max_tokens))
                .await
                .is_err()
        );
    }
    assert_eq!(provider.request_count(), Some(0));
    assert_eq!(
        provider
            .complete(ChatRequest::user_prompt("synthetic"))
            .await
            .unwrap()
            .text,
        "ok"
    );
    assert_eq!(provider.request_count(), Some(1));
}
