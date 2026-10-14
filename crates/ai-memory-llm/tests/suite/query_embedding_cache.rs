//! Query-vector reuse must never cross configuration or transport boundaries.

use ai_memory_llm::{Embedder, OpenAiCompatEmbedder};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

async fn endpoint() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let vector = match (body["model"].as_str(), body["input"].as_str()) {
                (Some("wide"), _) => vec![0.0, 0.0, 1.0],
                (Some("other"), _) | (_, Some("prefix: same")) => vec![0.0, 1.0],
                _ => vec![1.0, 0.0],
            };
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":vector}]}))
        })
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn query_cache_instances_models_dimensions_and_prefixes_are_isolated() {
    let server = endpoint().await;
    let first = OpenAiCompatEmbedder::new_local(server.uri(), "model", 2).unwrap();
    assert_eq!(first.embed_query("same").await.unwrap(), vec![1.0, 0.0]);
    assert_eq!(first.embed_query("same").await.unwrap(), vec![1.0, 0.0]);
    // Even the same coordinate in a separate instance must infer independently.
    let second = OpenAiCompatEmbedder::new_local(server.uri(), "model", 2).unwrap();
    assert_eq!(second.embed_query("same").await.unwrap(), vec![1.0, 0.0]);
    let other = OpenAiCompatEmbedder::new_local(server.uri(), "other", 2).unwrap();
    assert_eq!(other.embed_query("same").await.unwrap(), vec![0.0, 1.0]);
    let wide = OpenAiCompatEmbedder::new_local(server.uri(), "wide", 3).unwrap();
    assert_eq!(wide.embed_query("same").await.unwrap(), vec![0.0, 0.0, 1.0]);
    let changed = first.with_prefixes("prefix: ", "document: ");
    assert_eq!(changed.embed_query("same").await.unwrap(), vec![0.0, 1.0]);
    // Clearing the query prefix must not recover the old cache's entry either.
    let cleared = changed.with_prefixes("", "");
    assert_eq!(cleared.embed_query("same").await.unwrap(), vec![1.0, 0.0]);
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

#[tokio::test]
async fn query_cache_endpoint_change_cannot_bypass_local_validation() {
    let source = endpoint().await;
    let target = endpoint().await;
    let e = OpenAiCompatEmbedder::new_local(source.uri(), "model", 2).unwrap();
    e.embed_query("same").await.unwrap();
    e.embed_query("same").await.unwrap();
    let e = e.with_query_base_url(target.uri());
    e.embed_query("same").await.unwrap();
    assert_eq!(source.received_requests().await.unwrap().len(), 1);
    assert_eq!(target.received_requests().await.unwrap().len(), 1);
    let e = e.with_query_base_url("http://localhost:8003/v1");
    assert!(
        e.embed_query("same").await.is_err(),
        "a cached vector must not bypass literal-loopback validation"
    );
    assert_eq!(target.received_requests().await.unwrap().len(), 1);
    let e = e.with_query_base_url("");
    e.embed_query("same").await.unwrap();
    assert_eq!(source.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn query_cache_failures_and_invalid_local_vectors_never_poison_recovery() {
    for response in [
        ResponseTemplate::new(500),
        ResponseTemplate::new(429),
        ResponseTemplate::new(200).set_body_string("malformed"),
        ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[1.0]}]})),
        ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[0.0,0.0]}]})),
        ResponseTemplate::new(200).set_body_string("{\"data\":[{\"embedding\":[1e100,1]}]}"),
    ] {
        let server = MockServer::start().await;
        let refusal = Mock::given(method("POST"))
            .respond_with(response)
            .mount_as_scoped(&server)
            .await;
        let e = OpenAiCompatEmbedder::new_local(server.uri(), "model", 2).unwrap();
        assert!(e.embed_query("same").await.is_err());
        assert!(e.embed_query("same").await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        drop(refusal);
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]})),
            )
            .mount(&server)
            .await;
        assert_eq!(e.embed_query("same").await.unwrap(), vec![1.0, 0.0]);
        assert_eq!(e.embed_query("same").await.unwrap(), vec![1.0, 0.0]);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }
}

#[tokio::test]
async fn query_cache_does_not_retain_invalid_ordinary_vectors() {
    // The ordinary compat parser historically returns a zero/nonfinite vector;
    // cache admission must remain stricter without changing that parser here.
    for body in [
        "{\"data\":[{\"embedding\":[0,0]}]}",
        "{\"data\":[{\"embedding\":[1e-38,1e-38]}]}",
        "{\"data\":[{\"embedding\":[1e100,1]}]}",
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let e = OpenAiCompatEmbedder::new(server.uri(), None, "model", 2).unwrap();
        let _ = e.embed_query("same").await;
        let _ = e.embed_query("same").await;
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}
