//! Full-content local embeddings, with synthetic sources only.

use ai_memory_llm::{Embedder, LlmError, OpenAiCompatEmbedder};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let input = body["input"].as_str().unwrap();
            let vector = if input.contains('終') {
                [0.0, 1.0]
            } else {
                [1.0, 0.0]
            };
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":vector}]}))
        })
        .mount(&server)
        .await;
    server
}

fn embedder(server: &MockServer) -> OpenAiCompatEmbedder {
    OpenAiCompatEmbedder::new_local(server.uri(), "synthetic", 2).unwrap()
}

async fn inputs(server: &MockServer, prefix: &str) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|req| {
            assert!(
                req.body.len() <= 8192,
                "every encoded request keeps the guard"
            );
            assert!(!req.headers.contains_key("authorization"));
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["model"], "synthetic");
            body["input"]
                .as_str()
                .unwrap()
                .strip_prefix(prefix)
                .unwrap()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn local_document_chunks_preserve_all_utf8_escaped_text_and_prefixes() {
    let server = server().await;
    let e = embedder(&server).with_prefixes("query: ", "passage: \"\n");
    let identity = e.model_identity();
    let text = format!("{}{}", "é🙂\"\\\n\t\u{0001}".repeat(3000), "終".repeat(100));
    let vector = e
        .embed_document(&text)
        .await
        .expect("all source chunks embed");
    let sent = inputs(&server, "passage: \"\n").await;
    assert!(sent.len() > 2);
    assert_eq!(
        sent.concat(),
        text,
        "no missing, duplicated or rewritten source bytes"
    );
    assert!(sent.iter().all(|s| !s.is_empty()));
    assert!(
        vector[1] > 0.0,
        "text beyond the first window must influence the vector"
    );
    assert!((vector.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 0.00001);
    assert_eq!(e.model_identity(), identity);
    assert_eq!(e.provider(), "openai-compat");
    assert_eq!(e.dim(), 2);
}

#[tokio::test]
async fn local_raw_embedding_pools_by_source_bytes_and_includes_tail() {
    let server = server().await;
    let text = format!("{}{}", "a".repeat(9000), "終".repeat(100));
    let vector = embedder(&server).embed(&text).await.unwrap();
    let sent = inputs(&server, "").await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent.concat(), text);
    let norm = ((sent[0].len() as f64).powi(2) + (sent[1].len() as f64).powi(2)).sqrt();
    assert!((vector[0] as f64 - sent[0].len() as f64 / norm).abs() < 0.00001);
    assert!((vector[1] as f64 - sent[1].len() as f64 / norm).abs() < 0.00001);
}

#[tokio::test]
async fn local_document_exact_wire_limit_then_one_byte_over_is_lossless() {
    let server = server().await;
    let e = embedder(&server);
    let overhead = serde_json::to_vec(&json!({"model":"synthetic","input":""}))
        .unwrap()
        .len();
    let at_cap = "a".repeat(8192 - overhead);
    assert_eq!(e.embed_document(&at_cap).await.unwrap(), vec![1.0, 0.0]);
    assert_eq!(
        server.received_requests().await.unwrap()[0].body.len(),
        8192
    );
    let over = format!("{at_cap}b");
    e.embed_document(&over).await.unwrap();
    let sent = inputs(&server, "").await;
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[1..].concat(), over);
}

#[tokio::test]
async fn local_oversized_prefix_refuses_before_sending_any_source() {
    let server = server().await;
    let e = embedder(&server).with_prefixes("", "\n".repeat(5000));
    let err = e.embed_document("source").await.unwrap_err();
    assert!(matches!(err, LlmError::Schema(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn local_prefix_must_leave_room_for_a_complete_utf8_character() {
    let server = server().await;
    let overhead = serde_json::to_vec(&json!({"model":"synthetic","input":""}))
        .unwrap()
        .len();
    let e = embedder(&server).with_prefixes("", "a".repeat(8192 - overhead - 1));
    assert!(matches!(
        e.embed_document("é").await,
        Err(LlmError::Schema(_))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(e.embed_document("x").await.unwrap(), vec![1.0, 0.0]);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn local_late_transport_failures_are_redacted_and_never_retried() {
    for (status, response) in [
        (200, "private synthetic malformed {"),
        (200, "[\"private synthetic response\"]"),
        (429, "private synthetic rate-limit response"),
        (307, "private synthetic redirect response"),
    ] {
        let server = MockServer::start().await;
        let target = MockServer::start().await;
        let redirect = target.uri();
        Mock::given(method("POST"))
            .respond_with(move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                if body["input"].as_str().unwrap().contains('終') {
                    ResponseTemplate::new(status)
                        .insert_header("location", redirect.clone())
                        .set_body_string(response)
                } else {
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]}))
                }
            })
            .mount(&server)
            .await;
        let err = embedder(&server)
            .embed_document(&format!("{}終", "a".repeat(9000)))
            .await
            .unwrap_err();
        if status == 200 {
            assert!(matches!(err, LlmError::UnexpectedShape(_)));
        } else {
            assert_eq!(err.http_status(), Some(status));
        }
        assert!(!err.to_string().contains("private synthetic"));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        assert!(target.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn local_late_invalid_chunk_returns_error_without_partial_vector_or_retry() {
    for bad in [
        json!({"data":[]}),
        json!({"data":[{"embedding":[1.0,0.0]},{"embedding":[1.0,0.0]}]}),
        json!({"data":[{"embedding":[1.0]}]}),
        json!({"data":[{"embedding":["private synthetic value",0.0]}]}),
        json!({"data":[{"embedding":[1e100,0.0]}]}),
        json!({"data":[{"embedding":[0.0,0.0]}]}),
        json!({"data":[{"embedding":[1e30,1e30]}]}),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let response = if body["input"].as_str().unwrap().contains('終') {
                    bad.clone()
                } else {
                    json!({"data":[{"embedding":[1.0,0.0]}]})
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&server)
            .await;
        let err = embedder(&server)
            .embed_document(&format!("{}終", "a".repeat(9000)))
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::UnexpectedShape(_)));
        assert!(!err.to_string().contains("private synthetic value"));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn local_cancelling_chunk_directions_refuse_degenerate_pool() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let value = if body["input"].as_str().unwrap().starts_with('b') {
                -1.0
            } else {
                1.0
            };
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[value,0.0]}]}))
        })
        .mount(&server)
        .await;
    let overhead = serde_json::to_vec(&json!({"model":"synthetic","input":""}))
        .unwrap()
        .len();
    let n = 8192 - overhead;
    let err = embedder(&server)
        .embed_document(&format!("{}{}", "a".repeat(n), "b".repeat(n)))
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::UnexpectedShape(_)));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
