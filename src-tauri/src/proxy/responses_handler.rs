//! POST /v1/responses — OpenAI Responses API compatibility layer.
//!
//! Converts a subset of Responses API requests (text and function tools)
//! to Chat Completions format. Non-streaming responses are returned as
//! Responses JSON objects; streaming responses are converted from upstream
//! Chat Completions SSE to Responses-style SSE events in real time.

use super::auth;
use super::forwarder;
use super::handlers::ProxyError;
use super::rate_limiter::RateLimitConfig;
use super::protocol::responses::{
    build_responses_base_response, build_responses_sse_http_response, responses_failed_response,
    responses_sse_done, responses_sse_line, responses_to_openai_chat_request,
    transform_openai_sse_to_responses_stream, wrap_openai_response_as_responses,
};
use super::router;
use super::server::ProxyState;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use bytes::Bytes;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

// ─── Handler ─────────────────────────────────────────────────────────

/// Identify same-protocol passthrough responses returned by the forwarder.
/// When upstream channel itself is also Responses API, forwarder passes raw Responses JSON/SSE
/// directly to handler; handler should not perform secondary wrapping/conversion between Responses and Chat.
fn is_responses_passthrough_response(resp: &axum::response::Response) -> bool {
    resp.headers()
        .get("x-smart-api-gateway-responses-passthrough")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// POST /v1/responses — Responses API compatibility endpoint.
///
/// Flow:
/// 1. Authenticate (reuse existing access key logic)
/// 2. Parse Responses API request
/// 3. Convert `input` → Chat messages, `tools` → Chat tools
/// 4. Forward non-streaming to upstream via existing forwarder
/// 5. Wrap result as SSE events in Responses API format
pub async fn handle_responses(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let client_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;

    // 1. Auth
    let access_key = auth::extract_access_key(headers, &state)
        .await
        .map_err(|err| match err {
            crate::error::AppError::Validation(_) => ProxyError::Unauthorized,
            other => ProxyError::from(other),
        })?;

    // 2. Parse request body
    let body_bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read body: {e}")))?;

    let req_body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": {
                        "message": format!("Invalid JSON: {e}"),
                        "type": "invalid_request_error",
                        "code": "invalid_json"
                    }
                })),
            )
                .into_response());
        }
    };

    // ── Hosted tool types: passed through as-is ──
    //
    // As a pure relay, we pass all tool types through to the upstream unchanged.
    // Function tools are converted to Chat Completions format; all other tool
    // types (web_search, image_generation, local_shell, etc.) are forwarded as-is.
    // The upstream decides how to handle them.
    //
    // Affected tool types: web_search, image_generation, tool_search,
    //                      local_shell, custom (and any future hosted tools)

    let (mut chat_body, is_stream, model) = responses_to_openai_chat_request(&req_body);

    // Attach raw Responses request for forwarder to select same-protocol passthrough when upstream is Responses.
    // This avoids loss of reasoning.summary, encrypted_content, etc. through Responses->Chat->Responses conversion.
    // native fields.
    if let Some(obj) = chat_body.as_object_mut() {
        obj.insert("__as_raw_responses_req".to_string(), req_body.clone());
    }

    // Strip Responses-specific fields that shouldn't leak to upstream Chat API
    let responses_fields = [
        "include",
        "store",
        "client_metadata",
        "metadata",
        "prompt_cache_key",
        "prompt_cache_retention",
        "safety_identifier",
        "user",
    ];
    if let Some(obj) = chat_body.as_object_mut() {
        for field in &responses_fields {
            obj.remove(*field);
        }
    }

    let response_id = format!("resp_{}", Uuid::new_v4().to_string().replace('-', ""));
    let item_id = format!(
        "msg_{}",
        Uuid::new_v4().to_string().replace('-', "")[..16].to_string()
    );
    let created_at = chrono::Utc::now().timestamp();

    // 4. Route and forward (non-streaming)
    let requested_model = if model == "auto" || model.is_empty() {
        "auto".to_string()
    } else {
        super::handlers::validate_model_name(&model)?;
        model.to_string()
    };

    // Rate limiting: IP + access key + model dimensions.
    let cfg = RateLimitConfig::from_settings(&*state.settings.read().await);
    if let Some(exceeded) = state.rate_limiter.check(
        &cfg,
        client_ip.as_deref(),
        access_key.as_ref().map(|ak| ak.key.as_str()),
        Some(&requested_model),
    ) {
        return Err(ProxyError::RateLimited {
            retry_after_secs: exceeded.retry_after_secs,
        });
    }

    let all_entries = state.db.get_entries_for_routing()?;
    let auto_entries = state.db.get_enabled_entries_for_auto()?;
    let resolved = router::resolve(
        &requested_model,
        &all_entries,
        &auto_entries,
        &state.circuit_breakers,
    )
    .await;

    if resolved.is_empty() {
        return Err(ProxyError::NoAvailableProvider(requested_model));
    }

    // Forward with retry - handle_responses (Responses API)
    // Note: No ModelAnnotationMiddleware for Responses handler per requirements
    let middleware: Vec<Arc<dyn super::middleware::ForwarderMiddleware>> =
        vec![Arc::new(super::middleware::StreamOptionsMiddleware)];
    let caller_kind = super::middleware::CallerKind::Responses;

    let upstream_response = forwarder::forward_with_retry(
        &state,
        &resolved,
        &chat_body,
        headers,
        &requested_model,
        access_key.as_ref(),
        is_stream,
        &middleware,
        caller_kind,
    )
    .await;

    // 6. Build response based on stream mode
    let mut frames: Vec<Bytes> = Vec::new();
    let base_response = build_responses_base_response(&req_body, &response_id, created_at, &model);

    if is_stream {
        frames.push(responses_sse_line(&json!({
            "type": "response.created",
            "response": &base_response
        })));
    }

    match upstream_response {
        Ok(resp) => {
            if is_responses_passthrough_response(&resp) {
                return Ok(resp);
            }

            let status = resp.status().as_u16();

            if status != 200 {
                let body_bytes = axum::body::to_bytes(resp.into_body(), 32 * 1024 * 1024)
                    .await
                    .unwrap_or_default();
                let err_text = String::from_utf8_lossy(&body_bytes)
                    .chars()
                    .take(2000)
                    .collect::<String>();
                let error_event = json!({
                    "type": "response.failed",
                    "response": {
                        "id": &response_id,
                        "object": "response",
                        "created_at": created_at,
                        "status": "failed",
                        "error": { "message": err_text, "type": "upstream_error" }
                    }
                });
                if is_stream {
                    frames.push(responses_sse_line(&error_event));
                    frames.push(responses_sse_done());
                    return build_sse_response(frames);
                } else {
                    let non_stream_error = json!({
                        "id": &response_id,
                        "object": "response",
                        "created_at": created_at,
                        "status": "failed",
                        "error": { "message": err_text, "type": "upstream_error" }
                    });
                    return Ok(
                        (StatusCode::BAD_GATEWAY, axum::Json(non_stream_error)).into_response()
                    );
                }
            }

            if is_stream {
                return transform_openai_sse_to_responses_stream(
                    resp,
                    frames.drain(..).collect(),
                    response_id.clone(),
                    item_id.clone(),
                    model.to_string(),
                    req_body.clone(),
                    created_at,
                )
                .map_err(ProxyError::from);
            }

            // ── NON-STREAMING: buffer entire response, parse JSON ──
            let body_bytes = match axum::body::to_bytes(resp.into_body(), 32 * 1024 * 1024).await {
                Ok(b) => b,
                Err(_) => {
                    frames.push(responses_sse_line(&json!({
                        "type": "response.failed",
                        "response": {
                            "id": &response_id,
                            "object": "response",
                            "created_at": created_at,
                            "status": "failed",
                            "error": { "message": "Failed to read upstream body", "type": "upstream_error" }
                        }
                    })));
                    frames.push(responses_sse_done());
                    return build_sse_response(frames);
                }
            };

            // Parse upstream Chat Completions response
            let obj: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| {
                json!({ "choices": [{ "message": { "content": String::from_utf8_lossy(&body_bytes) } }] })
            });

            let (mut response_frames, completed_response) = wrap_openai_response_as_responses(
                &req_body,
                &response_id,
                &item_id,
                created_at,
                &model,
                &obj,
            );
            frames.append(&mut response_frames);

            if is_stream {
                frames.push(responses_sse_line(&json!({
                    "type": "response.completed",
                    "response": &completed_response
                })));
            } else {
                // Store response for later retrieval via GET
                let mut store = state.response_store.write().await;
                store.insert(response_id.clone(), completed_response.clone());
                // Evict oldest if store exceeds 100 entries
                if store.len() > 100 {
                    if let Some(oldest_key) = store.keys().next().cloned() {
                        store.remove(&oldest_key);
                    }
                }
                return Ok(axum::Json(completed_response).into_response());
            }
        }
        Err(e) => {
            let error_response =
                responses_failed_response(&response_id, created_at, &format!("{e}"), "proxy_error");

            if is_stream {
                frames.push(responses_sse_line(&json!({
                    "type": "response.failed",
                    "response": &error_response
                })));
            } else {
                return Ok(axum::Json(error_response).into_response());
            }
        }
    }

    frames.push(responses_sse_done());
    build_sse_response(frames)
}

/// Build an SSE response from pre-collected frames using a streaming channel.
fn build_sse_response(frames: Vec<Bytes>) -> Result<axum::response::Response, ProxyError> {
    build_responses_sse_http_response(frames).map_err(ProxyError::from)
}

// ─── Response Store Helpers ───────────────────────────────────────────

/// GET /v1/responses/:id — Retrieve a stored response.
pub async fn get_response(
    State(state): State<ProxyState>,
    axum::extract::Path(response_id): axum::extract::Path<String>,
) -> axum::response::Response {
    let store = state.response_store.read().await;
    if let Some(resp) = store.get(&response_id).cloned() {
        return axum::Json(resp).into_response();
    }

    (StatusCode::NOT_FOUND, axum::Json(json!({
        "error": {
            "message": format!("Response '{}' not found. This proxy does not persist responses.", response_id),
            "type": "not_found_error",
            "code": "response_not_found"
        }
    })))
    .into_response()
}

/// DELETE /v1/responses/:id — Delete a stored response.
pub async fn delete_response(
    State(state): State<ProxyState>,
    axum::extract::Path(response_id): axum::extract::Path<String>,
) -> axum::response::Response {
    let mut store = state.response_store.write().await;
    store.remove(&response_id);

    axum::Json(json!({
        "id": response_id,
        "object": "response",
        "deleted": true
    }))
    .into_response()
}

/// POST /v1/responses/:id/cancel — Cancel a response.
///
/// If the response exists in the store (future: background responses),
/// marks it as cancelled and returns the updated response.
/// Otherwise returns 404 since the proxy doesn't persist responses.
pub async fn cancel_response(
    State(state): State<ProxyState>,
    axum::extract::Path(response_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let store = state.response_store.read().await;
    if let Some(mut resp) = store.get(&response_id).cloned() {
        resp["status"] = json!("cancelled");
        return axum::Json(resp).into_response();
    }

    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({
            "error": {
                "message": format!("Response '{}' not found. This proxy does not persist responses.", response_id),
                "type": "not_found_error",
                "code": "response_not_found"
            }
        })),
    )
        .into_response()
}

// ─── Unit Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::protocol::responses::{
        convert_tools, input_to_messages, passthrough_output_item,
        responses_hosted_tool_types_for_chat_fallback, responses_hosted_tools_degradation_prompt,
        responses_to_openai_chat_request,
    };

    // ── Tool Type Tests ──

    #[test]
    fn test_tool_type_function_allowed() {
        let tools = vec![json!({ "type": "function", "name": "get_weather" })];
        // function should not trigger any rejection
        for tool in &tools {
            assert_eq!(tool.get("type").and_then(|v| v.as_str()), Some("function"));
        }
    }

    #[test]
    fn test_tool_type_host_tool_passthrough() {
        // host_tool is passed through as-is (pure relay)
        let tool = json!({ "type": "host_tool" });
        assert_eq!(tool.get("type").and_then(|v| v.as_str()), Some("host_tool"));
    }

    // ── Input Message Conversion Tests ──

    #[test]
    fn test_input_to_messages_string() {
        let input = json!("Hello");
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Hello");
    }

    #[test]
    fn test_input_to_messages_with_instructions() {
        let input = json!("Hello");
        let msgs = input_to_messages(&input, Some("Be helpful"));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be helpful");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn test_input_to_messages_null_input() {
        let input = Value::Null;
        let msgs = input_to_messages(&input, Some("Instructions"));
        // Null input + instructions → only system message, no default user "Hello"
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Instructions");
    }

    #[test]
    fn test_input_to_messages_function_call() {
        let input = json!([
            { "type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"NYC\"}" },
            { "type": "function_call_output", "call_id": "call_1", "output": "Sunny" }
        ]);
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[1]["role"], "tool");
        assert_eq!(msgs[1]["tool_call_id"], "call_1");
    }

    // ── Tool Conversion Tests ──

    #[test]
    fn test_convert_tools_function() {
        let tools = vec![
            json!({ "type": "function", "name": "my_fn", "parameters": { "type": "object" } }),
        ];
        let result = convert_tools(&tools);
        assert!(result.is_some());
        let arr = result.unwrap();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["function"]["name"], "my_fn");
    }

    #[test]
    fn test_convert_tools_function_preserves_extra_fields() {
        let tools = vec![json!({
            "type": "function",
            "name": "my_fn",
            "parameters": { "type": "object" },
            "x-provider": { "mode": "raw" }
        })];
        let result = convert_tools(&tools).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr[0]["type"], "function");
        assert_eq!(arr[0]["function"]["name"], "my_fn");
        assert_eq!(arr[0]["x-provider"]["mode"], "raw");
        assert!(arr[0].get("name").is_none());
    }

    #[test]
    fn test_passthrough_output_item_removes_index_and_adds_type() {
        let item = passthrough_output_item(
            &json!({ "id": "call_1", "index": 2, "custom": true }),
            Some("completed"),
        );
        assert_eq!(item["id"], "call_1");
        assert_eq!(item["type"], "tool_call");
        assert_eq!(item["custom"], true);
        assert_eq!(item["status"], "completed");
        assert!(item.get("index").is_none());
    }

    #[test]
    fn test_convert_tools_empty() {
        let tools: Vec<Value> = vec![];
        assert!(convert_tools(&tools).is_none());
    }

    #[test]
    fn test_convert_tools_non_function_skipped_for_chat_fallback() {
        // Temporary policy: Responses -> Chat fallback path no longer blindly passes non-function tools.
        let tools = vec![json!({ "type": "web_search", "search_context_size": "medium" })];
        let result = convert_tools(&tools);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_tools_mixed_skips_non_function_for_chat_fallback() {
        // function tools convert normally, non-function tools are skipped temporarily to avoid Chat upstream schema errors.
        let tools = vec![
            json!({ "type": "function", "name": "my_fn", "parameters": { "type": "object" } }),
            json!({ "type": "web_search" }),
            json!({ "type": "local_shell" }),
        ];
        let result = convert_tools(&tools);
        let arr = result.unwrap().as_array().unwrap().clone();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["function"]["name"], "my_fn");
        assert!(arr[0].get("name").is_none());
    }

    #[test]
    fn test_responses_hosted_tool_types_for_chat_fallback_dedupes_non_function_tools() {
        let tools = vec![
            json!({ "type": "function", "name": "my_fn" }),
            json!({ "type": "web_search" }),
            json!({ "type": "file_search" }),
            json!({ "type": "web_search" }),
        ];

        let types = responses_hosted_tool_types_for_chat_fallback(&tools);
        assert_eq!(
            types,
            vec!["web_search".to_string(), "file_search".to_string()]
        );
    }

    #[test]
    fn test_responses_hosted_tools_degradation_prompt_declares_safe_capability_boundary() {
        let prompt = responses_hosted_tools_degradation_prompt(&[
            "web_search".to_string(),
            "file_search".to_string(),
        ])
        .unwrap();

        assert!(prompt.contains("web_search"));
        assert!(prompt.contains("file_search"));
        assert!(prompt.contains("server-side tool"));
        assert!(prompt.contains("Do not claim"));
        assert!(prompt.contains("Do not invent"));
        assert!(prompt.contains("function tools"));
        assert!(!prompt.contains("PowerShell"));
        assert!(!prompt.contains("curl"));
        assert!(!prompt.contains("browser"));
        assert!(!prompt.contains("File system"));
        assert!(!prompt.contains("Database"));
    }

    #[test]
    fn test_responses_to_openai_chat_request_injects_hosted_tool_prompt() {
        let req = json!({
            "model": "auto",
            "instructions": "Keep it concise.",
            "input": "Search for latest info",
            "tools": [
                { "type": "web_search" },
                { "type": "function", "name": "get_weather", "parameters": { "type": "object" } }
            ]
        });

        let (chat_body, _, _) = responses_to_openai_chat_request(&req);
        let messages = chat_body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        let content = messages[0]["content"].as_str().unwrap();
        assert!(content.contains("Keep it concise."));
        assert!(content.contains("web_search"));
        assert!(content.contains("server-side tool"));
        assert!(content.contains("Do not claim"));

        let tools = chat_body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    // ── Image URL Content Tests ──

    #[test]
    fn test_input_to_messages_mixed_text_and_image() {
        let input = json!([{
            "type": "message", "role": "user",
            "content": [
                { "type": "text", "text": "describe" },
                { "type": "input_image", "image_url": "https://example.com/img.jpg" }
            ]
        }]);
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");

        // Content should be an array with text + image_url
        let content = msgs[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "describe");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "https://example.com/img.jpg"
        );
        assert_eq!(content[1]["image_url"]["detail"], "auto");
    }

    #[test]
    fn test_input_to_messages_pure_image() {
        let input = json!([{
            "type": "message", "role": "user",
            "content": [
                { "type": "input_image", "image_url": "https://example.com/photo.png" }
            ]
        }]);
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");

        // Content should be an array with only image_url
        let content = msgs[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "image_url");
        assert_eq!(
            content[0]["image_url"]["url"],
            "https://example.com/photo.png"
        );
        assert_eq!(content[0]["image_url"]["detail"], "auto");
    }

    #[test]
    fn test_input_to_messages_image_with_custom_detail() {
        let input = json!([{
            "type": "message", "role": "user",
            "content": [
                { "type": "input_image", "image_url": "https://example.com/img.jpg", "detail": "high" }
            ]
        }]);
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 1);

        let content = msgs[0]["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content[0]["image_url"]["detail"], "high");
    }

    #[test]
    fn test_input_to_messages_image_no_regression_text_only() {
        // Text-only content array should still produce a plain string
        let input = json!([{
            "type": "message", "role": "user",
            "content": [
                { "type": "text", "text": "hello" }
            ]
        }]);
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        // Without images or unknown parts, content remains a plain string
        assert_eq!(msgs[0]["content"], "hello");
    }

    #[test]
    fn test_input_to_messages_preserves_unknown_structured_item() {
        let input =
            json!([{ "type": "custom_tool_result", "id": "item_1", "payload": { "ok": true } }]);
        let msgs = input_to_messages(&input, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"]["type"], "custom_tool_result");
        assert_eq!(msgs[0]["content"]["id"], "item_1");
        assert_eq!(msgs[0]["content"]["payload"]["ok"], true);
    }

    #[test]
    fn test_append_utf8_safe_preserves_split_multibyte_content() {
        use super::super::sse::append_utf8_safe;
        let mut buffer = String::new();
        let mut remainder = Vec::new();
        let text = "data: {\"delta\":\"Hello € world\"}\n\n";
        let bytes = text.as_bytes();
        let split_at = bytes.iter().position(|byte| *byte >= 0x80).unwrap() + 1;

        append_utf8_safe(&mut buffer, &mut remainder, &bytes[..split_at]);
        assert!(!buffer.contains('�'));
        assert!(!remainder.is_empty());

        append_utf8_safe(&mut buffer, &mut remainder, &bytes[split_at..]);
        assert_eq!(buffer, text);
        assert!(remainder.is_empty());
        assert!(!buffer.contains('�'));
    }

    #[test]
    fn test_sse_data_payload_accepts_optional_space() {
        use super::super::sse::sse_data_payload;
        assert_eq!(
            sse_data_payload("data: {\"ok\":true}"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            sse_data_payload("data:{\"ok\":true}"),
            Some("{\"ok\":true}")
        );
        assert_eq!(sse_data_payload("event: message"), None);
    }
}
