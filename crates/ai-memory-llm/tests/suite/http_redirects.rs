//! Credentials and private request bodies must stay at the configured endpoint.

use ai_memory_llm::{
    AnthropicProvider, Embedder, GeminiProvider, GoogleEmbedder, LlmProvider, OpenAiCompatEmbedder,
    OpenAiEmbedder, OpenAiProvider, VoyageEmbedder,
};
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn key() -> SecretString {
    SecretString::from("redirect-fixture-key")
}

fn completion() -> Value {
    json!({
        "id":"fixture", "model":"fixture",
        "content":[{"type":"text","text":"ok"}],
        "choices":[{"message":{"role":"assistant","content":"ok"}}],
        "candidates":[{"content":{"parts":[{"text":"ok"}]}}],
        "usage":{"input_tokens":1,"output_tokens":1,"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
    })
}

#[tokio::test]
async fn completion_redirects_refuse_credentials_and_private_prompts() {
    for kind in ["anthropic", "anthropic-oauth", "openai", "gemini"] {
        for status in [302, 307, 308] {
            let target = MockServer::start().await;
            let origin = MockServer::start().await;
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200).set_body_json(completion()))
                .mount(&target)
                .await;
            Mock::given(any())
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("Location", target.uri())
                        // A valid success body must not turn a 3xx into success.
                        .set_body_json(completion()),
                )
                .mount(&origin)
                .await;
            let provider: Box<dyn LlmProvider> = match kind {
                "anthropic" => Box::new(
                    AnthropicProvider::new(key(), "fixture")
                        .unwrap()
                        .with_base_url(origin.uri())
                        .with_extra_headers(
                            ai_memory_llm::ExtraHeaders::parse([
                                "x-fixture-key: extra-private-key",
                            ])
                            .unwrap(),
                        ),
                ),
                "anthropic-oauth" => Box::new(
                    AnthropicProvider::new_oauth(key(), "fixture")
                        .unwrap()
                        .with_base_url(origin.uri())
                        .with_extra_headers(
                            ai_memory_llm::ExtraHeaders::parse([
                                "x-fixture-key: extra-private-key",
                            ])
                            .unwrap(),
                        ),
                ),
                "openai" => Box::new(
                    OpenAiProvider::new(key(), "fixture")
                        .unwrap()
                        .with_base_url(origin.uri())
                        .with_extra_headers(
                            ai_memory_llm::ExtraHeaders::parse([
                                "x-fixture-key: extra-private-key",
                            ])
                            .unwrap(),
                        ),
                ),
                "gemini" => Box::new(
                    GeminiProvider::new(key(), "fixture")
                        .unwrap()
                        .with_base_url(origin.uri())
                        .with_extra_headers(
                            ai_memory_llm::ExtraHeaders::parse([
                                "x-fixture-key: extra-private-key",
                            ])
                            .unwrap(),
                        ),
                ),
                _ => unreachable!(),
            };
            let result = provider
                .complete(ai_memory_llm::ChatRequest::user_prompt("private prompt"))
                .await;
            assert!(result.is_err(), "{kind} accepted HTTP {status}: {result:?}");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(&status.to_string())
            );
            assert!(
                target.received_requests().await.unwrap().is_empty(),
                "{kind} forwarded HTTP {status}"
            );
            let requests = origin.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert_eq!(
                request.headers["x-fixture-key"].to_str().unwrap(),
                "extra-private-key"
            );
            let auth_header = match kind {
                "anthropic" => "x-api-key",
                "gemini" => "x-goog-api-key",
                _ => "authorization",
            };
            assert!(
                request.headers[auth_header]
                    .to_str()
                    .unwrap()
                    .contains("redirect-fixture-key")
            );
            assert!(String::from_utf8_lossy(&request.body).contains("private prompt"));
            origin.reset().await;
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200).set_body_json(completion()))
                .mount(&origin)
                .await;
            assert!(
                provider
                    .complete(ai_memory_llm::ChatRequest::user_prompt(
                        "legitimate control"
                    ))
                    .await
                    .is_ok(),
                "{kind}: direct 200 must work"
            );
        }
    }
}

#[tokio::test]
async fn embedding_redirects_refuse_credentials_and_private_documents() {
    for kind in ["openai", "compat", "voyage", "google"] {
        for status in [302, 307, 308] {
            let target = MockServer::start().await;
            let origin = MockServer::start().await;
            let body =
                json!({"data":[{"embedding":[1.0,0.0,0.0]}],"embedding":{"values":[1.0,0.0,0.0]}});
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                .mount(&target)
                .await;
            Mock::given(any())
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("Location", target.uri())
                        .set_body_json(&body),
                )
                .mount(&origin)
                .await;
            let embedder: Box<dyn Embedder> = match kind {
                "openai" => Box::new(
                    OpenAiEmbedder::new(key(), "fixture", 3)
                        .unwrap()
                        .with_base_url(origin.uri()),
                ),
                "compat" => Box::new(
                    OpenAiCompatEmbedder::new(origin.uri(), Some(key()), "fixture", 3).unwrap(),
                ),
                "voyage" => Box::new(
                    VoyageEmbedder::new(key(), "fixture", 3)
                        .unwrap()
                        .with_base_url(origin.uri()),
                ),
                "google" => Box::new(
                    GoogleEmbedder::new(key(), "fixture", 3)
                        .unwrap()
                        .with_base_url(origin.uri()),
                ),
                _ => unreachable!(),
            };
            let result = embedder.embed_document("private document").await;
            assert!(result.is_err(), "{kind} accepted HTTP {status}: {result:?}");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(&status.to_string())
            );
            assert!(
                target.received_requests().await.unwrap().is_empty(),
                "{kind} forwarded HTTP {status}"
            );
            origin.reset().await;
            Mock::given(any())
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&origin)
                .await;
            assert_eq!(
                embedder
                    .embed_document("legitimate control")
                    .await
                    .unwrap()
                    .len(),
                3
            );
        }
    }
}

/// Exercise private provider constructors without adding production test seams.
pub(crate) async fn assert_client_refuses_redirects(client: reqwest::Client) {
    for status in [302, 307, 308] {
        let target = MockServer::start().await;
        let origin = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"access_token":"control"})),
            )
            .mount(&target)
            .await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("Location", target.uri())
                    .set_body_json(json!({"access_token":"forged-success"})),
            )
            .mount(&origin)
            .await;
        let result: crate::LlmResult<Value> = crate::stored_token::refresh_grant(
            &client,
            &origin.uri(),
            "private-refresh-token",
            "fixture-client",
            "refresh failed",
            "retry login",
        )
        .await;
        assert!(
            result.is_err(),
            "OAuth refresh accepted HTTP {status}: {result:?}"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains(&status.to_string())
        );
        assert!(
            target.received_requests().await.unwrap().is_empty(),
            "refresh body followed HTTP {status}"
        );
        origin.reset().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"access_token":"control"})),
            )
            .mount(&origin)
            .await;
        let result: Value = crate::stored_token::refresh_grant(
            &client,
            &origin.uri(),
            "private-refresh-token",
            "fixture-client",
            "refresh failed",
            "retry login",
        )
        .await
        .unwrap();
        assert_eq!(result["access_token"], "control");
        let requests = origin.received_requests().await.unwrap();
        assert!(String::from_utf8_lossy(&requests[0].body).contains("private-refresh-token"));
    }
}
