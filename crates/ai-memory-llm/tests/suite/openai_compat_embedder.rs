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
//! `openai`; (4) the factory refuses to build without a base URL;
//! (5) the embeddings request never carries the chat provider's
//! `x-request-id` header — the embedder is a separate type with no
//! operation id to send.

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
async fn factory_built_compat_embedder_sends_no_request_id_header() {
    // The openai-compat chat path injects `x-request-id` (the logical
    // operation id) on every chat attempt; the embedder must never carry
    // that header. A refactor that shares the chat header hook with the
    // embedder breaks this assertion.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            assert!(
                req.headers.get("x-request-id").is_none(),
                "the compat embedder must not carry the chat provider's x-request-id header"
            );
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
        query_base_url: None,
        document_timeout_secs: 120,
        local_only: false,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
    })
    .expect("factory builds compat embedder");

    embedder.embed("hello").await.expect("embed succeeds");
}

#[tokio::test]
async fn factory_builds_compat_embedder_and_requires_base_url() {
    let ok = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "nomic-embed-text".into(),
        dim: 768,
        api_key: SecretString::from(String::new()),
        base_url: Some("http://localhost:11434/v1".into()),
        query_base_url: None,
        document_timeout_secs: 120,
        local_only: false,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
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
        query_base_url: None,
        document_timeout_secs: 120,
        local_only: false,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
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
        query_base_url: None,
        document_timeout_secs: 120,
        local_only: false,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
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
        query_base_url: None,
        document_timeout_secs: 120,
        local_only: false,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: "query: ".into(),
        document_prefix: "passage: ".into(),
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

/// The operator's document fallback must never move query inference or the
/// identity used to compare previously stored document vectors.
#[tokio::test]
async fn query_endpoint_is_separate_from_documents_and_identity() {
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(8)))
        .expect(1)
        .mount(&documents)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(8)))
        .expect(1)
        .mount(&queries)
        .await;
    let embedder = build_embedder(EmbedderConfig {
        provider: EmbedderChoice::OpenAiCompat,
        model: "fixture".into(),
        dim: 8,
        api_key: SecretString::from(String::new()),
        base_url: Some(documents.uri() + "/v1"),
        query_base_url: Some(queries.uri() + "/v1"),
        document_timeout_secs: 120,
        local_only: false,
        models_dir: None,
        copilot_auth: None,
        defaulted: false,
        query_prefix: String::new(),
        document_prefix: String::new(),
    })
    .unwrap();
    assert_eq!(embedder.model_identity(), "fixture");
    embedder.embed_document("document").await.unwrap();
    embedder.embed_query("query").await.unwrap();
}

#[tokio::test]
async fn compat_document_timeout_covers_response_body_but_queries_keep_their_budget() {
    use std::io::{Read, Write};
    let listener = (59100..59200)
        .find_map(|port| std::net::TcpListener::bind(("127.0.0.1", port)).ok())
        .expect("isolated high port");
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            let body = embedding_body(8).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1150));
            let _ = socket.write_all(body.as_bytes());
        }
    });
    let embedder = OpenAiCompatEmbedder::new(format!("http://{address}/v1"), None, "fixture", 8)
        .unwrap()
        .with_document_timeout_secs(1)
        .unwrap();
    assert!(embedder.embed_document("document").await.is_err());
    assert!(embedder.embed_query("query").await.is_ok());
    server.join().unwrap();
}

#[tokio::test]
async fn local_documents_cover_all_utf8_source_and_pool_by_source_bytes() {
    use std::sync::{Arc, Mutex};
    let documents = MockServer::start().await;
    let queries = MockServer::start().await;
    let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = chunks.clone();
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |request: &Request| {
            assert!(request.body.len() <= 8192);
            assert!(request.headers.get("authorization").is_none());
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let source = body["input"]
                .as_str()
                .unwrap()
                .strip_prefix("passage: ")
                .unwrap();
            recorded.lock().unwrap().push(source.to_string());
            let vector = if source.ends_with("TAIL") {
                [0.0, 1.0]
            } else {
                [1.0, 0.0]
            };
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":vector}]}))
        })
        .mount(&documents)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"embedding":[1.0,0.0]}]})),
        )
        .expect(1)
        .mount(&queries)
        .await;
    let text = "界\"\\\n".repeat(3000) + "TAIL";
    let embedder = OpenAiCompatEmbedder::new_local(documents.uri() + "/v1", "fixture", 2)
        .unwrap()
        .with_prefixes("query: ", "passage: ")
        .with_query_base_url(queries.uri() + "/v1");
    let identity = embedder.model_identity();
    let vector = embedder.embed_document(&text).await.unwrap();
    embedder.embed_query("where is TAIL?").await.unwrap();
    assert_eq!(embedder.model_identity(), identity);
    let chunks = chunks.lock().unwrap();
    assert!(chunks.len() > 1);
    assert_eq!(chunks.concat(), text);
    let tail_bytes = chunks.last().unwrap().len() as f32;
    let head_bytes = (text.len() - chunks.last().unwrap().len()) as f32;
    let norm = (tail_bytes * tail_bytes + head_bytes * head_bytes).sqrt();
    assert!((vector[0] - head_bytes / norm).abs() < 1e-5);
    assert!((vector[1] - tail_bytes / norm).abs() < 1e-5);
}

#[test]
fn restricted_local_embedding_refuses_nonliteral_or_credentialed_endpoints() {
    for endpoint in [
        "http://localhost:59100/v1",
        "https://127.0.0.1:59100/v1",
        "http://127.1:59100/v1",
        "http://user:secret@127.0.0.1:59100/v1",
        "http://192.0.2.1:59100/v1",
        "http://127.0.0.1:8789/v1",
        "http://127.0.0.1:59100/v1?token=secret",
    ] {
        assert!(OpenAiCompatEmbedder::new_local(endpoint, "fixture", 2).is_err());
    }
    assert!(OpenAiCompatEmbedder::new_local("http://127.0.0.1:59100/v1", "fixture", 2).is_ok());
}

#[tokio::test]
async fn query_vector_cache_is_per_instance_and_never_caches_documents() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(8)))
        .expect(6)
        .mount(&server)
        .await;
    let url = server.uri() + "/v1";
    let embedder = OpenAiCompatEmbedder::new(&url, None, "fixture", 8).unwrap();
    embedder.embed_query("same text").await.unwrap();
    embedder.embed_query("same text").await.unwrap();
    embedder.embed_document("same text").await.unwrap();
    embedder.embed_document("same text").await.unwrap();
    embedder.embed_query("Different text").await.unwrap();
    let second = OpenAiCompatEmbedder::new(&url, None, "fixture", 8).unwrap();
    second.embed_query("same text").await.unwrap();
    let embedder = embedder.with_query_base_url(&url);
    embedder.embed_query("same text").await.unwrap();
}
