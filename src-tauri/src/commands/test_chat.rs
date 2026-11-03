use crate::error::AppError;
use crate::proxy::protocol::get_adapter;
use crate::services::api_key_utils::primary_api_key;
use crate::services::log_service::{insert_test_usage_log, TestUsageLogInput};
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;
use tauri::State;

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

#[derive(Debug, Serialize, Deserialize)]
pub struct TestChatMessage {
    pub role: String,
    pub content: String,
}

fn non_empty_message_field<'a>(message: &'a Value, field: &str) -> Option<&'a str> {
    message
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn extract_text_from_content_block(block: &Value) -> String {
    block
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn extract_content_from_message(message: &Value) -> String {
    // First try directly taking string content
    if let Some(text) = non_empty_message_field(message, "content") {
        return text.to_string();
    }

    // content may be an array (content blocks)
    if let Some(arr) = message.get("content").and_then(Value::as_array) {
        let texts: Vec<String> = arr
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    Some(extract_text_from_content_block(block))
                } else {
                    None
                }
            })
            .filter(|t| !t.is_empty())
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }

    // fallback: try serializing content to string
    if let Some(content) = message.get("content") {
        if !content.is_null() {
            if let Some(s) = content.as_str() {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            } else {
                // Not a string (may be an array), try JSON serialization as fallback display
                if let Ok(s) = serde_json::to_string(content) {
                    if s != "null" {
                        return s;
                    }
                }
            }
        }
    }

    // fallback: reasoning field
    non_empty_message_field(message, "reasoning_content")
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

#[tauri::command]
pub async fn test_chat(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    entry_id: String,
    messages: Vec<TestChatMessage>,
) -> Result<TestChatResponse, AppError> {
    let db = state.db.clone();

    // Get the entry directly (all entries, not just enabled ones)
    let entries = db.get_entries_for_routing_all()?;
    let entry = entries
        .iter()
        .find(|e| e.id == entry_id)
        .ok_or_else(|| AppError::NotFound(format!("Entry {entry_id} not found")))?
        .clone();

    // Get channel info
    let channel = db.get_channel(&entry.channel_id)?;

    // Get protocol adapter
    let adapter = get_adapter(&channel.api_type);

    // Build URL and transform request
    let url = adapter.build_chat_url(&channel.base_url, &entry.model);
    let mut upstream_body = json!({
        "model": entry.model,
        "messages": messages,
        "stream": false,
    });
    adapter.transform_request(&mut upstream_body, &entry.model);
    if state.settings.read().await.disable_reasoning {
        apply_disable_reasoning_for_test_chat(&mut upstream_body);
    }

    let start = Instant::now();

    // Send request directly to upstream
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
                None,
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
            return Err(AppError::Network(message));
        }
    };
    let mut request = adapter
        .apply_auth(client.post(&url), primary_api_key(&channel.api_key))
        .json(&upstream_body);

    // Use channel-configured upstream_headers instead of hardcoded header injection
    request = crate::services::upstream_headers::apply_upstream_headers(
        request,
        channel.upstream_headers.as_deref(),
    )?;

    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            let message = format!("Request failed: {e}");
            insert_test_usage_log(
                &db,
                None,
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

            return Err(AppError::Network(message));
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
            None,
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
                response_payload: Some(body.as_str()),
                error_kind: Some("http_error"),
                response_ms: Some("X"),
                error_preview: Some(&body),
            },
        );

        return Err(AppError::Proxy(error_message));
    }

    let latency_ms = start.elapsed().as_millis() as u64;

    let response_body = match response.text().await {
        Ok(body) => body,
        Err(e) => {
            let message = format!("response_read_error: {e}");
            insert_test_usage_log(
                &db,
                None,
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

            return Err(AppError::Internal(message));
        }
    };

    if response_body.trim().is_empty() {
        let message = "empty_response";
        insert_test_usage_log(
            &db,
            None,
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
                response_payload: Some(response_body.as_str()),
                error_kind: Some("empty_response"),
                response_ms: Some("X"),
                error_preview: None,
            },
        );

        return Err(AppError::Internal(message.to_string()));
    }

    let json_body: Value = match serde_json::from_str(&response_body) {
        Ok(body) => body,
        Err(e) => {
            let message = format!("Failed to parse response: {e}");
            insert_test_usage_log(
                &db,
                None,
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
                    response_payload: Some(response_body.as_str()),
                    error_kind: Some("parse_error"),
                    response_ms: Some("X"),
                    error_preview: Some(&response_body),
                },
            );

            return Err(AppError::Internal(message));
        }
    };

    // Transform response if needed (e.g. Claude → OpenAI format)
    let mut json_body = json_body;
    adapter.transform_response(&mut json_body);

    // Extract content; some reasoning models may only return reasoning field with empty content.
    let content = extract_test_chat_content(&json_body);

    if content.trim().is_empty() {
        let message = "empty_response_content";
        insert_test_usage_log(
            &db,
            None,
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
                response_payload: Some(response_body.as_str()),
                error_kind: Some("empty_content"),
                response_ms: Some("X"),
                error_preview: Some(&response_body),
            },
        );

        return Err(AppError::Internal(message.to_string()));
    }

    // Extract usage
    let usage = json_body.get("usage").map(|u| TestChatUsage {
        prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        completion_tokens: u
            .get("completion_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        total_tokens: u.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
    });

    let response_ms = latency_ms.to_string();
    insert_test_usage_log(
        &db,
        None,
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
            response_payload: Some(response_body.as_str()),
            error_kind: None,
            response_ms: Some(&response_ms),
            error_preview: Some(&response_body),
        },
    );

    Ok(TestChatResponse {
        content,
        latency_ms,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_test_chat_content_prefers_content() {
        let body = json!({
            "choices": [{
                "message": {
                    "content": "Regular content",
                    "reasoning_content": "Reasoning content"
                }
            }]
        });

        assert_eq!(extract_test_chat_content(&body), "Regular content");
    }

    #[test]
    fn extract_test_chat_content_falls_back_to_reasoning_content() {
        let body = json!({
            "choices": [{
                "message": {
                    "content": "",
                    "reasoning_content": "Reasoning content"
                }
            }]
        });

        assert_eq!(extract_test_chat_content(&body), "Reasoning content");
    }

    #[test]
    fn extract_test_chat_content_falls_back_to_reasoning_text() {
        let body = json!({
            "choices": [{
                "message": {
                    "content": "   ",
                    "reasoning_text": "Compatible reasoning content"
                }
            }]
        });

        assert_eq!(extract_test_chat_content(&body), "Compatible reasoning content");
    }

    #[test]
    fn apply_disable_reasoning_for_test_chat_removes_fields() {
        let mut body = json!({
            "model": "qwen/qwen3.5-122b-a10b",
            "thinking": true,
            "reasoning": { "effort": "high" },
            "reasoning_content": "Reasoning content",
            "reasoning_text": "Compatible reasoning content",
            "reasoning_details": "Reasoning details",
            "reasoning_effort": "high"
        });

        apply_disable_reasoning_for_test_chat(&mut body);

        let obj = body.as_object().expect("Request body must be an object");
        assert!(!obj.contains_key("thinking"));
        assert!(!obj.contains_key("reasoning"));
        assert!(!obj.contains_key("reasoning_content"));
        assert!(!obj.contains_key("reasoning_text"));
        assert!(!obj.contains_key("reasoning_details"));
        assert!(!obj.contains_key("reasoning_effort"));
    }
}
