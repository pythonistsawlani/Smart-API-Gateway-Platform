use super::auth;
use super::forwarder;
use super::protocol::{
    azure_to_openai_request, claude_to_openai_request, gemini_to_openai_request,
    openai_to_claude_response, openai_to_gemini_response, transform_openai_sse_to_claude_stream,
    transform_openai_sse_to_gemini_stream,
};
use super::rate_limiter::RateLimitConfig;
use super::router;
use super::server::ProxyState;
use crate::database::AccessKey;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

/// Enforce rate limits for an inference request.
///
/// Identity is derived from the client IP (via `ConnectInfo`), the presented
/// access key (when any), and the requested model. The two-pass check in
/// `RateLimiter::check` guarantees an exhausted dimension never partially
/// consumes budget on other dimensions.
async fn enforce_rate_limit(
    state: &ProxyState,
    client_ip: Option<&str>,
    access_key: Option<&AccessKey>,
    model: &str,
) -> Result<(), ProxyError> {
    let cfg = RateLimitConfig::from_settings(&*state.settings.read().await);
    if let Some(exceeded) = state.rate_limiter.check(
        &cfg,
        client_ip,
        access_key.map(|ak| ak.key.as_str()).filter(|k| !k.is_empty()),
        Some(model),
    ) {
        return Err(ProxyError::RateLimited {
            retry_after_secs: exceeded.retry_after_secs,
        });
    }
    Ok(())
}

/// Gemini single model format (used for /v1beta/models/{model})
fn gemini_single_model_item(entry: &crate::database::ApiEntry) -> Value {
    let token_limit = parse_context_limit(entry);
    json!({
        "name": format!("models/{}", entry.model),
        "version": gemini_version(entry),
        "displayName": gemini_display_name(entry),
        "description": gemini_description(entry),
        "inputTokenLimit": token_limit,
        "outputTokenLimit": token_limit,
        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
        "group_name": entry.group_name,
    })
}

/// Max length of a model name accepted by the proxy.
pub const MODEL_NAME_MAX_LEN: usize = 128;

/// Validate a model identifier supplied by the client.
///
/// Rejects empty names, path traversal (`..`), absolute-path prefixes,
/// control/whitespace characters, and anything outside the conservative
/// safe charset (`[A-Za-z0-9]` plus `- _ . / : @ + ( )`).
pub fn validate_model_name(model: &str) -> Result<(), ProxyError> {
    if model.is_empty() {
        return Err(ProxyError::BadRequest("model must not be empty".to_string()));
    }
    if model.len() > MODEL_NAME_MAX_LEN {
        return Err(ProxyError::BadRequest(format!(
            "model exceeds maximum length of {MODEL_NAME_MAX_LEN} characters"
        )));
    }
    if model.contains("..") {
        return Err(ProxyError::BadRequest(
            "model must not contain '..'".to_string(),
        ));
    }
    if model.starts_with('/') || model.starts_with('\\') {
        return Err(ProxyError::BadRequest(
            "model must not start with '/' or '\\'".to_string(),
        ));
    }
    if model.chars().any(|c| {
        c.is_control() || c.is_whitespace() || !(c.is_alphanumeric() || "._-/:@+()".contains(c))
    }) {
        return Err(ProxyError::BadRequest(
            "model contains invalid characters".to_string(),
        ));
    }
    Ok(())
}

/// Normalize the requested model, validating it for security.
///
/// Empty/`auto` inputs resolve to the `"auto"` sentinel (unvalidated, it is
/// an internal keyword); everything else is lowercased and checked against
/// `validate_model_name`.
fn normalize_requested_model(model: Option<&str>) -> Result<String, ProxyError> {
    let trimmed = model.unwrap_or("auto").trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return Ok("auto".to_string());
    }
    let normalized = trimmed.to_ascii_lowercase();
    validate_model_name(&normalized)?;
    Ok(normalized)
}

/// Identify Gemini same-protocol passthrough responses returned by the forwarder.
/// Passthrough response is already native Gemini JSON/SSE; handler should not perform OpenAI->Gemini conversion.
fn is_gemini_passthrough_response(resp: &axum::response::Response) -> bool {
    resp.headers()
        .get("x-api-switch-gemini-passthrough")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

async fn load_sorted_entries(
    state: &ProxyState,
) -> Result<Vec<crate::database::ApiEntry>, ProxyError> {
    let mut entries = state.db.get_entries_for_routing()?;
    entries.retain(|entry| !entry.model.trim().is_empty());
    let sort_mode = state.settings.read().await.default_sort_mode.clone();
    router::apply_sort_mode(&mut entries, &sort_mode);
    Ok(entries)
}

fn dedup_models_by_name(
    mut entries: Vec<crate::database::ApiEntry>,
) -> Vec<crate::database::ApiEntry> {
    // Deduplicate by group + model name, ensuring identical models in different groups are preserved separately
    let mut seen = HashSet::new();
    entries.retain(|entry| {
        let group = entry
            .group_name
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let key = format!("{}::{}", group, entry.model.to_ascii_lowercase());
        seen.insert(key)
    });
    entries
}

fn group_created_at(entries: &[crate::database::ApiEntry], group: &str) -> i64 {
    entries
        .iter()
        .find(|entry| entry.group_name.as_deref() == Some(group))
        .map(entry_created_at)
        .unwrap_or_else(|| chrono::Utc::now().timestamp())
}

fn openai_model_item(entry: &crate::database::ApiEntry) -> Value {
    let alias = if entry.display_name.trim().is_empty() {
        entry.model.clone()
    } else {
        entry.display_name.clone()
    };
    json!({
        "id": entry.model,
        "object": "model",
        "created": entry_created_at(entry),
        "owned_by": entry_owned_by(entry, "openai"),
        "display_name": alias,
        "group_name": entry.group_name,
    })
}

fn claude_model_item(entry: &crate::database::ApiEntry) -> Value {
    json!({
        "type": "model",
        "id": entry.model,
        "display_name": claude_display_name(entry),
        "created_at": entry_created_at_rfc3339(entry),
        "group_name": entry.group_name,
        "object": "model",
    })
}

fn gemini_model_item(entry: &crate::database::ApiEntry) -> Value {
    let token_limit = parse_context_limit(entry);
    json!({
        "name": format!("models/{}", entry.model),
        "version": gemini_version(entry),
        "displayName": gemini_display_name(entry),
        "description": gemini_description(entry),
        "inputTokenLimit": token_limit,
        "outputTokenLimit": token_limit,
        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
        "group_name": entry.group_name,
    })
}

fn azure_deployment_item(entry: &crate::database::ApiEntry) -> Value {
    let alias = if entry.display_name.trim().is_empty() {
        entry.model.clone()
    } else {
        entry.display_name.clone()
    };
    json!({
        "id": entry.model,
        "object": "deployment",
        "created": entry_created_at(entry),
        "owned_by": entry_owned_by(entry, "openai"),
        "model": alias,
        "display_name": alias,
        "group_name": entry.group_name,
    })
}

fn entry_created_at(entry: &crate::database::ApiEntry) -> i64 {
    if entry.created_at > 0 {
        entry.created_at
    } else {
        chrono::Utc::now().timestamp()
    }
}

fn entry_created_at_rfc3339(entry: &crate::database::ApiEntry) -> String {
    let ts = entry_created_at(entry);
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .unwrap_or_else(|| chrono::Utc::now())
        .to_rfc3339()
}

fn entry_owned_by(entry: &crate::database::ApiEntry, default_owned_by: &str) -> String {
    entry
        .owned_by
        .clone()
        .unwrap_or_else(|| default_owned_by.to_string())
}

fn claude_display_name(entry: &crate::database::ApiEntry) -> String {
    if !entry.display_name.trim().is_empty() {
        entry.display_name.clone()
    } else {
        entry.model.clone()
    }
}

fn gemini_display_name(entry: &crate::database::ApiEntry) -> String {
    if !entry.display_name.trim().is_empty() {
        entry.display_name.clone()
    } else {
        entry.model.clone()
    }
}

fn gemini_description(entry: &crate::database::ApiEntry) -> String {
    entry
        .model_meta_en
        .clone()
        .or_else(|| entry.model_meta_zh.clone())
        .unwrap_or_default()
}

fn gemini_version(entry: &crate::database::ApiEntry) -> String {
    entry
        .release_date
        .clone()
        .unwrap_or_else(|| "stable".to_string())
}

fn parse_context_limit(entry: &crate::database::ApiEntry) -> i64 {
    let mut haystacks = Vec::new();
    if let Some(text) = &entry.model_meta_en {
        haystacks.push(text.as_str());
    }
    if let Some(text) = &entry.model_meta_zh {
        haystacks.push(text.as_str());
    }

    for text in haystacks {
        for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == 'k' || c == 'K')) {
            let lower = token.to_ascii_lowercase();
            if let Some(stripped) = lower.strip_suffix('k') {
                if let Ok(value) = stripped.parse::<i64>() {
                    return value * 1024;
                }
            }
            if let Ok(value) = lower.parse::<i64>() {
                if value >= 1024 {
                    return value;
                }
            }
        }
    }

    8192
}

/// Health check endpoint
pub async fn health_check() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "status": "healthy",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        })),
    )
}

/// Handle /v1/chat/completions
pub async fn handle_chat_completions(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let client_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;

    // Extract Access Key
    let access_key = auth::extract_access_key(headers, &state)
        .await
        .map_err(|err| match err {
            crate::error::AppError::Validation(_) => ProxyError::Unauthorized,
            other => ProxyError::from(other),
        })?;

    // Read request body
    let body_bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read body: {e}")))?;

    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse JSON: {e}")))?;

    // Save raw OpenAI request (for same-protocol passthrough)
    let mut body_with_raw = body.clone();
    if let Some(obj) = body_with_raw.as_object_mut() {
        obj.insert("__as_raw_openai_req".to_string(), body.clone());
    }

    let requested_model =
        normalize_requested_model(body_with_raw.get("model").and_then(|m| m.as_str()))?;

    let is_stream = body_with_raw
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // Rate limiting: IP + access key + model dimensions.
    if let Err(rate_error) =
        enforce_rate_limit(&state, client_ip.as_deref(), access_key.as_ref(), &requested_model)
            .await
    {
        return Err(rate_error);
    }

    // Resolve target entries
    // - AUTO: only enabled entries enter the auto pool
    // - named routes: resolution is based on group/model matching before AUTO fallback
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

    // Forward with retry
    let middleware: Vec<Arc<dyn super::middleware::ForwarderMiddleware>> =
        vec![Arc::new(super::middleware::StreamOptionsMiddleware)];
    let caller_kind = super::middleware::CallerKind::OpenAiChat;

    forwarder::forward_with_retry(
        &state,
        &resolved,
        &body_with_raw,
        headers,
        &requested_model,
        access_key.as_ref(),
        is_stream,
        &middleware,
        caller_kind,
    )
    .await
}

/// Handle /v1/messages (Claude format)
///
/// Accepts requests in Claude protocol format, converts to OpenAI format
/// for internal routing, forwards to the resolved provider, and converts
/// the response back to Claude format.
pub async fn handle_messages(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let client_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;

    // Extract Access Key (same as OpenAI)
    let access_key = auth::extract_access_key(headers, &state)
        .await
        .map_err(|err| match err {
            crate::error::AppError::Validation(_) => ProxyError::Unauthorized,
            other => ProxyError::from(other),
        })?;

    // Read request body
    let body_bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read body: {e}")))?;

    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse JSON: {e}")))?;

    // Convert Claude format to OpenAI format for internal routing
    let mut openai_body = claude_to_openai_request(&body);

    // Same-protocol passthrough: preserve raw Claude request body for forwarder when upstream is Claude/Anthropic
    // to pass directly, avoiding double translation loss (including mid-conversation system positions).
    if let Some(obj) = openai_body.as_object_mut() {
        obj.insert("__as_raw_claude_req".to_string(), body.clone());
    }

    let requested_model =
        normalize_requested_model(openai_body.get("model").and_then(|m| m.as_str()))?;

    let is_stream = openai_body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // Rate limiting: IP + access key + model dimensions.
    if let Err(rate_error) =
        enforce_rate_limit(&state, client_ip.as_deref(), access_key.as_ref(), &requested_model)
            .await
    {
        return Err(rate_error);
    }

    // Resolve target entries (same logic as chat completions)
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

    // Forward with retry - handle_messages (Claude)
    let middleware: Vec<Arc<dyn super::middleware::ForwarderMiddleware>> =
        vec![Arc::new(super::middleware::StreamOptionsMiddleware)];
    let caller_kind = super::middleware::CallerKind::ClaudeMessages;

    let response = forwarder::forward_with_retry(
        &state,
        &resolved,
        &openai_body,
        headers,
        &requested_model,
        access_key.as_ref(),
        is_stream,
        &middleware,
        caller_kind,
    )
    .await?;

    // Same-protocol passthrough: response is already native Claude format (forwarder skipped conversion), return directly,
    // avoid secondary OpenAI->Claude conversion.
    if response
        .headers()
        .contains_key("x-api-switch-claude-passthrough")
    {
        return Ok(response);
    }

    // Convert the response from OpenAI format back to Claude format
    if is_stream {
        transform_openai_sse_to_claude_stream(response, requested_model.clone())
            .map_err(ProxyError::from)
    } else {
        // Non-streaming: read the body, convert JSON from OpenAI to Claude format
        let body_bytes = axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024)
            .await
            .map_err(|e| ProxyError::Internal(format!("Failed to read response: {e}")))?;

        let body: Value = serde_json::from_slice(&body_bytes)
            .map_err(|e| ProxyError::Internal(format!("Failed to parse response: {e}")))?;

        let claude_response = openai_to_claude_response(&body);

        Ok(Json(claude_response).into_response())
    }
}

/// Handle /v1/models - list ALL models from the pool (including disabled).
/// disabled only means "skip in AUTO", the model is still usable when requested by name.
pub async fn handle_list_models(
    State(state): State<ProxyState>,
) -> Result<Json<Value>, ProxyError> {
    let entries = dedup_models_by_name(load_sorted_entries(&state).await?);

    let mut group_set: HashSet<String> = HashSet::new();
    for e in &entries {
        if let Some(name) = &e.group_name {
            if !name.is_empty() {
                group_set.insert(name.clone());
            }
        }
    }
    let mut group_names: Vec<String> = group_set.into_iter().collect();
    group_names.sort();

    let group_models: Vec<Value> = group_names
        .iter()
        .map(|g| {
            json!({
                "id": g,
                "object": "model",
                "created": group_created_at(&entries, g),
                "owned_by": "group",
            })
        })
        .collect();

    let models: Vec<Value> = entries.iter().map(openai_model_item).collect();

    let mut data = group_models;
    data.extend(models);

    Ok(Json(json!({
        "object": "list",
        "data": data,
    })))
}

/// Handle /anthropic/v1/models - Anthropic native model list format.
pub async fn handle_list_models_claude(
    State(state): State<ProxyState>,
) -> Result<Json<Value>, ProxyError> {
    let entries = dedup_models_by_name(load_sorted_entries(&state).await?);
    let data: Vec<Value> = entries.iter().map(claude_model_item).collect();

    let first_id = data
        .first()
        .and_then(|item| item.get("id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let last_id = data
        .last()
        .and_then(|item| item.get("id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    Ok(Json(json!({
        "data": data,
        "first_id": first_id,
        "last_id": last_id,
        "has_more": false,
    })))
}

/// Handle /v1beta/models - Gemini native model list format.
pub async fn handle_list_models_gemini(
    State(state): State<ProxyState>,
) -> Result<Json<Value>, ProxyError> {
    let entries = dedup_models_by_name(load_sorted_entries(&state).await?);
    let models: Vec<Value> = entries.iter().map(gemini_model_item).collect();

    Ok(Json(json!({
        "models": models,
    })))
}

/// Handle /openai/deployments - Azure deployment list format.
pub async fn handle_list_models_azure(
    State(state): State<ProxyState>,
    Query(_query): Query<super::server::AzureDeploymentsQuery>,
) -> Result<Json<Value>, ProxyError> {
    let entries = dedup_models_by_name(load_sorted_entries(&state).await?);
    let data: Vec<Value> = entries.iter().map(azure_deployment_item).collect();

    Ok(Json(json!({
        "object": "list",
        "data": data,
    })))
}

/// Handle /v1beta/models/{model}:{action} (Gemini native)
///
/// Supported actions:
/// - `generateContent`        - Non-streaming content generation
/// - `streamGenerateContent`  - Streaming content generation (OpenAI SSE -> Gemini SSE conversion)
pub async fn handle_gemini_native(
    State(state): State<ProxyState>,
    Path(rest): Path<String>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let Some((model, action)) = rest.rsplit_once(':') else {
        return Err(ProxyError::Internal("Invalid Gemini path".to_string()));
    };

    // Access key enforcement + rate limiting (single entry point).
    let client_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    let access_key = auth::extract_access_key(request.headers(), &state)
        .await
        .map_err(|err| match err {
            crate::error::AppError::Validation(_) => ProxyError::Unauthorized,
            other => ProxyError::from(other),
        })?;
    let requested_model = normalize_requested_model(Some(model))?;
    enforce_rate_limit(&state, client_ip.as_deref(), access_key.as_ref(), &requested_model).await?;

    match action {
        "generateContent" => {
            handle_gemini_generate_content(State(state), model, access_key, request).await
        }
        "streamGenerateContent" => {
            handle_gemini_stream_generate_content(State(state), model, access_key, request).await
        }
        _ => Err(ProxyError::Internal(format!(
            "Unsupported Gemini action: {action}"
        ))),
    }
}

/// Non-streaming Gemini content generation (:generateContent)
async fn handle_gemini_generate_content(
    State(state): State<ProxyState>,
    model: &str,
    access_key: Option<AccessKey>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;

    let body_bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read body: {e}")))?;

    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse JSON: {e}")))?;

    let openai_body = gemini_to_openai_request(&body);

    // Save raw Gemini request (for same-protocol passthrough)
    let mut openai_body = openai_body;
    if let Some(obj) = openai_body.as_object_mut() {
        obj.insert("__as_raw_gemini_req".to_string(), body.clone());
    }

    openai_body["model"] = json!(model);

    let requested_model = normalize_requested_model(Some(model))?;

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

    let middleware: Vec<Arc<dyn super::middleware::ForwarderMiddleware>> =
        vec![Arc::new(super::middleware::StreamOptionsMiddleware)];
    let caller_kind = super::middleware::CallerKind::GeminiNative;

    let response = forwarder::forward_with_retry(
        &state,
        &resolved,
        &openai_body,
        headers,
        &requested_model,
        access_key.as_ref(),
        false,
        &middleware,
        caller_kind,
    )
    .await?;

    if is_gemini_passthrough_response(&response) {
        return Ok(response);
    }

    let body_bytes = axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read response: {e}")))?;

    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse response: {e}")))?;

    let gemini_response = openai_to_gemini_response(&body);
    Ok(Json(gemini_response).into_response())
}

/// Streaming Gemini content generation (:streamGenerateContent)
///
/// Flow:
/// 1. Native Gemini request -> OpenAI format
/// 2. Force stream=true, forward via forwarder
/// 3. forwarder returns OpenAI SSE stream (chat.completion.chunk)
/// 4. Convert line-by-line to Gemini native SSE format (candidates)
/// 5. Return streaming response to client
async fn handle_gemini_stream_generate_content(
    State(state): State<ProxyState>,
    model: &str,
    access_key: Option<AccessKey>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;

    let body_bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read body: {e}")))?;

    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse JSON: {e}")))?;

    let openai_body = gemini_to_openai_request(&body);

    // Save raw Gemini request (for same-protocol passthrough)
    let mut openai_body = openai_body;
    if let Some(obj) = openai_body.as_object_mut() {
        obj.insert("__as_raw_gemini_req".to_string(), body.clone());
    }

    openai_body["model"] = json!(model);
    openai_body["stream"] = json!(true);

    let requested_model = normalize_requested_model(Some(model))?;

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

    let middleware: Vec<Arc<dyn super::middleware::ForwarderMiddleware>> =
        vec![Arc::new(super::middleware::StreamOptionsMiddleware)];
    let caller_kind = super::middleware::CallerKind::GeminiNative;

    let response = forwarder::forward_with_retry(
        &state,
        &resolved,
        &openai_body,
        headers,
        &requested_model,
        access_key.as_ref(),
        true, // is_stream = true
        &middleware,
        caller_kind,
    )
    .await?;

    if is_gemini_passthrough_response(&response) {
        return Ok(response);
    }

    // Convert OpenAI SSE stream returned by forwarder into native Gemini SSE stream
    transform_openai_sse_to_gemini_stream(response).map_err(ProxyError::from)
}

/// Handle GET /v1beta/models/{model} - Gemini single model details
pub async fn handle_gemini_model_detail(
    State(state): State<ProxyState>,
    Path(model): Path<String>,
) -> Result<Json<Value>, ProxyError> {
    // Find model from DB
    let entries = state.db.get_entries_for_routing()?;
    let model_lower = model.to_ascii_lowercase();

    if let Some(entry) = entries
        .iter()
        .find(|e| e.model.to_ascii_lowercase() == model_lower)
    {
        return Ok(Json(gemini_single_model_item(entry)));
    }

    // Not found -> return 404 in Gemini format
    Err(ProxyError::Internal(format!("Model '{}' not found", model)))
}

/// Handle /openai/deployments/{deployment}/chat/completions (Azure native)
pub async fn handle_azure_chat(
    State(state): State<ProxyState>,
    Path(rest): Path<String>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let deployment = rest
        .strip_suffix("/chat/completions")
        .ok_or_else(|| ProxyError::Internal("Invalid Azure path".to_string()))?
        .to_string();
    validate_model_name(&deployment)?;

    let client_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string());
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;

    let access_key = auth::extract_access_key(headers, &state)
        .await
        .map_err(|err| match err {
            crate::error::AppError::Validation(_) => ProxyError::Unauthorized,
            other => ProxyError::from(other),
        })?;

    let body_bytes = axum::body::to_bytes(body, 32 * 1024 * 1024)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read body: {e}")))?;

    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse JSON: {e}")))?;

    let openai_body = azure_to_openai_request(&body, &deployment);

    // Save raw Azure request (for same-protocol passthrough)
    let mut openai_body = openai_body;
    if let Some(obj) = openai_body.as_object_mut() {
        obj.insert("__as_raw_azure_req".to_string(), body.clone());
    }

    let requested_model =
        normalize_requested_model(openai_body.get("model").and_then(|m| m.as_str()))?;

    let is_stream = openai_body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // Rate limiting: IP + access key + model dimensions.
    if let Err(rate_error) =
        enforce_rate_limit(&state, client_ip.as_deref(), access_key.as_ref(), &requested_model)
            .await
    {
        return Err(rate_error);
    }

    let all_entries = state.db.get_entries_for_routing()?;
    let auto_entries = state.db.get_enabled_entries_for_auto()?;

    let mut resolved: Vec<crate::database::ApiEntry> = {
        let deployment_lower = deployment.to_ascii_lowercase();
        all_entries
            .iter()
            .filter(|e| e.enabled)
            .filter(|e| e.model.to_ascii_lowercase() == deployment_lower)
            .cloned()
            .collect()
    };

    if resolved.is_empty() {
        resolved = router::resolve(
            &requested_model,
            &all_entries,
            &auto_entries,
            &state.circuit_breakers,
        )
        .await;
    }

    if resolved.is_empty() {
        return Err(ProxyError::NoAvailableProvider(requested_model));
    }

    let middleware: Vec<Arc<dyn super::middleware::ForwarderMiddleware>> =
        vec![Arc::new(super::middleware::StreamOptionsMiddleware)];
    let caller_kind = super::middleware::CallerKind::AzureChat;

    let response = forwarder::forward_with_retry(
        &state,
        &resolved,
        &openai_body,
        headers,
        &requested_model,
        access_key.as_ref(),
        is_stream,
        &middleware,
        caller_kind,
    )
    .await?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::ApiEntry;

    fn sample_entry(
        id: &str,
        model: &str,
        display_name: &str,
        group_name: Option<&str>,
    ) -> ApiEntry {
        ApiEntry {
            id: id.to_string(),
            channel_id: format!("channel-{id}"),
            model: model.to_string(),
            display_name: display_name.to_string(),
            sort_index: 0,
            enabled: true,
            cooldown_until: None,
            circuit_state: "closed".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            channel_name: Some("channel-a".to_string()),
            channel_api_type: Some("openai".to_string()),
            owned_by: Some("openai".to_string()),
            response_ms: None,
            provider_logo: None,
            release_date: Some("2024-01-02".to_string()),
            model_meta_zh: Some("Context 32k".to_string()),
            model_meta_en: Some("Context 32k".to_string()),
            group_name: group_name.map(str::to_string),
            score: 0.0,
        }
    }

    #[test]
    fn openai_model_item_includes_created_and_owned_by() {
        let entry = sample_entry("1", "gpt-4o", "GPT-4o", None);
        let value = openai_model_item(&entry);
        assert_eq!(value["id"], "gpt-4o");
        assert_eq!(value["object"], "model");
        assert_eq!(value["created"], 1_700_000_000);
        assert_eq!(value["owned_by"], "openai");
    }

    #[test]
    fn claude_model_item_uses_rfc3339_created_at() {
        let entry = sample_entry("1", "claude-3-5-sonnet", "Claude Sonnet", None);
        let value = claude_model_item(&entry);
        assert_eq!(value["type"], "model");
        assert_eq!(value["id"], "claude-3-5-sonnet");
        assert_eq!(value["display_name"], "Claude Sonnet");
        assert_eq!(value["created_at"], "2023-11-14T22:13:20+00:00");
    }

    #[test]
    fn gemini_passthrough_response_header_requires_true_value() {
        let response = axum::response::Response::builder()
            .header("x-api-switch-gemini-passthrough", "true")
            .body(axum::body::Body::empty())
            .expect("response");
        assert!(is_gemini_passthrough_response(&response));

        let response = axum::response::Response::builder()
            .header("x-api-switch-gemini-passthrough", "false")
            .body(axum::body::Body::empty())
            .expect("response");
        assert!(!is_gemini_passthrough_response(&response));
    }

    #[test]
    fn gemini_model_item_uses_native_shape() {
        let entry = sample_entry("1", "gemini-2.0-flash", "Gemini Flash", None);
        let value = gemini_model_item(&entry);
        assert_eq!(value["name"], "models/gemini-2.0-flash");
        assert_eq!(value["displayName"], "Gemini Flash");
        assert_eq!(
            value["supportedGenerationMethods"],
            json!(["generateContent", "streamGenerateContent"])
        );
    }

    #[test]
    fn azure_deployment_item_uses_deployment_object_and_model_fallback() {
        let entry = sample_entry("1", "deployment-1", "", None);
        let value = azure_deployment_item(&entry);
        assert_eq!(value["object"], "deployment");
        assert_eq!(value["model"], "deployment-1");
    }

    #[test]
    fn dedup_models_by_name_keeps_first_case_insensitive_match() {
        let first = sample_entry("1", "gpt-4o", "First", None);
        let second = sample_entry("2", "GPT-4O", "Second", None);
        let items = dedup_models_by_name(vec![first.clone(), second]);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn validate_model_name_accepts_common_names() {
        for name in [
            "gpt-4o",
            "claude-3.5-sonnet",
            "gemini-2.5-flash",
            "models/gemini-2.0-flash-001",
            "azure/gpt-4o-mini",
            "deepseek-r1",
            "o1-pro",
        ] {
            assert!(validate_model_name(name).is_ok(), "should accept {name}");
        }
    }

    #[test]
    fn validate_model_name_rejects_empty_and_whitespace() {
        assert!(matches!(validate_model_name(""), Err(ProxyError::BadRequest(_))));
        assert!(matches!(
            validate_model_name("  "),
            Err(ProxyError::BadRequest(_))
        ));
        assert!(matches!(
            validate_model_name("gpt 4o"),
            Err(ProxyError::BadRequest(_))
        ));
        assert!(matches!(
            validate_model_name("gpt\u{0000}"),
            Err(ProxyError::BadRequest(_))
        ));
    }

    #[test]
    fn validate_model_name_rejects_path_traversal() {
        for name in [
            "..",
            "a..b",
            "../etc/passwd",
            "..\\windows",
            "/etc/passwd",
            "\\boot",
        ] {
            assert!(
                matches!(validate_model_name(name), Err(ProxyError::BadRequest(_))),
                "should reject {name}"
            );
        }
    }

    #[test]
    fn validate_model_name_rejects_invalid_characters() {
        for name in [
            "gpt$4o",
            "model;drop",
            "gpt'4o",
            "gpt\"4o",
            "gpt*4o",
            "gpt?4o",
        ] {
            assert!(
                matches!(validate_model_name(name), Err(ProxyError::BadRequest(_))),
                "should reject {name}"
            );
        }
    }

    #[test]
    fn validate_model_name_rejects_oversized_names() {
        let long = "m".repeat(MODEL_NAME_MAX_LEN + 1);
        assert!(matches!(
            validate_model_name(&long),
            Err(ProxyError::BadRequest(_))
        ));
        let boundary = "m".repeat(MODEL_NAME_MAX_LEN);
        assert!(validate_model_name(&boundary).is_ok());
    }

    #[test]
    fn normalize_requested_model_maps_empty_and_auto() {
        assert_eq!(normalize_requested_model(None).unwrap(), "auto");
        assert_eq!(normalize_requested_model(Some("")).unwrap(), "auto");
        assert_eq!(normalize_requested_model(Some("  ")).unwrap(), "auto");
        assert_eq!(normalize_requested_model(Some("AUTO")).unwrap(), "auto");
    }

    #[test]
    fn normalize_requested_model_lowercases_valid_names() {
        assert_eq!(normalize_requested_model(Some("GPT-4O")).unwrap(), "gpt-4o");
        assert!(normalize_requested_model(Some("..")).is_err());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("No available provider for model: {0}")]
    NoAvailableProvider(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("All providers failed")]
    AllProvidersFailed,

    #[error("Upstream error {status}: {message}")]
    Upstream { status: u16, message: String },

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            ProxyError::NoAvailableProvider(model) => (
                StatusCode::NOT_FOUND,
                format!("No available provider for model: {model}"),
            ),
            ProxyError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()),
            ProxyError::AllProvidersFailed => {
                (StatusCode::BAD_GATEWAY, "All providers failed".to_string())
            }
            ProxyError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            ProxyError::Upstream { status, message } => {
                let code = StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY);
                (code, message.clone())
            }
            ProxyError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            ProxyError::RateLimited { retry_after_secs } => {
                let body = json!({
                    "error": {
                        "message": format!(
                            "Rate limit exceeded. Please retry in {retry_after_secs} seconds."
                        ),
                        "type": "rate_limit_exceeded",
                        "code": StatusCode::TOO_MANY_REQUESTS.as_u16(),
                    }
                });
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", retry_after_secs.to_string())],
                    Json(body),
                )
                    .into_response();
            }
        };

        let body = json!({
            "error": {
                "message": message,
                "type": "proxy_error",
                "code": status.as_u16(),
            }
        });

        (status, Json(body)).into_response()
    }
}

impl From<crate::error::AppError> for ProxyError {
    fn from(e: crate::error::AppError) -> Self {
        ProxyError::Internal(e.to_string())
    }
}
