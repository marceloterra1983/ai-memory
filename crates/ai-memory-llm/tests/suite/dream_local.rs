//! Restricted local inference boundary for dream.

use ai_memory_llm::{ChatRequest, DreamLocalClient, Embedder, OpenAiCompatEmbedder};
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

#[tokio::test]
async fn dream_embedding_local_control_refuses_redirect_retry_and_truncation() {
    for endpoint in ["https://external.invalid/v1", "http://127.0.0.1:8789/v1"] {
        assert!(OpenAiCompatEmbedder::new_local(endpoint, "synthetic", 2).is_err());
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[3.0,4.0]}]})),
        )
        .expect(3)
        .mount(&server)
        .await;
    let embedder = OpenAiCompatEmbedder::new_local(server.uri(), "synthetic", 2).unwrap();
    let vector = embedder
        .embed_document("complete synthetic source")
        .await
        .unwrap();
    assert!((vector[0] - 0.6).abs() < 0.0001);
    assert!((vector[1] - 0.8).abs() < 0.0001);
    assert!(embedder.embed_document(&"x".repeat(8193)).await.is_ok());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.body.len() <= 8192));
    let reconstructed: String = requests[1..]
        .iter()
        .map(|request| {
            let sent: Value = serde_json::from_slice(&request.body).unwrap();
            sent["input"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(reconstructed, "x".repeat(8193));
    assert!(!requests[0].headers.contains_key("authorization"));
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["input"], "complete synthetic source");
    for status in [307, 429] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", "https://external.invalid")
                    .set_body_string("sensitive synthetic response"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let embedder = OpenAiCompatEmbedder::new_local(server.uri(), "synthetic", 2).unwrap();
        let error = embedder.embed("synthetic").await.unwrap_err();
        assert_eq!(error.http_status(), Some(status));
        assert!(!error.to_string().contains("sensitive synthetic"));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
