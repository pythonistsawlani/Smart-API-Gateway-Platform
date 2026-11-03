// Handler for test chat via admin API
use crate::admin::error::AdminError;
use crate::admin::state::AdminState;
use crate::proxy::protocol::get_adapter;
use crate::services::api_key_utils::primary_api_key;
use crate::services::log_service::{insert_test_usage_log, TestUsageLogInput};
use axum::extract::{Json, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

#[derive(Debug, Serialize, Deserialize)]
pub struct TestChatRequest {
    pub entry_id: String,
    pub messages: Vec<TestChatMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct TestChatResponse {
    pub content: String,
    pub latency_ms: u64,
    pub usage: Option<TestChatUsage>,
}

#[derive(Debug, Serialize)]
pub struct TestChatUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

fn non_empty_message_field<'a>(message: &'a Value, field: &str) -> Option<&'a str> {
    message
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn extract_content_from_message(message: &Value) -> String {
    non_empty_message_field(message, "content")
        .or_else(|| non_empty_message_field(message, "reasoning_content"))
        .or_else(|| non_empty_message_field(message, "reasoning_text"))
        .or_else(|| non_empty_message_field(message, "reasoning_details"))
        .unwrap_or("")
        .to_string()
}

fn extract_test_chat_content(body: &Value) -> String {
    body.get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("message"))
        .find_map(|message| {
            let content = extract_content_from_message(message);
            if content.trim().is_empty() {
                None
            } else {
                Some(content)
            }
        })
        .unwrap_or_default()
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect::<String>()
}

fn apply_disable_reasoning_for_test_chat(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    obj.remove("thinking");
    obj.remove("reasoning");
    obj.remove("reasoning_content");
    obj.remove("reasoning_text");
    obj.remove("reasoning_details");
    obj.remove("reasoning_effort");
}

pub async fn test_chat(
    State(state): State<AdminState>,
    Json(payload): Json<TestChatRequest>,
) -> Result<Json<TestChatResponse>, AdminError> {
    let runtime = state
        .runtime
        .as_ref()
        .ok_or_else(|| AdminError::BadRequest("Admin runtime not initialized".to_string()))?;
    let db = runtime.db.clone();

    let entries = db.get_entries_for_routing_all()?;
    let entry = entries
        .iter()
        .find(|e| e.id == payload.entry_id)
        .ok_or_else(|| AdminError::NotFound(format!("Entry {} not found", payload.entry_id)))?
        .clone();

    let channel = db.get_channel(&entry.channel_id)?;
    let adapter = get_adapter(&channel.api_type);

    let url = adapter.build_chat_url(&channel.base_url, &entry.model);
    let mut upstream_body = json!({
        "model": entry.model,
        "messages": payload.messages,
        "stream": false,
    });
    adapter.transform_request(&mut upstream_body, &entry.model);

    if db.get_settings().unwrap_or_default().disable_reasoning {
        apply_disable_reasoning_for_test_chat(&mut upstream_body);
    }

    let start = Instant::now();

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            let message = format!("HTTP client: {e}");
            insert_test_usage_log(
                &db,
                state.app_handle.as_ref(),
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "test_chat",
                    log_group: "test_chat",
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    latency_ms,
                    status_code: 502,
                    success: false,
                    error_message: Some(&message),
                    request_payload: Some(&upstream_body),
                    response_payload: None,
                    error_kind: Some("client_build_error"),
                    response_ms: Some("X"),
                    error_preview: None,
                },
            );
            state.mark_log_dirty();
            return Err(AdminError::Internal(message));
        }
    };
    let request = adapter
        .apply_auth(client.post(&url), primary_api_key(&channel.api_key))
        .json(&upstream_body);
    let request = crate::services::upstream_headers::apply_upstream_headers(
        request,
        channel.upstream_headers.as_deref(),
    )
    .map_err(|e| {
        let message = format!("Header configuration error: {e}");
        let latency_ms = start.elapsed().as_millis() as i64;
        insert_test_usage_log(
            &db,
            state.app_handle.as_ref(),
            TestUsageLogInput {
                entry: &entry,
                channel: &channel,
                operation: "test_chat",
                log_group: "test_chat",
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms,
                status_code: 502,
                success: false,
                error_message: Some(&message),
                request_payload: Some(&upstream_body),
                response_payload: None,
                error_kind: Some("header_config_error"),
                response_ms: Some("X"),
                error_preview: None,
            },
        );
        state.mark_log_dirty();
        AdminError::Internal(message)
    })?;

    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            let message = format!("Network request failed: {e}");
            insert_test_usage_log(
                &db,
                state.app_handle.as_ref(),
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "test_chat",
                    log_group: "test_chat",
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    latency_ms,
                    status_code: 502,
                    success: false,
                    error_message: Some(&message),
                    request_payload: Some(&upstream_body),
                    response_payload: None,
                    error_kind: Some("network_error"),
                    response_ms: Some("X"),
                    error_preview: None,
                },
            );
            state.mark_log_dirty();
            return Err(AdminError::Internal(message));
        }
    };

    if !response.status().is_success() {
        let latency_ms = start.elapsed().as_millis() as i64;
        let status = response.status();
        let status_code = status.as_u16() as i32;
        let body = response.text().await.unwrap_or_default();
        let error_message = format!("Upstream error {status}: {body}");
        let log_message = format!("upstream_http_{}", status.as_u16());
        insert_test_usage_log(
            &db,
            state.app_handle.as_ref(),
            TestUsageLogInput {
                entry: &entry,
                channel: &channel,
                operation: "test_chat",
                log_group: "test_chat",
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms,
                status_code,
                success: false,
                error_message: Some(&log_message),
                request_payload: Some(&upstream_body),
                response_payload: None,
                error_kind: Some("http_error"),
                response_ms: Some("X"),
                error_preview: Some(&body),
            },
        );
        state.mark_log_dirty();
        return Err(AdminError::Internal(error_message));
    }

    let latency_ms = start.elapsed().as_millis() as u64;

    let response_body = match response.text().await {
        Ok(body) => body,
        Err(e) => {
            let message = format!("response_read_error: {e}");
            insert_test_usage_log(
                &db,
                state.app_handle.as_ref(),
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "test_chat",
                    log_group: "test_chat",
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    latency_ms: latency_ms as i64,
                    status_code: 502,
                    success: false,
                    error_message: Some(&message),
                    request_payload: Some(&upstream_body),
                    response_payload: None,
                    error_kind: Some("response_read_error"),
                    response_ms: Some("X"),
                    error_preview: None,
                },
            );
            state.mark_log_dirty();
            return Err(AdminError::Internal(message));
        }
    };

    if response_body.trim().is_empty() {
        let message = "empty_response";
        insert_test_usage_log(
            &db,
            state.app_handle.as_ref(),
            TestUsageLogInput {
                entry: &entry,
                channel: &channel,
                operation: "test_chat",
                log_group: "test_chat",
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms: latency_ms as i64,
                status_code: 200,
                success: false,
                error_message: Some(message),
                request_payload: Some(&upstream_body),
                response_payload: None,
                error_kind: Some("empty_response"),
                response_ms: Some("X"),
                error_preview: None,
            },
        );
        state.mark_log_dirty();
        return Err(AdminError::Internal(message.to_string()));
    }

    let json_body: Value = match serde_json::from_str(&response_body) {
        Ok(body) => body,
        Err(e) => {
            let message = format!("Failed to parse response: {e}");
            let error_preview = truncate_for_log(&response_body, 1000);
            insert_test_usage_log(
                &db,
                state.app_handle.as_ref(),
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "test_chat",
                    log_group: "test_chat",
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    latency_ms: latency_ms as i64,
                    status_code: 502,
                    success: false,
                    error_message: Some(&message),
                    request_payload: Some(&upstream_body),
                    response_payload: None,
                    error_kind: Some("parse_error"),
                    response_ms: Some("X"),
                    error_preview: Some(&error_preview),
                },
            );
            state.mark_log_dirty();
            return Err(AdminError::Internal(message));
        }
    };

    let mut json_body = json_body;
    adapter.transform_response(&mut json_body);

    let content = extract_test_chat_content(&json_body);

    if content.trim().is_empty() {
        let message = "empty_response_content";
        let error_preview = truncate_for_log(&response_body, 1000);
        insert_test_usage_log(
            &db,
            state.app_handle.as_ref(),
            TestUsageLogInput {
                entry: &entry,
                channel: &channel,
                operation: "test_chat",
                log_group: "test_chat",
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms: latency_ms as i64,
                status_code: 200,
                success: false,
                error_message: Some(message),
                request_payload: Some(&upstream_body),
                response_payload: None,
                error_kind: Some("empty_content"),
                response_ms: Some("X"),
                error_preview: Some(&error_preview),
            },
        );
        state.mark_log_dirty();
        return Err(AdminError::Internal(message.to_string()));
    }

    let usage = json_body.get("usage").map(|u| TestChatUsage {
        prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        completion_tokens: u
            .get("completion_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        total_tokens: u.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
    });

    insert_test_usage_log(
        &db,
        state.app_handle.as_ref(),
        TestUsageLogInput {
            entry: &entry,
            channel: &channel,
            operation: "test_chat",
            log_group: "test_chat",
            prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
            completion_tokens: usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
            latency_ms: latency_ms as i64,
            status_code: 200,
            success: true,
            error_message: None,
            request_payload: Some(&upstream_body),
            response_payload: None,
            error_kind: None,
            response_ms: Some(&latency_ms.to_string()),
            error_preview: None,
        },
    );
    state.mark_log_dirty();

    Ok(Json(TestChatResponse {
        content,
        latency_ms,
        usage,
    }))
}
