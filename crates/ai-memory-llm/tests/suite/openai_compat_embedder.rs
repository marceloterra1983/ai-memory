//! Integration tests for `OpenAiCompatEmbedder` against an in-process
//! HTTP mock (wiremock).
//!
//! The load-bearing behaviours: (1) keyless requests carry NO
//! `Authorization` header — local engines such as Ollama and LM Studio
//! reject or ignore malformed auth, and sending a fabricated bearer
//! token would leak the assumption that a key always exists; (2) when
//! a gateway key IS configured it is sent as a bearer token; (3) the
//! embedder identifies as provider `openai-compat`, so stored
//! `{provider, model, dim}` triples are a distinct family from plain
//! `openai`; (4) the factory refuses to build without a base URL.

use ai_memory_llm::{
    Embedder, EmbedderChoice, EmbedderConfig, LlmError, OpenAiCompatEmbedder, build_embedder,
    default_embedding_dim, try_default_embedding_dim,
};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn embedding_body(dim: usize) -> serde_json::Value {
    json!({
        "object": "list",
        "data": [{ "object": "embedding", "index": 0, "embedding": vec![0.5_f32; dim] }],
        "model": "nomic-embed-text",
        "usage": { "prompt_tokens": 1, "total_tokens": 1 },
    })
}

#[tokio::test]
async fn query_cache_reuses_only_queries_and_different_queries_still_infer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let vector = if body["input"] == "query: second" {
                vec![0.0, 1.0]
            } else {
                vec![1.0, 0.0]
            };
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":vector}]}))
        })
        .mount(&server)
        .await;
    let e = OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "model", 2)
        .unwrap()
        .with_prefixes("query: ", "passage: ");
    assert_eq!(e.embed_query("first").await.unwrap(), vec![1.0, 0.0]);
    assert_eq!(e.embed_query("second").await.unwrap(), vec![0.0, 1.0]);
    assert_eq!(e.embed_query("first").await.unwrap(), vec![1.0, 0.0]);
    assert_eq!(
        recorded(&server).await.len(),
        2,
        "an intervening query must not force repeated inference of the first query"
    );
    e.embed_document("first").await.unwrap();
    e.embed_document("first").await.unwrap();
    e.embed("first").await.unwrap();
    assert_eq!(
        recorded(&server).await.len(),
        5,
        "documents are never cached"
    );
}

#[tokio::test]
async fn keyless_embed_sends_no_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            assert!(
                req.headers.get("authorization").is_none(),
                "keyless compat embedder must not send an Authorization header"
            );
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(1)
        .mount(&server)
        .await;

    let e = OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "nomic-embed-text", 8)
        .expect("embedder builds");
    assert_eq!(e.provider(), "openai-compat");
    assert_eq!(e.provider(), EmbedderChoice::OpenAiCompat.name());

    let v = e.embed("hello world").await.expect("embed succeeds");
    assert_eq!(v.len(), 8);
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "expected unit norm, got {norm}");
}

#[tokio::test]
async fn configured_key_is_sent_as_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer sk-gateway"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(8)))
        .expect(1)
        .mount(&server)
        .await;

    let e = OpenAiCompatEmbedder::new(
        format!("{}/v1", server.uri()),
        Some(SecretString::from("sk-gateway")),
        "nomic-embed-text",
        8,
    )
    .expect("embedder builds");
    e.embed("hello").await.expect("embed succeeds");
}

#[tokio::test]
async fn factory_builds_compat_embedder_and_requires_base_url() {
    let ok = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "nomic-embed-text".into(),
        dim: 768,
        api_key: SecretString::from(String::new()),
        base_url: Some("http://localhost:11434/v1".into()),
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
        query_base_url: None,
        local_only: false,
    })
    .expect("factory builds compat embedder");
    assert_eq!(ok.provider(), "openai-compat");
    assert_eq!(ok.dim(), 768);

    let err = match build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "nomic-embed-text".into(),
        dim: 768,
        api_key: SecretString::from(String::new()),
        base_url: None,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
        query_base_url: None,
        local_only: false,
    }) {
        Ok(_) => panic!("compat embedder must not build without a base URL"),
        Err(err) => err,
    };
    assert!(
        matches!(err, LlmError::NotConfigured(ref msg) if msg.contains("AI_MEMORY_EMBEDDING_BASE_URL")),
        "expected NotConfigured for missing base URL, got {err:?}"
    );

    let zero_dim = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "nomic-embed-text".into(),
        dim: 0,
        api_key: SecretString::from(String::new()),
        base_url: Some("http://localhost:11434/v1".into()),
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
        query_base_url: None,
        local_only: false,
    });
    assert!(
        matches!(zero_dim, Err(LlmError::NotConfigured(ref msg)) if msg.contains("greater than zero")),
        "zero-dimensional vectors must fail at the factory boundary"
    );
}

#[tokio::test]
async fn query_prefix_is_sent_with_embed_query_not_embed_document() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
            assert_eq!(
                body["input"], "query: find the runbook",
                "embed_query must send the query prefix ahead of the text"
            );
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(1)
        .mount(&server)
        .await;

    let e = OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "nomic-embed-text", 8)
        .expect("embedder builds")
        .with_prefixes("query: ", "passage: ");

    e.embed_query("find the runbook")
        .await
        .expect("embed_query succeeds");
}

#[tokio::test]
async fn document_prefix_is_sent_with_embed_document_not_embed_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
            assert_eq!(
                body["input"], "passage: the runbook says...",
                "embed_document must send the document prefix ahead of the text"
            );
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(1)
        .mount(&server)
        .await;

    let e = OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "nomic-embed-text", 8)
        .expect("embedder builds")
        .with_prefixes("query: ", "passage: ");

    e.embed_document("the runbook says...")
        .await
        .expect("embed_document succeeds");
}

#[tokio::test]
async fn unset_prefixes_leave_embed_query_and_embed_document_unchanged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
            assert_eq!(
                body["input"], "plain text",
                "an unset prefix (the default) must not change the request body"
            );
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(2)
        .mount(&server)
        .await;

    // No `with_prefixes` call at all — construction alone must be
    // byte-identical to before this feature existed.
    let e = OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "nomic-embed-text", 8)
        .expect("embedder builds");

    e.embed_query("plain text")
        .await
        .expect("embed_query succeeds");
    e.embed_document("plain text")
        .await
        .expect("embed_document succeeds");
}

#[tokio::test]
async fn prefix_is_truncated_together_with_the_text_it_precedes() {
    // Truncation must bound prefix+text together; a prefix must never let
    // the combined request exceed the server's input cap.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
            let input = body["input"].as_str().expect("input is a string");
            assert!(
                input.starts_with("passage: "),
                "prefix must survive truncation: {input:.60}..."
            );
            assert!(
                input.len() <= 8_000,
                "prefix+text must stay within the same hard byte cap as before"
            );
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(1)
        .mount(&server)
        .await;

    let e = OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "nomic-embed-text", 8)
        .expect("embedder builds")
        .with_prefixes("query: ", "passage: ");

    let long_text = "x".repeat(50_000);
    e.embed_document(&long_text)
        .await
        .expect("embed_document succeeds");
}

#[tokio::test]
async fn factory_wires_configured_prefixes_into_the_compat_embedder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
            assert_eq!(body["input"], "query: via factory");
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(1)
        .mount(&server)
        .await;

    let embedder = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "nomic-embed-text".into(),
        dim: 8,
        api_key: SecretString::from(String::new()),
        base_url: Some(format!("{}/v1", server.uri())),
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: "query: ".into(),
        document_prefix: "passage: ".into(),
        query_base_url: None,
        local_only: false,
    })
    .expect("factory builds compat embedder with prefixes");

    embedder
        .embed_query("via factory")
        .await
        .expect("embed_query succeeds");
}

#[test]
fn existing_default_dimension_api_remains_source_compatible() {
    assert_eq!(
        default_embedding_dim(EmbedderChoice::OpenAi, "text-embedding-3-small"),
        1536
    );
    assert_eq!(
        try_default_embedding_dim(EmbedderChoice::OpenAiCompat, "nomic-embed-text"),
        None
    );
}

fn compat_config(
    base_url: Option<String>,
    query_base_url: Option<String>,
    local_only: bool,
) -> EmbedderConfig {
    EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "nomic-embed-text".into(),
        dim: 8,
        api_key: SecretString::from(String::new()),
        base_url,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: "query: ".into(),
        document_prefix: "passage: ".into(),
        query_base_url,
        local_only,
    }
}

async fn mount_embeddings(server: &MockServer, dim: usize) {
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(dim)))
        .mount(server)
        .await;
}

fn request_input(req: &Request) -> String {
    let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
    body["input"].as_str().unwrap_or_default().to_string()
}

fn request_model(req: &Request) -> String {
    let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
    body["model"].as_str().unwrap_or_default().to_string()
}

async fn recorded(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("mock server still recording")
}

#[tokio::test]
async fn query_endpoint_override_is_used_only_by_embed_query() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    mount_embeddings(&queries, 8).await;
    let doc_url = format!("{}/v1", documents.uri());
    let query_url = format!("{}/v1", queries.uri());
    let key = SecretString::from("sk-gateway");
    let baseline = OpenAiCompatEmbedder::new(&doc_url, Some(key.clone()), "nomic-embed-text", 8)
        .expect("baseline builds")
        .with_prefixes("query: ", "passage: ");
    let embedder = OpenAiCompatEmbedder::new(doc_url, Some(key), "nomic-embed-text", 8)
        .expect("embedder builds")
        .with_prefixes("query: ", "passage: ")
        .with_query_base_url(query_url);

    assert_eq!(embedder.provider(), baseline.provider());
    assert_eq!(embedder.model(), baseline.model());
    assert_eq!(embedder.model_identity(), baseline.model_identity());
    assert_eq!(embedder.dim(), baseline.dim());

    embedder
        .embed_document("the runbook")
        .await
        .expect("document embed");
    embedder.embed("symmetric").await.expect("symmetric embed");
    embedder
        .embed_query("find the runbook")
        .await
        .expect("query embed");

    let docs = recorded(&documents).await;
    let query_reqs = recorded(&queries).await;
    assert_eq!(
        query_reqs.len(),
        1,
        "embed_query must use only the override"
    );
    assert_eq!(
        docs.len(),
        2,
        "embed and embed_document must stay on the base endpoint"
    );
    assert_eq!(request_input(&query_reqs[0]), "query: find the runbook");
    assert!(
        docs.iter()
            .any(|req| request_input(req) == "passage: the runbook")
    );
    assert!(docs.iter().any(|req| request_input(req) == "symmetric"));
    for req in docs.iter().chain(query_reqs.iter()) {
        assert_eq!(request_model(req), "nomic-embed-text");
        assert_eq!(
            req.headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sk-gateway")
        );
    }
}

#[tokio::test]
async fn unset_query_endpoint_keeps_both_methods_on_the_base_url() {
    let server = MockServer::start().await;
    mount_embeddings(&server, 8).await;
    let embedder =
        OpenAiCompatEmbedder::new(format!("{}/v1", server.uri()), None, "nomic-embed-text", 8)
            .expect("embedder builds")
            .with_query_base_url("   ");
    embedder.embed_query("plain").await.expect("query");
    embedder.embed_document("plain").await.expect("document");
    let reqs = recorded(&server).await;
    assert_eq!(reqs.len(), 2, "blank override must keep the base route");
    assert!(reqs.iter().all(|req| request_input(req) == "plain"));
}

#[tokio::test]
async fn query_endpoint_failure_does_not_fall_back_to_the_document_endpoint() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("down"))
        .mount(&queries)
        .await;
    mount_embeddings(&documents, 8).await;
    let embedder = OpenAiCompatEmbedder::new(
        format!("{}/v1", documents.uri()),
        None,
        "nomic-embed-text",
        8,
    )
    .expect("embedder builds")
    .with_query_base_url(format!("{}/v1", queries.uri()));

    let err = embedder
        .embed_query("find")
        .await
        .expect_err("query endpoint 500 must fail");
    assert!(
        matches!(err, LlmError::Provider { status: 500, .. }),
        "{err:?}"
    );
    assert_eq!(recorded(&queries).await.len(), 1);
    assert_eq!(
        recorded(&documents).await.len(),
        0,
        "a query failure must not be sent to the document endpoint"
    );
}

#[tokio::test]
async fn query_endpoint_dim_mismatch_does_not_fall_back() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(4)))
        .mount(&queries)
        .await;
    mount_embeddings(&documents, 8).await;
    let embedder = OpenAiCompatEmbedder::new(
        format!("{}/v1", documents.uri()),
        None,
        "nomic-embed-text",
        8,
    )
    .expect("embedder builds")
    .with_query_base_url(format!("{}/v1", queries.uri()));

    let err = embedder
        .embed_query("find")
        .await
        .expect_err("wrong dim must fail");
    assert!(matches!(err, LlmError::UnexpectedShape(_)), "{err:?}");
    assert_eq!(recorded(&queries).await.len(), 1);
    assert_eq!(
        recorded(&documents).await.len(),
        0,
        "a dim mismatch must not be retried on the document endpoint"
    );
}

#[tokio::test]
async fn factory_wires_query_endpoint_without_changing_identity() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    mount_embeddings(&queries, 8).await;
    let routed = build_embedder(compat_config(
        Some(format!("{}/v1", documents.uri())),
        Some(format!("{}/v1", queries.uri())),
        false,
    ))
    .expect("routed embedder");
    let same_route = build_embedder(compat_config(
        Some(format!("{}/v1", documents.uri())),
        None,
        false,
    ))
    .expect("default embedder");
    assert_eq!(routed.provider(), same_route.provider());
    assert_eq!(routed.model(), same_route.model());
    assert_eq!(routed.model_identity(), same_route.model_identity());
    assert_eq!(routed.dim(), same_route.dim());

    routed.embed_query("via factory").await.expect("query");
    routed.embed_document("page").await.expect("document");
    let query_reqs = recorded(&queries).await;
    let docs = recorded(&documents).await;
    assert_eq!(query_reqs.len(), 1);
    assert_eq!(request_input(&query_reqs[0]), "query: via factory");
    assert_eq!(docs.len(), 1);
    assert_eq!(request_input(&docs[0]), "passage: page");
}

#[tokio::test]
async fn factory_dream_local_query_endpoint_keeps_each_document_request_bounded() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    mount_embeddings(&queries, 8).await;
    let embedder = build_embedder(compat_config(
        Some(format!("{}/v1", documents.uri())),
        Some(format!("{}/v1", queries.uri())),
        true,
    ))
    .expect("dream-local embedder");
    embedder
        .embed_document("short page")
        .await
        .expect("short document");
    embedder.embed_query("find").await.expect("query");
    assert_eq!(recorded(&queries).await.len(), 1);
    assert_eq!(recorded(&documents).await.len(), 1);
    embedder.embed_document(&"y".repeat(9_000)).await.unwrap();
    let docs = recorded(&documents).await;
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().all(|req| req.body.len() <= 8192));
    assert_eq!(
        docs[1..]
            .iter()
            .map(|req| request_input(req)
                .strip_prefix("passage: ")
                .unwrap()
                .to_string())
            .collect::<String>(),
        "y".repeat(9_000)
    );
    assert_eq!(
        recorded(&queries).await.len(),
        1,
        "an oversized document must not move onto the query endpoint"
    );
}

#[test]
fn query_base_url_is_rejected_for_other_providers() {
    let refused = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAi,
        model: "text-embedding-3-small".into(),
        dim: 8,
        api_key: SecretString::from("sk-test"),
        base_url: Some("http://127.0.0.1:9/v1".into()),
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
        query_base_url: Some("http://127.0.0.1:18003/v1".into()),
        local_only: false,
    });
    let err = match refused {
        Ok(_) => panic!("non-compat providers must refuse a query endpoint"),
        Err(err) => err,
    };
    assert!(
        matches!(err, LlmError::NotConfigured(ref msg) if msg.contains("EMBEDDING_QUERY_BASE_URL")),
        "{err:?}"
    );

    let unchanged = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAi,
        model: "text-embedding-3-small".into(),
        dim: 8,
        api_key: SecretString::from("sk-test"),
        base_url: None,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
        query_base_url: None,
        local_only: false,
    });
    assert!(
        unchanged.is_ok(),
        "unset query endpoint must leave other providers unchanged"
    );
}

fn dream_local_embedder(
    documents: &MockServer,
    queries: &MockServer,
) -> std::sync::Arc<dyn Embedder> {
    build_embedder(compat_config(
        Some(format!("{}/v1", documents.uri())),
        Some(format!("{}/v1", queries.uri())),
        true,
    ))
    .expect("dream-local embedder with a local query endpoint builds")
}

#[tokio::test]
async fn dream_local_mode_refuses_a_remote_query_endpoint_in_the_factory() {
    let err = match build_embedder(compat_config(
        Some("http://127.0.0.1:8002/v1".into()),
        Some("https://api.openai.com/v1".into()),
        true,
    )) {
        Ok(_) => panic!("dream-local mode must refuse a remote query endpoint"),
        Err(err) => err,
    };
    assert!(
        matches!(err, LlmError::NotConfigured(ref msg) if msg.contains("loopback")),
        "{err:?}"
    );
}

#[tokio::test]
async fn new_local_with_a_remote_query_endpoint_is_refused_before_any_network() {
    let documents = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    let embedder =
        OpenAiCompatEmbedder::new_local(format!("{}/v1", documents.uri()), "nomic-embed-text", 8)
            .expect("local embedder builds")
            .with_query_base_url("http://localhost:18003/v1");
    let err = match embedder.embed_query("find").await {
        Ok(_) => panic!("a remote query endpoint must be refused before any request"),
        Err(err) => err,
    };
    assert!(
        matches!(err, LlmError::NotConfigured(ref msg) if msg.contains("loopback")),
        "{err:?}"
    );
    assert_eq!(
        recorded(&documents).await.len(),
        0,
        "the refusal must happen before the document endpoint is touched"
    );
}

#[tokio::test]
async fn dream_local_query_endpoint_is_never_retried_on_429() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    // A retry would succeed here: the second answer is a valid embedding.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .up_to_n_times(1)
        .mount(&queries)
        .await;
    mount_embeddings(&queries, 8).await;
    let embedder = dream_local_embedder(&documents, &queries);
    let err = embedder
        .embed_query("find")
        .await
        .expect_err("dream-local queries must not be retried on 429");
    assert!(
        matches!(err, LlmError::Provider { status: 429, .. }),
        "{err:?}"
    );
    assert_eq!(
        recorded(&queries).await.len(),
        1,
        "exactly one request may reach the query endpoint"
    );
}

#[tokio::test]
async fn dream_local_query_endpoint_does_not_follow_a_redirect() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    let mut redirect = ResponseTemplate::new(302);
    redirect = redirect.insert_header("location", format!("{}/elsewhere", queries.uri()));
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(redirect)
        .expect(1)
        .mount(&queries)
        .await;
    let embedder = dream_local_embedder(&documents, &queries);
    let err = embedder
        .embed_query("find")
        .await
        .expect_err("dream-local queries must not follow a redirect");
    assert!(
        matches!(err, LlmError::Provider { status: 302, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn dream_local_query_endpoint_refuses_oversized_input_like_documents() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    mount_embeddings(&queries, 8).await;
    let embedder = dream_local_embedder(&documents, &queries);
    embedder
        .embed_query("short query")
        .await
        .expect("short query embeds");
    let err = embedder
        .embed_query(&"y".repeat(9_000))
        .await
        .expect_err("the dream-local byte cap must bound queries too");
    assert!(matches!(err, LlmError::Schema(_)), "{err:?}");
    assert_eq!(
        recorded(&queries).await.len(),
        1,
        "the oversized query must be refused before reaching the endpoint"
    );
}

#[tokio::test]
async fn dream_local_query_endpoint_refuses_a_degenerate_vector_like_documents() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"embedding": vec![0.0_f32; 8]}]})),
        )
        .mount(&queries)
        .await;
    let embedder = dream_local_embedder(&documents, &queries);
    let err = embedder
        .embed_query("find")
        .await
        .expect_err("a zero-norm vector must be refused like a document embed");
    assert!(matches!(err, LlmError::UnexpectedShape(_)), "{err:?}");
}

#[tokio::test]
async fn dream_local_query_dim_mismatch_does_not_fall_back_to_documents() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(4)))
        .mount(&queries)
        .await;
    let embedder = dream_local_embedder(&documents, &queries);
    let err = embedder
        .embed_query("find")
        .await
        .expect_err("wrong dim must fail");
    assert!(matches!(err, LlmError::UnexpectedShape(_)), "{err:?}");
    assert_eq!(recorded(&queries).await.len(), 1);
    assert_eq!(
        recorded(&documents).await.len(),
        0,
        "a dim mismatch must not be retried on the document endpoint"
    );
}

#[tokio::test]
async fn dream_local_query_endpoint_stays_keyless() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    mount_embeddings(&documents, 8).await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            assert!(
                req.headers.get("authorization").is_none(),
                "dream-local queries must stay credential-free"
            );
            ResponseTemplate::new(200).set_body_json(embedding_body(8))
        })
        .expect(1)
        .mount(&queries)
        .await;
    let embedder = dream_local_embedder(&documents, &queries);
    embedder
        .embed_query("find")
        .await
        .expect("keyless dream-local query embeds");
}
