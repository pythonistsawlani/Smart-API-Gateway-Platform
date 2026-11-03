//! Forwarding layer middleware
//!
//! Originally designed to abstract cross-cutting concerns in forwarder (stream_options injection,
//! model:xxx injection, usage stats, circuit breaking, idle timeout) into modular middleware.
//!
//! Retained only middleware with genuine standalone value; other cross-cutting logic
//! is implemented natively in forwarder.rs.
//!
//! - `StreamOptionsMiddleware`: Retained. Independent value is clear
//!   (centralizes merge logic using entry().or_insert()).
//!
//! - `ModelAnnotationMiddleware` (removed): Redundant with model_info_delta
//!   in forwarder.rs which caused duplicate injection per chunk. Now handled
//!   by checking caller_kind == Responses in should_append_model_info,
//!   without needing middleware.
//!
//! - `IdleTimeoutMiddleware` (removed): idle timeout relies on async state machine
//!   (tokio::time::sleep + poll reset), not expressible in synchronous trait methods.
//!   Actually implemented in forwarder.rs STREAMING_IDLE_TIMEOUT poll loop and
//!   claude.rs tokio::select! in transform_openai_sse_to_claude_stream.
//!
//! - `UsageLoggingMiddleware` (removed): usage tracking needs cross-chunk state accumulation
//!   （`prompt_tokens` / `completion_tokens` / `first_token_ms` / `StreamLogGuard`），
//!   unsuitable for stateless middleware. Implemented natively in forwarder.rs.
//!
//! - `CircuitBreakerMiddleware` (removed): circuit breaking needs shared circuit_breakers
//!   RwLock and async writes based on status codes. Implemented natively in
//!   forwarder.rs cool_down_entry / disable_entry.
//!
//! Retained RequestContext and CallerKind allow forwarder to differentiate callers
//! and branch at decision points such as should_append_model_info.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::header::HeaderValue;
use serde_json::Value;
use std::sync::Arc;

/// Header carrying the gateway-wide correlation ID.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Generates (or preserves) an `X-Request-ID` for every inbound request and
/// echoes it back on the response. Handlers and the forwarder read the same
/// header from the request (the middleware mutates it in place), so usage
/// logs and upstream calls share one correlation ID per request.
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        request.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// Caller type: identifies which entry handler invoked the proxy
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallerKind {
    OpenAiChat,
    ClaudeMessages,
    GeminiNative,
    AzureChat,
    Responses,
}

/// Request context passed to middleware
#[derive(Clone)]
pub struct RequestContext {
    pub caller_kind: CallerKind,
    #[allow(dead_code)]
    pub requested_model: Arc<str>,
}

/// Forwarder middleware trait
///
/// Currently only StreamOptionsMiddleware is implemented. Other logic remains native.
pub trait ForwarderMiddleware: Send + Sync {
    /// Before request body is sent
    fn on_request(&self, _body: &mut Value, _ctx: &RequestContext) {}
    /// After response body is received (non-streaming)
    #[allow(dead_code)]
    fn on_response_complete(&self, _body: &mut Value, _ctx: &RequestContext) {}
    /// SSE data chunk (streaming)
    #[allow(dead_code)]
    fn on_sse_chunk(&self, _chunk: &mut String, _ctx: &RequestContext) {}
}

/// StreamOptions: merge using `entry().or_insert()` instead of unconditional overwrite.
///
/// Preserve explicit client stream_options fields (such as `include_usage: false`,
/// `continuous_usage_stats`, etc.), only defaulting to `include_usage: true` when omitted.
///
/// This is the fix path for P2: the forwarder.rs original `body_obj.insert("stream_options", ...)`
/// has been removed.
pub struct StreamOptionsMiddleware;

impl ForwarderMiddleware for StreamOptionsMiddleware {
    fn on_request(&self, body: &mut Value, _ctx: &RequestContext) {
        if !body
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false)
        {
            return;
        }
        if let Some(obj) = body.as_object_mut() {
            let so = obj
                .entry("stream_options".to_string())
                .or_insert(serde_json::json!({}));
            if let Some(so_obj) = so.as_object_mut() {
                so_obj
                    .entry("include_usage".to_string())
                    .or_insert(serde_json::json!(true));
            }
        }
    }
}
