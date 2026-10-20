//! Credential-free, bounded local HTTP inference for dream.
//!
//! A separate client keeps the ordinary providers' proxy, redirect, auth and
//! retry policies out of the restricted dream path.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{ChatRequest, ChatResponse, LlmError, LlmProvider, LlmResult, Usage};

/// Conservative input estimate: one UTF-8 wire byte is charged as one token.
pub const DREAM_LOCAL_MAX_INPUT_BYTES: usize = 8192;
/// Maximum generated tokens on a dream completion.
pub const DREAM_LOCAL_MAX_OUTPUT_TOKENS: u32 = 4000;
/// Default bound for a local request, including its response. Chat, semantic
/// judgments and queries keep it; document embeddings have a separate budget.
pub const DREAM_LOCAL_REQUEST_TIMEOUT_SECS: u64 = 120;
const MAX_RESPONSE_BYTES: usize = 128 * 1024;

/// Parse a credential-free HTTP endpoint on an explicit literal loopback port.
///
/// DNS names, userinfo, query strings, fragments and the excluded 8789 service
/// are refused before any connection. Errors never repeat the supplied URL.
///
/// # Errors
/// Returns a redacted configuration error for any non-local endpoint.
pub fn validate_dream_local_endpoint(endpoint: &str) -> LlmResult<reqwest::Url> {
    let refused = || {
        LlmError::NotConfigured("dream requires a literal loopback HTTP endpoint without credentials; port 8789 is excluded".into())
    };
    let url = reqwest::Url::parse(endpoint).map_err(|_| refused())?;
    // `Url` normalizes IPv4 aliases (127.1, integer addresses) before exposing
    // host(), so require the operator's authority to spell the allowed literal.
    let authority = endpoint
        .strip_prefix("http://")
        .and_then(|s| s.split('/').next())
        .unwrap_or_default();
    let allowed_authority = authority.starts_with("127.0.0.1:") || authority.starts_with("[::1]:");
    let local_host = url
        .host_str()
        .is_some_and(|host| host == "127.0.0.1" || host == "[::1]");
    if url.scheme() != "http"
        || !allowed_authority
        || !local_host
        || url.port().is_none_or(|port| port == 0 || port == 8789)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(refused());
    }
    Ok(url)
}

/// Shared restricted transport for local merge and local semantic judgments.
#[derive(Debug, Clone)]
pub struct DreamLocalClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    timeout: Duration,
    requests: Arc<AtomicUsize>,
}

impl DreamLocalClient {
    /// Build a credential-free client with no proxies, redirects or retries.
    ///
    /// # Errors
    /// Refuses non-local endpoints or a failed HTTP client construction.
    pub fn new(endpoint: &str) -> LlmResult<Self> {
        let endpoint = validate_dream_local_endpoint(endpoint)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| {
                LlmError::NotConfigured("dream local client construction failed".into())
            })?;
        Ok(Self {
            client,
            endpoint,
            timeout: Duration::from_secs(DREAM_LOCAL_REQUEST_TIMEOUT_SECS),
            requests: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Number of HTTP attempts admitted after local validation and input caps.
    #[must_use]
    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    /// Send exactly one bounded request. Errors and response parsing are redacted.
    ///
    /// The caller checks cancellation before admission. Dropping this future
    /// stops waiting locally; the admitted request may finish in the backend.
    ///
    /// # Errors
    /// Refuses oversized input, transport/status errors, oversized responses,
    /// malformed JSON and a response that is not an object.
    pub async fn post_json(&self, body: &Value) -> LlmResult<Value> {
        self.post_json_with_timeout(body, self.timeout).await
    }

    pub(crate) async fn post_embedding_document_json(
        &self,
        body: &Value,
        timeout: Duration,
    ) -> LlmResult<Value> {
        if timeout.is_zero()
            || timeout > Duration::from_secs(crate::embedding::MAX_EMBEDDING_DOCUMENT_TIMEOUT_SECS)
        {
            return Err(LlmError::NotConfigured(
                "dream document timeout must be positive and at most 1800 seconds".into(),
            ));
        }
        self.post_json_with_timeout(body, timeout).await
    }

    async fn post_json_with_timeout(&self, body: &Value, timeout: Duration) -> LlmResult<Value> {
        let wire = serde_json::to_vec(body)
            .map_err(|_| LlmError::Schema("dream request serialization failed".into()))?;
        if wire.len() > DREAM_LOCAL_MAX_INPUT_BYTES {
            return Err(LlmError::Schema("dream input exceeds the conservative 8192-token budget (one UTF-8 wire byte per token)".into()));
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .post(self.endpoint.clone())
            .timeout(timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(wire)
            .send()
            .await
            .map_err(|_| {
                LlmError::UnexpectedShape("dream local request failed or timed out".into())
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(LlmError::Provider {
                status: status.as_u16(),
                body: "dream local endpoint refused request; response body omitted".into(),
            });
        }
        let bytes = crate::response::response_bytes_limited(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| {
                LlmError::UnexpectedShape(
                    "dream local response failed or exceeded its byte limit".into(),
                )
            })?;
        let parsed: Value = serde_json::from_slice(&bytes).map_err(|_| {
            LlmError::UnexpectedShape("dream local response is invalid JSON".into())
        })?;
        if !parsed.is_object() {
            return Err(LlmError::UnexpectedShape(
                "dream local response must be an object".into(),
            ));
        }
        Ok(parsed)
    }
}

/// Local OpenAI-compatible dream provider without provider-chain fallback.
#[derive(Debug)]
pub struct DreamLocalProvider {
    client: DreamLocalClient,
    model: String,
}

impl DreamLocalProvider {
    /// Construct from explicit endpoint/model; no primary-provider auth is read.
    ///
    /// # Errors
    /// Refuses an empty model and any endpoint outside the local boundary.
    pub fn new(endpoint: impl AsRef<str>, model: impl Into<String>) -> LlmResult<Self> {
        validate_dream_local_endpoint(endpoint.as_ref())?;
        let endpoint = crate::openai::normalize_openai_base(endpoint.as_ref(), "chat/completions");
        let model = model.into();
        if model.trim().is_empty() || model.len() > 200 {
            return Err(LlmError::NotConfigured(
                "dream local model must be explicit".into(),
            ));
        }
        Ok(Self {
            client: DreamLocalClient::new(&endpoint)?,
            model,
        })
    }

    async fn request(
        &self,
        request: ChatRequest,
        schema: Option<Value>,
    ) -> LlmResult<ChatResponse> {
        if request.max_tokens == 0 || request.max_tokens > DREAM_LOCAL_MAX_OUTPUT_TOKENS {
            return Err(LlmError::Schema(
                "dream output must be between 1 and 4000 tokens".into(),
            ));
        }
        let mut messages = Vec::new();
        if let Some(system) = request.system {
            messages.push(json!({"role":"system","content":system}));
        }
        messages.extend(
            request
                .messages
                .into_iter()
                .map(|message| json!({"role":message.role.as_str(),"content":message.content})),
        );
        let mut body = json!({"model":self.model,"messages":messages,"max_tokens":request.max_tokens,"temperature":request.temperature.unwrap_or(0.1)});
        if let Some(mut schema) = schema {
            crate::openai::enforce_strict_object_schemas(&mut schema);
            body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"dream_merge","strict":true,"schema":schema}});
        }
        let response = self.client.post_json(&body).await?;
        let choices = response
            .get("choices")
            .and_then(Value::as_array)
            .filter(|choices| choices.len() == 1)
            .ok_or_else(|| {
                LlmError::UnexpectedShape("dream requires exactly one completion".into())
            })?;
        let choice = &choices[0];
        if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") {
            return Err(LlmError::UnexpectedShape(
                "dream completion was incomplete".into(),
            ));
        }
        let text = choice
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| LlmError::UnexpectedShape("dream completion was empty".into()))?;
        let usage = response.get("usage").and_then(|usage| {
            Some(Usage {
                input_tokens: u32::try_from(usage.get("prompt_tokens")?.as_u64()?).ok()?,
                output_tokens: u32::try_from(usage.get("completion_tokens")?.as_u64()?).ok()?,
            })
        });
        if usage.is_some_and(|usage| {
            usage.input_tokens > 8192 || usage.output_tokens > request.max_tokens
        }) {
            return Err(LlmError::UnexpectedShape(
                "dream endpoint reported usage over budget".into(),
            ));
        }
        Ok(ChatResponse {
            text: text.to_owned(),
            usage,
            model: self.model.clone(),
        })
    }
}

#[async_trait]
impl LlmProvider for DreamLocalProvider {
    fn name(&self) -> &'static str {
        "dream-local"
    }
    fn model(&self) -> &str {
        &self.model
    }

    fn request_count(&self) -> Option<usize> {
        Some(self.client.request_count())
    }

    async fn complete(&self, request: ChatRequest) -> LlmResult<ChatResponse> {
        self.request(request, None).await
    }

    async fn complete_structured_raw(
        &self,
        request: ChatRequest,
        schema: Value,
    ) -> LlmResult<Value> {
        let response = self.request(request, Some(schema)).await?;
        // Compatibility parsing spends the SAME admitted response, never a
        // second generation. Refuse prose around JSON rather than choosing an
        // arbitrary object a reasoning block or attacker placed first.
        let cleaned = crate::openai_compat::strip_reasoning_blocks(&response.text);
        let parsed: Value = serde_json::from_str(&cleaned).map_err(|_| {
            LlmError::UnexpectedShape("dream completion did not match JSON object shape".into())
        })?;
        if !parsed.is_object() {
            return Err(LlmError::UnexpectedShape(
                "dream completion must be a JSON object".into(),
            ));
        }
        Ok(parsed)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) fn with_timeout(
        mut client: DreamLocalClient,
        timeout: Duration,
    ) -> DreamLocalClient {
        client.timeout = timeout;
        client
    }

    #[tokio::test]
    async fn local_timeout_is_bounded_and_redacted_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok":true}))
                    .set_delay(Duration::from_millis(100)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut client = DreamLocalClient::new(&server.uri()).unwrap();
        client.timeout = Duration::from_millis(15);
        let started = std::time::Instant::now();
        assert!(
            client
                .post_json(&json!({"synthetic":"timeout"}))
                .await
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
