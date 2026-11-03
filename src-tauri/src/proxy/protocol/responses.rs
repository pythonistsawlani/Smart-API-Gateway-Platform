//! OpenAI Responses API upstream adapter (Beta)
//!
//! Adapter for channel.api_type = "responses", translating chat.completions
//! intermediate format into Responses API requests sent upstream, and translating Responses API responses
//! back to chat.completions format for internal consumption.
//!
//! Reference official documentation:
//! https://platform.openai.com/docs/api-reference/responses
//!
//! Axiom: input semantics match output semantics. Known fields translate per doc, unknown fields passthrough.

use super::{join_url, ProtocolAdapter};
use axum::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

pub fn responses_sse_line(obj: &Value) -> bytes::Bytes {
    let line = format!(
        "data: {}\n\n",
        serde_json::to_string(obj).unwrap_or_default()
    );
    bytes::Bytes::from(line)
}

pub fn responses_sse_done() -> bytes::Bytes {
    bytes::Bytes::from("data: [DONE]\n\n")
}

// --- Responses SSE -> Chat SSE helper functions (upstream direction) ---

/// Convert Responses `response.output_text.delta` to Chat SSE text delta.
fn chat_sse_text_delta(delta: &str) -> String {
    serde_json::to_string(&json!({
        "choices": [{"index": 0, "delta": {"content": delta}}]
    }))
    .unwrap_or_default()
}

/// Convert Responses `response.output_item.added` (function_call) to Chat SSE tool call start.
fn chat_sse_tool_call_begin(
    output_index: u64,
    call_id: &str,
    name: &str,
    arguments: &str,
) -> String {
    serde_json::to_string(&json!({
        "choices": [{"index": 0, "delta": {
            "tool_calls": [{
                "index": output_index,
                "id": call_id,
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            }]
        }}]
    }))
    .unwrap_or_default()
}

/// Convert Responses `response.function_call_arguments.delta` to Chat SSE tool argument delta.
fn chat_sse_tool_call_args(output_index: u64, delta: &str) -> String {
    serde_json::to_string(&json!({
        "choices": [{"index": 0, "delta": {
            "tool_calls": [{
                "index": output_index,
                "function": {
                    "arguments": delta
                }
            }]
        }}]
    }))
    .unwrap_or_default()
}

/// Convert Responses `response.completed` to Chat SSE completion signal (finish_reason + usage).
fn chat_sse_completed(finish_reason: &str, input_tokens: i64, output_tokens: i64) -> String {
    serde_json::to_string(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    }))
    .unwrap_or_default()
}

/// Convert Responses failure event to error envelope recognizable by Chat/OpenAI clients.
fn chat_sse_error(error: Option<&Value>) -> String {
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("Responses stream failed");
    let error_type = error
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("responses_error");
    let code = error
        .and_then(|e| e.get("code"))
        .cloned()
        .unwrap_or(Value::Null);

    serde_json::to_string(&json!({
        "error": {
            "message": message,
            "type": error_type,
            "code": code
        }
    }))
    .unwrap_or_default()
}

pub fn responses_function_call_done_event(
    response_id: &str,
    item_id: &str,
    output_index: u32,
    arguments: &str,
) -> Value {
    json!({
        "type": "response.function_call_arguments.done",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "arguments": arguments
    })
}

pub fn responses_function_call_output_item(
    id: &str,
    name: &str,
    arguments: &str,
    status: &str,
) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "call_id": id,
        "name": name,
        "arguments": arguments,
        "status": status
    })
}

pub fn responses_message_output_item(item_id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "role": "assistant",
        "id": item_id,
        "status": status,
        "content": [{ "type": "output_text", "text": text, "annotations": [] }]
    })
}

fn extract_reasoning_from_chat_value(value: &Value) -> Option<&str> {
    value
        .get("reasoning_text")
        .or_else(|| value.get("reasoning_content"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            value
                .get("provider_specific")
                .and_then(|p| p.get("thinking"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
}

fn extract_reasoning_from_responses_item(item: &Value) -> Option<&str> {
    item.get("content")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts.iter().find_map(|part| {
                let typ = part.get("type").and_then(Value::as_str).unwrap_or("");
                if typ == "reasoning_text" || typ == "text" {
                    part.get("text")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            item.get("summary")
                .and_then(Value::as_array)
                .and_then(|parts| {
                    parts.iter().find_map(|part| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                    })
                })
        })
        .or_else(|| {
            item.get("encrypted_content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
}

fn responses_reasoning_output_item(item_id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": item_id,
        "status": status,
        "summary": [{ "type": "summary_text", "text": text }],
        "content": [{ "type": "reasoning_text", "text": text }]
    })
}

fn responses_reasoning_summary_part_added_event(
    response_id: &str,
    item_id: &str,
    output_index: u32,
) -> Value {
    json!({
        "type": "response.reasoning_summary_part.added",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "summary_index": 0
    })
}

fn responses_reasoning_summary_text_delta_event(
    response_id: &str,
    item_id: &str,
    output_index: u32,
    delta: &str,
) -> Value {
    json!({
        "type": "response.reasoning_summary_text.delta",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "summary_index": 0,
        "delta": delta
    })
}

fn responses_reasoning_text_delta_event(
    response_id: &str,
    item_id: &str,
    output_index: u32,
    delta: &str,
) -> Value {
    json!({
        "type": "response.reasoning_text.delta",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "content_index": 0,
        "delta": delta
    })
}

fn chat_sse_reasoning_delta(delta: &str) -> String {
    serde_json::to_string(&json!({
        "choices": [{"index": 0, "delta": {
            "reasoning_text": delta,
            "reasoning_content": delta
        }}]
    }))
    .unwrap_or_default()
}

pub fn responses_output_item_added_event(
    response_id: &str,
    output_index: u32,
    item: Value,
) -> Value {
    json!({
        "type": "response.output_item.added",
        "response_id": response_id,
        "output_index": output_index,
        "item": item
    })
}

pub fn responses_output_item_done_event(
    response_id: &str,
    output_index: u32,
    item: Value,
) -> Value {
    json!({
        "type": "response.output_item.done",
        "response_id": response_id,
        "output_index": output_index,
        "item": item
    })
}

pub fn responses_output_text_delta_event(
    response_id: &str,
    item_id: &str,
    output_index: u32,
    content_index: u32,
    delta: &str,
) -> Value {
    json!({
        "type": "response.output_text.delta",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "content_index": content_index,
        "delta": delta
    })
}

pub fn responses_incomplete_details(finish_reason: Option<&str>) -> Value {
    match finish_reason {
        Some("length") | Some("content_filter") => json!({ "reason": finish_reason }),
        _ => json!(null),
    }
}

pub fn responses_final_status(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") | Some("content_filter") => "incomplete",
        _ => "completed",
    }
}

pub fn responses_usage_object(usage: &Value) -> Value {
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(input_tokens + output_tokens);
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": { "cached_tokens": cached_tokens },
        "output_tokens": output_tokens,
        "output_tokens_details": { "reasoning_tokens": reasoning_tokens },
        "total_tokens": total_tokens
    })
}

pub fn responses_completed_response(
    response_id: &str,
    created_at: i64,
    final_status: &str,
    incomplete_details: Value,
    req_body: &Value,
    model: &str,
    output: Vec<Value>,
    output_text: Option<&str>,
    usage: Value,
) -> Value {
    let mut response = json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": final_status,
        "error": null,
        "incomplete_details": incomplete_details,
        "instructions": req_body.get("instructions"),
        "max_output_tokens": req_body.get("max_output_tokens"),
        "model": model,
        "output": output,
        "output_text": output_text,
        "parallel_tool_calls": req_body.get("parallel_tool_calls").unwrap_or(&json!(true)),
        "reasoning": req_body.get("reasoning").cloned().unwrap_or(json!({"effort": null, "summary": null})),
        "temperature": req_body.get("temperature").unwrap_or(&json!(1.0)),
        "text": req_body.get("text").cloned().unwrap_or(json!({"format": {"type": "text"}})),
        "tool_choice": req_body.get("tool_choice").unwrap_or(&json!("auto")),
        "tools": req_body.get("tools").unwrap_or(&json!([])),
        "top_p": req_body.get("top_p").unwrap_or(&json!(1.0)),
        "truncation": req_body.get("truncation").unwrap_or(&json!("disabled")),
        "previous_response_id": null,
        "store": req_body.get("store").unwrap_or(&json!(true)),
        "usage": usage,
        "user": req_body.get("user"),
        "metadata": req_body.get("metadata").unwrap_or(&json!({}))
    });

    if let Some(obj) = response.as_object_mut() {
        filter_responses_response_fields(obj);
    }

    response
}

pub fn responses_failed_response(
    response_id: &str,
    created_at: i64,
    message: &str,
    error_type: &str,
) -> Value {
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "failed",
        "error": { "message": message, "type": error_type }
    })
}

fn filter_responses_response_fields(obj: &mut serde_json::Map<String, Value>) {
    const RESPONSES_RESPONSE_CORE_FIELDS: &[&str] = &[
        "id",
        "object",
        "created_at",
        "model",
        "output",
        "usage",
        "status",
        "error",
        "incomplete_details",
        "metadata",
    ];

    obj.retain(|key, _| {
        RESPONSES_RESPONSE_CORE_FIELDS.contains(&key.as_str())
            || RESPONSES_RESPONSE_STANDARD_FIELDS.contains(&key.as_str())
            || RESPONSES_RESPONSE_EXTENSION_FIELDS.contains(&key.as_str())
    });
}

pub fn responses_hosted_tool_types_for_chat_fallback(tools: &[Value]) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    for tool in tools {
        let typ = tool
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>");
        if typ != "function" && !types.iter().any(|existing| existing == typ) {
            types.push(typ.to_string());
        }
    }
    types
}

pub fn responses_hosted_tools_degradation_prompt(tool_types: &[String]) -> Option<String> {
    if tool_types.is_empty() {
        return None;
    }

    Some(format!(
        "The original Responses request included server-side tool(s): {}, but the current Chat upstream does not provide those hosted tools.\n\n\
Do not claim that you used these server-side tools. Do not invent tool results.\n\
If the task can be answered from the conversation and model knowledge, answer normally.\n\
If the task requires live, external, private, or tool-only data, clearly state that the required hosted tool is unavailable in this route and ask the caller to provide the needed data or retry through a Responses-capable route.\n\
You may only use function tools that are explicitly present in this Chat request.",
        tool_types.join(", ")
    ))
}

fn inject_responses_tool_degradation_prompt(messages: &mut Vec<Value>, prompt: String) {
    if let Some(first_system) = messages
        .iter_mut()
        .find(|message| message.get("role").and_then(|role| role.as_str()) == Some("system"))
    {
        if let Some(content) = first_system
            .get("content")
            .and_then(|content| content.as_str())
        {
            first_system["content"] = json!(format!("{}\n\n{}", content, prompt));
            return;
        }
    }

    messages.insert(0, json!({ "role": "system", "content": prompt }));
}

pub fn responses_to_openai_chat_request(req_body: &Value) -> (Value, bool, String) {
    let is_stream = req_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let model = req_body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("auto")
        .to_string();

    let mut messages = input_to_messages(
        req_body.get("input").unwrap_or(&Value::Null),
        req_body.get("instructions").and_then(|v| v.as_str()),
    );

    if let Some(tools) = req_body.get("tools").and_then(|v| v.as_array()) {
        let hosted_tool_types = responses_hosted_tool_types_for_chat_fallback(tools);
        if let Some(prompt) = responses_hosted_tools_degradation_prompt(&hosted_tool_types) {
            inject_responses_tool_degradation_prompt(&mut messages, prompt);
        }
    }

    let mut chat_body = json!({
        "model": model,
        "messages": messages,
        "stream": is_stream,
    });

    // Reasoning: Responses API reasoning object -> Chat API flat fields
    if let Some(reasoning) = req_body.get("reasoning").and_then(|v| v.as_object()) {
        if let Some(effort) = reasoning.get("effort").and_then(|v| v.as_str()) {
            chat_body["reasoning_effort"] = json!(effort);
        }
    }

    // text.format -> response_format (Responses -> Chat reverse mapping)
    if let Some(text_obj) = req_body.get("text").and_then(|v| v.as_object()) {
        if let Some(format) = text_obj.get("format") {
            if let Some(response_format) = chat_response_format_from_text_format(format) {
                chat_body["response_format"] = response_format;
            }
        }
    }

    for field in [
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("service_tier", "service_tier"),
        ("top_logprobs", "top_logprobs"),
        ("stream_options", "stream_options"),
        ("include", "include"),
        ("prompt", "prompt"),
        ("prompt_cache_key", "prompt_cache_key"),
        ("prompt_cache_retention", "prompt_cache_retention"),
        ("safety_identifier", "safety_identifier"),
    ] {
        if let Some(value) = req_body.get(field.0) {
            chat_body[field.1] = value.clone();
        }
    }

    if let Some(max_tokens) = req_body.get("max_output_tokens") {
        chat_body["max_tokens"] = max_tokens.clone();
    }

    if let Some(tools) = req_body.get("tools").and_then(|v| v.as_array()) {
        if let Some(converted) = convert_tools(tools) {
            chat_body["tools"] = converted;
            for field in [
                ("tool_choice", "tool_choice"),
                ("parallel_tool_calls", "parallel_tool_calls"),
                ("max_tool_calls", "max_tool_calls"),
            ] {
                if let Some(value) = req_body.get(field.0) {
                    chat_body[field.1] = value.clone();
                }
            }
        }
    }

    if let (Some(req_obj), Some(chat_obj)) = (req_body.as_object(), chat_body.as_object_mut()) {
        for (key, value) in req_obj {
            if chat_obj.contains_key(key) {
                continue;
            }
            // Skip fields already consumed by Responses→Chat conversion
            if matches!(
                key.as_str(),
                "input"
                    | "instructions"
                    | "reasoning"
                    | "text"
                    | "response_format"
                    | "tools"
                    | "tool_choice"
                    | "parallel_tool_calls"
                    | "max_tool_calls"
            ) {
                continue;
            }
            chat_obj.insert(key.clone(), value.clone());
        }
    }

    (chat_body, is_stream, model)
}

fn chat_response_format_from_text_format(format: &Value) -> Option<Value> {
    let typ = format.get("type").and_then(Value::as_str)?;
    match typ {
        "json_object" => Some(format.clone()),
        "json_schema" if format.get("json_schema").is_some() => Some(format.clone()),
        "json_schema" => Some(json!({ "type": "json_object" })),
        _ => None,
    }
}

pub fn build_responses_base_response(
    req_body: &Value,
    response_id: &str,
    created_at: i64,
    model: &str,
) -> Value {
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "in_progress",
        "error": null,
        "incomplete_details": null,
        "instructions": req_body.get("instructions"),
        "max_output_tokens": req_body.get("max_output_tokens"),
        "model": model,
        "output": [],
        "parallel_tool_calls": req_body.get("parallel_tool_calls").unwrap_or(&json!(true)),
        "reasoning": req_body.get("reasoning").cloned().unwrap_or(json!({"effort": null, "summary": null})),
        "temperature": req_body.get("temperature").unwrap_or(&json!(1.0)),
        "text": req_body.get("text").cloned().unwrap_or(json!({"format": {"type": "text"}})),
        "tool_choice": req_body.get("tool_choice").unwrap_or(&json!("auto")),
        "tools": req_body.get("tools").unwrap_or(&json!([])),
        "top_p": req_body.get("top_p").unwrap_or(&json!(1.0)),
        "truncation": req_body.get("truncation").unwrap_or(&json!("disabled")),
        "previous_response_id": null,
        "store": req_body.get("store").unwrap_or(&json!(true)),
        "usage": null,
        "user": req_body.get("user"),
        "metadata": req_body.get("metadata").unwrap_or(&json!({}))
    })
}

pub fn wrap_openai_response_as_responses(
    req_body: &Value,
    response_id: &str,
    item_id: &str,
    created_at: i64,
    model_fallback: &str,
    obj: &Value,
) -> (Vec<Bytes>, Value) {
    let msg = obj
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let reasoning = extract_reasoning_from_chat_value(&msg);
    let tool_calls = msg.get("tool_calls").and_then(|v| v.as_array());
    let mut frames: Vec<Bytes> = Vec::new();
    let mut output_items: Vec<Value> = Vec::new();
    let mut next_output_index: u32 = 0;

    if let Some(reasoning) = reasoning {
        let reasoning_item_id = format!("rsn_{item_id}");
        let reasoning_item =
            responses_reasoning_output_item(&reasoning_item_id, reasoning, "completed");
        frames.push(responses_sse_line(&responses_output_item_added_event(
            response_id,
            next_output_index,
            responses_reasoning_output_item(&reasoning_item_id, "", "in_progress"),
        )));
        frames.push(responses_sse_line(
            &responses_reasoning_summary_part_added_event(
                response_id,
                &reasoning_item_id,
                next_output_index,
            ),
        ));
        frames.push(responses_sse_line(
            &responses_reasoning_summary_text_delta_event(
                response_id,
                &reasoning_item_id,
                next_output_index,
                reasoning,
            ),
        ));
        frames.push(responses_sse_line(&responses_reasoning_text_delta_event(
            response_id,
            &reasoning_item_id,
            next_output_index,
            reasoning,
        )));
        frames.push(responses_sse_line(&responses_output_item_done_event(
            response_id,
            next_output_index,
            reasoning_item.clone(),
        )));
        output_items.push(reasoning_item);
        next_output_index += 1;
    }

    if !content.is_empty() {
        frames.push(responses_sse_line(&responses_output_text_delta_event(
            response_id,
            item_id,
            next_output_index,
            0,
            content,
        )));
        let completed_message = responses_message_output_item(item_id, content, "completed");
        frames.push(responses_sse_line(&responses_output_item_done_event(
            response_id,
            next_output_index,
            completed_message.clone(),
        )));
        output_items.push(completed_message);
        next_output_index += 1;
    }

    if let Some(tc_array) = tool_calls {
        for (idx, tc) in tc_array.iter().enumerate() {
            let output_index = next_output_index + idx as u32;

            if !is_function_tool_call(tc) {
                let item = passthrough_output_item(tc, Some("completed"));
                frames.push(responses_sse_line(&responses_output_item_added_event(
                    response_id,
                    output_index,
                    passthrough_output_item(tc, Some("in_progress")),
                )));
                frames.push(responses_sse_line(&responses_output_item_done_event(
                    response_id,
                    output_index,
                    item.clone(),
                )));
                output_items.push(item);
                continue;
            }

            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let tc_fn = tc.get("function").cloned().unwrap_or_else(|| json!({}));
            let tc_name = tc_fn.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let tc_args = match tc_fn.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Object(_)) | Some(Value::Array(_)) => {
                    serde_json::to_string(tc_fn.get("arguments").unwrap())
                        .unwrap_or_else(|_| "{}".to_string())
                }
                _ => "{}".to_string(),
            };

            frames.push(responses_sse_line(&responses_output_item_added_event(
                response_id,
                output_index,
                json!({
                    "id": tc_id,
                    "type": "function_call",
                    "call_id": tc_id,
                    "name": tc_name,
                    "arguments": "",
                    "status": "in_progress"
                }),
            )));
            frames.push(responses_sse_line(&json!({
                "type": "response.function_call_arguments.delta",
                "response_id": response_id,
                "item_id": tc_id,
                "output_index": output_index,
                "delta": tc_args
            })));
            frames.push(responses_sse_line(&responses_function_call_done_event(
                response_id,
                tc_id,
                output_index,
                &tc_args,
            )));
            let completed_item =
                responses_function_call_output_item(tc_id, tc_name, &tc_args, "completed");
            frames.push(responses_sse_line(&responses_output_item_done_event(
                response_id,
                output_index,
                completed_item.clone(),
            )));
            output_items.push(completed_item);
        }
    }

    let finish_reason = obj
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str());

    let incomplete_details = responses_incomplete_details(finish_reason);
    let final_status = responses_final_status(finish_reason);
    let upstream_model = obj
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(model_fallback);
    let usage = obj.get("usage").cloned().unwrap_or_else(|| json!({}));
    let usage_obj = responses_usage_object(&usage);

    let completed_response = responses_completed_response(
        response_id,
        created_at,
        final_status,
        incomplete_details,
        req_body,
        upstream_model,
        output_items,
        if content.is_empty() {
            None
        } else {
            Some(content)
        },
        usage_obj,
    );

    (frames, completed_response)
}

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

struct ToolCallEntry {
    id: String,
    name: String,
    arguments: String,
    passthrough_item: Option<Value>,
    added_emitted: bool,
    assigned_index: u32,
}

pub fn build_responses_sse_http_response(
    frames: Vec<Bytes>,
) -> Result<axum::response::Response, crate::error::AppError> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(frames.len().max(1));

    tokio::spawn(async move {
        for frame in frames {
            if tx.send(Ok(frame)).await.is_err() {
                break;
            }
        }
    });

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        let item = rx.recv().await?;
        Some((item, rx))
    });

    axum::http::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/event-stream; charset=utf-8",
        )
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::CONNECTION, "close")
        .body(Body::from_stream(stream))
        .map_err(|e| {
            crate::error::AppError::Internal(format!("Failed to build Responses SSE response: {e}"))
        })
}

pub fn transform_openai_sse_to_responses_stream(
    response: axum::response::Response,
    initial_frames: Vec<Bytes>,
    response_id: String,
    item_id: String,
    model: String,
    req_body: Value,
    created_at: i64,
) -> Result<axum::response::Response, crate::error::AppError> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(256);
    let upstream_body = response.into_body();

    tokio::spawn(async move {
        macro_rules! send {
            ($frame:expr) => {
                if tx.send(Ok($frame)).await.is_err() {
                    return;
                }
            };
        }

        for frame in initial_frames {
            send!(frame);
        }

        let upstream_stream = upstream_body.into_data_stream();
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut full_content = String::new();
        let mut reasoning_content = String::new();
        let mut reasoning_added = false;
        let mut reasoning_done = false;
        let reasoning_item_id = format!("rsn_{item_id}");
        let mut usage = json!({});
        let mut finish_reason: Option<String> = None;
        let mut upstream_model: Option<String> = None;
        let mut content_len: usize = 0;
        let mut next_content_index: u32 = 1;
        let mut index_by_key: HashMap<String, u32> = HashMap::new();
        let mut tool_index_by_item_id: HashMap<String, u32> = HashMap::new();
        let mut last_tool_index: Option<u32> = None;
        let mut tool_accum: HashMap<usize, ToolCallEntry> = HashMap::new();
        let mut stream = upstream_stream;
        let mut idle_timeout = Box::pin(tokio::time::sleep(STREAM_IDLE_TIMEOUT));

        loop {
            let chunk_result = tokio::select! {
                _ = &mut idle_timeout => {
                    send!(responses_sse_line(&json!({
                        "type": "error",
                        "code": "stream_idle_timeout",
                        "message": "Stream idle timeout"
                    })));
                    break;
                }
                next = stream.next() => match next {
                    Some(result) => result,
                    None => break,
                }
            };

            let bytes = match chunk_result {
                Ok(b) => {
                    idle_timeout
                        .as_mut()
                        .reset(tokio::time::Instant::now() + STREAM_IDLE_TIMEOUT);
                    b
                }
                Err(_) => break,
            };

            content_len += bytes.len();
            if content_len > crate::proxy::sse::MAX_STREAM_BUFFER_BYTES {
                send!(responses_sse_line(&json!({
                    "type": "error",
                    "code": "content_too_large",
                    "message": "Stream content exceeds 10MB limit"
                })));
                break;
            }

            crate::proxy::sse::append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();
                let line = line.trim();

                if line.is_empty() {
                    continue;
                }

                if let Some(data) = crate::proxy::sse::sse_data_payload(line) {
                    if data == "[DONE]" {
                        if reasoning_added && !reasoning_done {
                            send!(responses_sse_line(&responses_output_item_done_event(
                                &response_id,
                                0,
                                responses_reasoning_output_item(
                                    &reasoning_item_id,
                                    &reasoning_content,
                                    "completed"
                                ),
                            )));
                            reasoning_done = true;
                        }

                        if !full_content.is_empty() {
                            let content_index = if reasoning_added { 1 } else { 0 };
                            send!(responses_sse_line(&responses_output_item_done_event(
                                &response_id,
                                content_index,
                                responses_message_output_item(&item_id, &full_content, "completed"),
                            )));
                        }

                        let mut sorted_indices: Vec<usize> = tool_accum.keys().copied().collect();
                        sorted_indices.sort();
                        for &idx in &sorted_indices {
                            let entry = &tool_accum[&idx];
                            if let Some(item) = &entry.passthrough_item {
                                send!(responses_sse_line(&responses_output_item_done_event(
                                    &response_id,
                                    entry.assigned_index,
                                    passthrough_output_item(item, Some("completed")),
                                )));
                            } else {
                                send!(responses_sse_line(&responses_function_call_done_event(
                                    &response_id,
                                    &entry.id,
                                    entry.assigned_index,
                                    &entry.arguments,
                                )));
                                send!(responses_sse_line(&responses_output_item_done_event(
                                    &response_id,
                                    entry.assigned_index,
                                    responses_function_call_output_item(
                                        &entry.id,
                                        &entry.name,
                                        &entry.arguments,
                                        "completed",
                                    ),
                                )));
                            }
                        }

                        let final_status = responses_final_status(finish_reason.as_deref());
                        let incomplete_details =
                            responses_incomplete_details(finish_reason.as_deref());
                        let resolved_model = upstream_model.as_deref().unwrap_or(&model);

                        let mut final_items: Vec<Value> = Vec::new();
                        if !reasoning_content.is_empty() {
                            final_items.push(responses_reasoning_output_item(
                                &reasoning_item_id,
                                &reasoning_content,
                                final_status,
                            ));
                        }
                        if !full_content.is_empty() {
                            final_items.push(responses_message_output_item(
                                &item_id,
                                &full_content,
                                final_status,
                            ));
                        }
                        for &idx in &sorted_indices {
                            let entry = &tool_accum[&idx];
                            if let Some(item) = &entry.passthrough_item {
                                final_items.push(passthrough_output_item(item, Some("completed")));
                            } else {
                                final_items.push(responses_function_call_output_item(
                                    &entry.id,
                                    &entry.name,
                                    &entry.arguments,
                                    "completed",
                                ));
                            }
                        }

                        let completed_response = responses_completed_response(
                            &response_id,
                            created_at,
                            final_status,
                            incomplete_details,
                            &req_body,
                            resolved_model,
                            final_items,
                            if full_content.is_empty() {
                                None
                            } else {
                                Some(full_content.as_str())
                            },
                            responses_usage_object(&usage),
                        );
                        send!(responses_sse_line(&json!({
                            "type": "response.completed",
                            "response": completed_response
                        })));
                        send!(responses_sse_done());
                        return;
                    }

                    if let Ok(chunk_obj) = serde_json::from_str::<Value>(data) {
                        if upstream_model.is_none() {
                            if let Some(m) = chunk_obj.get("model").and_then(|m| m.as_str()) {
                                upstream_model = Some(m.to_string());
                            }
                        }

                        if let Some(u) = chunk_obj.get("usage") {
                            usage = u.clone();
                        }

                        if let Some(fr) = chunk_obj
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("finish_reason"))
                            .and_then(|f| f.as_str())
                        {
                            if !fr.is_empty() {
                                finish_reason = Some(fr.to_string());
                            }
                        }

                        if let Some(tool_calls_delta) = chunk_obj
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("tool_calls"))
                            .and_then(|t| t.as_array())
                        {
                            for tc_delta in tool_calls_delta {
                                let tc_idx =
                                    tc_delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as usize;
                                let tc_id_new =
                                    tc_delta.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let tc_fn = tc_delta
                                    .get("function")
                                    .cloned()
                                    .unwrap_or_else(|| json!({}));
                                let tc_name_delta =
                                    tc_fn.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let is_function_delta =
                                    tc_delta.get("type").and_then(|v| v.as_str())
                                        == Some("function")
                                        || (tc_delta.get("type").is_none()
                                            && tc_delta.get("function").is_some());
                                let tc_args_delta = match tc_fn.get("arguments") {
                                    Some(Value::String(s)) => s.clone(),
                                    Some(Value::Object(_)) | Some(Value::Array(_)) => {
                                        serde_json::to_string(tc_fn.get("arguments").unwrap())
                                            .unwrap_or_else(|_| String::new())
                                    }
                                    _ => String::new(),
                                };

                                let entry =
                                    tool_accum.entry(tc_idx).or_insert_with(|| ToolCallEntry {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                        passthrough_item: None,
                                        added_emitted: false,
                                        assigned_index: 0,
                                    });

                                if !is_function_delta || entry.passthrough_item.is_some() {
                                    let item = entry.passthrough_item.get_or_insert_with(|| {
                                        passthrough_output_item(tc_delta, None)
                                    });
                                    merge_tool_delta(item, tc_delta);
                                    if !tc_id_new.is_empty() {
                                        entry.id = tc_id_new.to_string();
                                    }
                                    if !entry.added_emitted {
                                        entry.assigned_index = (tc_idx + 1) as u32;
                                        send!(responses_sse_line(
                                            &responses_output_item_added_event(
                                                &response_id,
                                                entry.assigned_index,
                                                passthrough_output_item(item, Some("in_progress")),
                                            )
                                        ));
                                        entry.added_emitted = true;
                                    }
                                    continue;
                                }

                                if !tc_id_new.is_empty() {
                                    entry.id = tc_id_new.to_string();
                                }
                                if !tc_name_delta.is_empty() {
                                    entry.name = tc_name_delta.to_string();
                                }
                                if !tc_args_delta.is_empty() {
                                    entry.arguments.push_str(&tc_args_delta);
                                }

                                let tool_key = if !entry.id.is_empty() {
                                    Some(format!("tool:{}", entry.id))
                                } else {
                                    None
                                };

                                let assigned_index = if let Some(ref k) = tool_key {
                                    if let Some(existing) = index_by_key.get(k).copied() {
                                        existing
                                    } else {
                                        let assigned = next_content_index;
                                        next_content_index += 1;
                                        index_by_key.insert(k.clone(), assigned);
                                        assigned
                                    }
                                } else {
                                    let assigned = next_content_index;
                                    next_content_index += 1;
                                    assigned
                                };

                                entry.assigned_index = assigned_index;
                                if !entry.id.is_empty() {
                                    tool_index_by_item_id.insert(entry.id.clone(), assigned_index);
                                    last_tool_index = Some(assigned_index);
                                }

                                if !entry.added_emitted && !entry.name.is_empty() {
                                    send!(responses_sse_line(&responses_output_item_added_event(
                                        &response_id,
                                        assigned_index,
                                        responses_function_call_output_item(
                                            &entry.id,
                                            &entry.name,
                                            "",
                                            "in_progress",
                                        ),
                                    )));
                                    entry.added_emitted = true;
                                }

                                if !tc_args_delta.is_empty() {
                                    let delta_index = tool_index_by_item_id
                                        .get(&entry.id)
                                        .copied()
                                        .or(last_tool_index)
                                        .unwrap_or(assigned_index);
                                    send!(responses_sse_line(&json!({
                                        "type": "response.function_call_arguments.delta",
                                        "response_id": &response_id,
                                        "item_id": &entry.id,
                                        "output_index": delta_index,
                                        "delta": tc_args_delta
                                    })));
                                }
                            }
                        }

                        if let Some(delta) = chunk_obj
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("delta"))
                            .and_then(extract_reasoning_from_chat_value)
                        {
                            if !reasoning_added {
                                send!(responses_sse_line(&responses_output_item_added_event(
                                    &response_id,
                                    0,
                                    responses_reasoning_output_item(
                                        &reasoning_item_id,
                                        "",
                                        "in_progress"
                                    ),
                                )));
                                send!(responses_sse_line(
                                    &responses_reasoning_summary_part_added_event(
                                        &response_id,
                                        &reasoning_item_id,
                                        0,
                                    )
                                ));
                                reasoning_added = true;
                            }
                            reasoning_content.push_str(delta);
                            send!(responses_sse_line(
                                &responses_reasoning_summary_text_delta_event(
                                    &response_id,
                                    &reasoning_item_id,
                                    0,
                                    delta,
                                )
                            ));
                            send!(responses_sse_line(&responses_reasoning_text_delta_event(
                                &response_id,
                                &reasoning_item_id,
                                0,
                                delta,
                            )));
                        }

                        if let Some(content) = chunk_obj
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !content.is_empty() {
                                if reasoning_added && !reasoning_done {
                                    send!(responses_sse_line(&responses_output_item_done_event(
                                        &response_id,
                                        0,
                                        responses_reasoning_output_item(
                                            &reasoning_item_id,
                                            &reasoning_content,
                                            "completed"
                                        ),
                                    )));
                                    reasoning_done = true;
                                }
                                let content_index = if reasoning_added { 1 } else { 0 };
                                if full_content.is_empty() {
                                    send!(responses_sse_line(&responses_output_item_added_event(
                                        &response_id,
                                        content_index,
                                        json!({
                                            "type": "message",
                                            "role": "assistant",
                                            "id": &item_id,
                                            "status": "in_progress",
                                            "content": []
                                        }),
                                    )));
                                }
                                full_content.push_str(content);
                                send!(responses_sse_line(&responses_output_text_delta_event(
                                    &response_id,
                                    &item_id,
                                    content_index,
                                    0,
                                    content,
                                )));
                            }
                        }
                    }
                }
            }
        }

        send!(responses_sse_done());
    });

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });

    axum::http::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/event-stream; charset=utf-8",
        )
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::CONNECTION, "close")
        .body(Body::from_stream(stream))
        .map_err(|e| {
            crate::error::AppError::Internal(format!(
                "Failed to build Responses stream response: {e}"
            ))
        })
}

/// Responses request standard field whitelist (based on official docs)
/// Ref: https://developers.openai.com/api/reference/resources/responses/methods/create
/// Note: model, input, instructions, tools, max_output_tokens, reasoning, response_format
/// have already been handled in transform_request_to_responses
const RESPONSES_REQUEST_STANDARD_FIELDS: &[&str] = &[
    "background",
    "conversation",
    "include",
    "context_management",
    "max_tool_calls",
    "metadata",
    "parallel_tool_calls",
    "previous_response_id",
    "prompt",
    "prompt_cache_key",
    "prompt_cache_retention",
    "safety_identifier",
    "service_tier",
    "store",
    "stream",
    "temperature",
    "text",
    "tool_choice",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
];

/// Responses request extension field whitelist
const RESPONSES_REQUEST_EXTENSION_FIELDS: &[&str] = &["x_responses_future_field"];

/// Responses response standard field whitelist (based on official docs)
/// Ref: https://developers.openai.com/api/reference/resources/responses/
/// Note: id, object, created_at, model, output, usage, status, error,
/// incomplete_details, metadata have already been constructed in responses_completed_response
/// Note: max_output_tokens, max_tool_calls are request params only, not appearing in response
const RESPONSES_RESPONSE_STANDARD_FIELDS: &[&str] = &[
    "instructions",
    "parallel_tool_calls",
    "temperature",
    "tool_choice",
    "tools",
    "top_p",
    "background",
    "conversation",
    "previous_response_id",
    "prompt",
    "prompt_cache_key",
    "prompt_cache_retention",
    "safety_identifier",
    "service_tier",
    "store",
    "truncation",
    "user",
];

/// Responses response extension field whitelist
const RESPONSES_RESPONSE_EXTENSION_FIELDS: &[&str] = &[
    "x_responses_future_field",
    "x_future_response_field",
    "reasoning",
];

pub struct ResponsesAdapter;

impl ProtocolAdapter for ResponsesAdapter {
    fn build_chat_url(&self, base_url: &str, _model: &str) -> String {
        join_url(base_url, "v1/responses")
    }

    fn build_models_url(&self, base_url: &str, _api_key: &str) -> String {
        // Responses API reuses /v1/models list endpoint
        join_url(base_url, "v1/models")
    }

    fn uses_query_auth(&self) -> bool {
        false
    }

    fn build_auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![("Authorization".to_string(), format!("Bearer {api_key}"))]
    }

    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        builder.header("Authorization", format!("Bearer {api_key}"))
    }

    fn transform_request(&self, body: &mut Value, actual_model: &str) {
        transform_request_to_responses(body, actual_model);
    }

    fn transform_response(&self, body: &mut Value) {
        transform_response_from_responses(body);
    }

    fn needs_sse_transform(&self) -> bool {
        // Enable SSE conversion: translate upstream Responses native SSE events to Chat Completions SSE,
        // allowing forwarder success evaluation and downstream handler to recognize stream output.
        // See docs/REFACTOR_PLAN.md section 9.
        true
    }

    fn extract_sse_usage(&self, data_line: &str) -> (i64, i64) {
        if data_line == "[DONE]" {
            return (0, 0);
        }
        let Ok(value) = serde_json::from_str::<Value>(data_line) else {
            return (0, 0);
        };
        // Responses SSE usage format: response.completed event carries usage.input_tokens/output_tokens
        let prompt = value
            .pointer("/response/usage/input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                value
                    .get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            });
        let completion = value
            .pointer("/response/usage/output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                value
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            });
        (prompt, completion)
    }

    /// Translate Responses native SSE event lines to Chat Completions SSE format.
    ///
    /// Mapping rules:
    /// | Responses Event | Chat SSE Output | Description |
    /// |---|---:|---|
    /// | `response.output_text.delta` | `choices[0].delta.content` | Text delta |
    /// | `response.output_item.added` (function_call) | `choices[0].delta.tool_calls[N]` (with id + name) | Tool call start |
    /// | `response.function_call_arguments.delta` | `choices[0].delta.tool_calls[N].function.arguments` | Tool args delta |
    /// | `response.completed` | `choices[0].finish_reason` + `usage` | Stream completion signal |
    /// | `response.created` / `.done` / other status events | `None` (skip) | No Chat counterpart |
    fn transform_sse_line(&self, data_line: &str) -> Option<String> {
        let Ok(value) = serde_json::from_str::<Value>(data_line) else {
            // Non-JSON lines (e.g. comment lines) passthrough as-is
            return Some(data_line.to_string());
        };

        let typ = match value.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => {
                // Lines without type field (e.g. Chat SSE usage events) passthrough as-is
                return Some(data_line.to_string());
            }
        };

        match typ {
            // -- Text delta ----------------------------------------------
            "response.output_text.delta" => {
                let delta = value.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return None;
                }
                Some(chat_sse_text_delta(delta))
            }

            // -- Reasoning delta -----------------------------------------
            "response.reasoning_text.delta" => {
                let delta = value.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return None;
                }
                Some(chat_sse_reasoning_delta(delta))
            }

            // -- Tool call start (output_item.added + function_call) -----
            "response.output_item.added" => {
                let item = value.get("item")?;
                let item_type = item.get("type").and_then(|v| v.as_str())?;
                if item_type != "function_call" {
                    // Non-function output item has no Chat SSE equivalent
                    return None;
                }
                let output_index = value
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let call_id = item
                    .get("id")
                    .or_else(|| item.get("call_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
                Some(chat_sse_tool_call_begin(
                    output_index,
                    call_id,
                    name,
                    arguments,
                ))
            }

            // -- Reasoning item complete (MIMO Responses places reasoning in encrypted_content) --
            "response.output_item.done" => {
                let item = value.get("item")?;
                if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                    return extract_reasoning_from_responses_item(item)
                        .map(chat_sse_reasoning_delta);
                }
                None
            }

            // -- Tool parameter delta ------------------------------------
            "response.function_call_arguments.delta" => {
                let delta = value.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return None;
                }
                let output_index = value
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                Some(chat_sse_tool_call_args(output_index, delta))
            }

            // -- Stream completed ----------------------------------------
            "response.completed" => {
                let resp = value.get("response");
                let status = resp
                    .and_then(|r| r.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("completed");
                let finish_reason = match status {
                    "incomplete" => "length",
                    "failed" => "error",
                    _ => "stop",
                };
                let usage = resp.and_then(|r| r.get("usage"));
                let input_tokens = usage
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let output_tokens = usage
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);

                Some(chat_sse_completed(
                    finish_reason,
                    input_tokens,
                    output_tokens,
                ))
            }

            // -- Failure event -------------------------------------------
            "response.failed" => Some(chat_sse_error(
                value
                    .pointer("/response/error")
                    .or_else(|| value.get("error")),
            )),

            // -- Responses events without Chat SSE equivalent ------------
            // Responses lifecycle/status events must not leak as-isto downstream Chat; otherwise
            // clients expecting choices/error envelopes will treat valid upstream events as malformed.
            event if event.starts_with("response.") => None,

            // Preserve non-Responses JSON SSE (e.g. Chat style errors already emitted by upstream).
            _ => Some(data_line.to_string()),
        }
    }

    fn parse_models_response(&self, body: &Value) -> Vec<(String, Option<String>)> {
        // OpenAI standard /v1/models response format
        body.get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str()?.to_string();
                        let owned_by = m.get("owned_by").and_then(|v| v.as_str()).map(String::from);
                        Some((id, owned_by))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════════════════
//  chat.completions -> Responses API request
// ═══════════════════════════════════════════════════════════════════

/// Translate chat.completions format request into Responses API request.
///
/// Core mapping (according to official OpenAI docs):
/// - `model` → `model`(direct)
/// - `messages[]` → `input[]`(event stream format)
///   - system/developer → top-level `instructions` (take first)
///   - user message → `input_text` item
///   - assistant message → `message` item（role=assistant）
///   - assistant tool_calls → `function_call` items
///   - tool message → `function_call_output` item
/// - `max_tokens` → `max_output_tokens`
/// - `stop` → Responses API no direct counterpart, preserved in body (passthrough)
/// - `stream` → preserved
/// - `temperature`/`top_p` → preserved
/// - `tools[]` → `tools[]`(function tools unwrap function nesting)
/// - `tool_choice` → `tool_choice`(string or object direct passthrough)
/// - `response_format` → `text.format`(official correspondence)
/// - Unknown fields: all preserved when ENABLE_UNKNOWN_FIELD_PASSTHROUGH=true
fn transform_request_to_responses(body: &mut Value, actual_model: &str) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // When already a native Responses request, only update model name, preserving reasoning.summary,
    // include: ["reasoning.encrypted_content"] and other Responses proprietary fields.
    if obj.contains_key("input") && !obj.contains_key("messages") {
        obj.insert("model".to_string(), json!(actual_model));
        return;
    }

    // 1. Collect and convert messages -> input
    let (instructions, input_items) = messages_to_input(obj.remove("messages"));

    // 2. Construct Responses request skeleton
    let mut responses = serde_json::Map::new();
    responses.insert("model".to_string(), json!(actual_model));
    responses.insert("input".to_string(), json!(input_items));
    if let Some(inst) = instructions {
        if !inst.is_empty() {
            responses.insert("instructions".to_string(), json!(inst));
        }
    }

    // 3. Map known fields
    if let Some(max_tokens) = obj
        .remove("max_completion_tokens")
        .or_else(|| obj.remove("max_tokens"))
    {
        responses.insert("max_output_tokens".to_string(), max_tokens);
    }

    // tools: from chat.completions {type:"function", function:{name,...}}
    // to Responses {type:"function", name, ...} (unwrap function nesting)
    if let Some(tools) = obj.remove("tools") {
        responses.insert("tools".to_string(), convert_tools_to_responses(&tools));
    }

    // response_format -> text.format (per official docs)
    if let Some(rf) = obj.remove("response_format") {
        responses.insert("text".to_string(), json!({ "format": rf }));
    }

    // reasoning_effort: Chat API flat field -> Responses reasoning object.
    // If request has native reasoning, update effort while preserving summary etc.
    if let Some(effort) = obj.remove("reasoning_effort") {
        let mut reasoning = obj
            .remove("reasoning")
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        if let Some(reasoning_obj) = reasoning.as_object_mut() {
            reasoning_obj.insert("effort".to_string(), effort);
        }
        responses.insert("reasoning".to_string(), reasoning);
    } else if let Some(reasoning) = obj.remove("reasoning") {
        // Preserve native reasoning field (even without reasoning_effort)
        responses.insert("reasoning".to_string(), reasoning);
    }

    // 4. Copy other known standard fields (Responses API standard fields)
    // Copy Responses standard fields
    for field in RESPONSES_REQUEST_STANDARD_FIELDS {
        if let Some(val) = obj.remove(*field) {
            responses.insert((*field).to_string(), val);
        }
    }

    // 5. Whitelist passthrough: only preserve declared Responses extension fields
    for key in RESPONSES_REQUEST_EXTENSION_FIELDS {
        if let Some(value) = obj.get(*key) {
            if !responses.contains_key(*key) {
                responses.insert((*key).to_string(), value.clone());
            }
        }
    }

    *body = Value::Object(responses);
}

/// Convert chat.completions messages[] to Responses (instructions, input[]).
///
/// Returns (instructions, input_items).
/// - First system/developer message promoted to instructions (top-level parameter)
/// - Subsequent system messages preserved as input items
/// - user message -> input_text item
/// - assistant message -> message item (may carry tool_calls)
/// - tool message -> function_call_output item
fn messages_to_input(messages: Option<Value>) -> (Option<String>, Vec<Value>) {
    let Some(Value::Array(msgs)) = messages else {
        return (None, Vec::new());
    };

    let mut instructions: Option<String> = None;
    let mut input_items: Vec<Value> = Vec::new();

    for msg in msgs {
        let Some(obj) = msg.as_object() else {
            continue;
        };
        let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = obj.get("content");

        match role {
            "system" | "developer" => {
                // First system message becomes instructions
                let text = extract_text_content(content);
                if instructions.is_none() {
                    instructions = Some(text);
                } else {
                    // Subsequent system messages go into input
                    input_items.push(json!({
                        "type": "message",
                        "role": "system",
                        "content": [{ "type": "input_text", "text": text }]
                    }));
                }
            }
            "user" => {
                let parts = user_content_to_responses_parts(content);
                input_items.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": parts,
                }));
            }
            "assistant" => {
                // assistant may have both content and tool_calls
                let text = extract_text_content(content);
                if !text.is_empty() {
                    input_items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }]
                    }));
                }
                if let Some(tool_calls) = obj.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let fn_obj = tc.get("function");
                        let name = fn_obj
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args = fn_obj
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        input_items.push(json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": args,
                        }));
                    }
                }
            }
            "tool" => {
                let call_id = obj
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = extract_text_content(content);
                input_items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            other => {
                // Unknown role, passthrough
                input_items.push(json!({
                    "type": "message",
                    "role": other,
                    "content": content.cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }

    (instructions, input_items)
}

fn extract_text_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| part.as_str())
                    .map(String::from)
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(Value::Null) | None => String::new(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Convert chat.completions user message content to Responses input content parts.
/// Supports strings and content block arrays (including image_url etc.).
fn user_content_to_responses_parts(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => vec![json!({ "type": "input_text", "text": s })],
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| {
                if let Some(obj) = p.as_object() {
                    let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match typ {
                        "text" => {
                            let text = obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            json!({ "type": "input_text", "text": text })
                        }
                        "image_url" => {
                            let url = obj
                                .get("image_url")
                                .and_then(|u| u.get("url"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let detail = obj
                                .get("image_url")
                                .and_then(|u| u.get("detail"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("auto");
                            json!({
                                "type": "input_image",
                                "image_url": url,
                                "detail": detail,
                            })
                        }
                        _ => p.clone(), // Unknown type passthrough
                    }
                } else if let Some(s) = p.as_str() {
                    json!({ "type": "input_text", "text": s })
                } else {
                    p.clone()
                }
            })
            .collect(),
        Some(other) => vec![json!({
            "type": "input_text",
            "text": serde_json::to_string(other).unwrap_or_default()
        })],
        None => Vec::new(),
    }
}

/// Convert chat.completions tools[] to Responses tools[].
/// chat: `{type:"function", function:{name,...}}` → resp: `{type:"function", name,...}`
fn convert_tools_to_responses(tools: &Value) -> Value {
    let Some(arr) = tools.as_array() else {
        return tools.clone();
    };

    let converted: Vec<Value> = arr
        .iter()
        .map(|tool| {
            let Some(obj) = tool.as_object() else {
                return tool.clone();
            };
            let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            // function tool needs to unwrap function nesting
            if typ == "function" {
                if let Some(func) = obj.get("function").and_then(|v| v.as_object()) {
                    let mut new_tool = serde_json::Map::new();
                    new_tool.insert("type".to_string(), json!("function"));
                    for (k, v) in func.iter() {
                        new_tool.insert(k.clone(), v.clone());
                    }
                    // Whitelist passthrough: only preserve declared Responses extension fields
                    for key in RESPONSES_REQUEST_EXTENSION_FIELDS {
                        if let Some(v) = obj.get(*key) {
                            if !new_tool.contains_key(*key) {
                                new_tool.insert((*key).to_string(), v.clone());
                            }
                        }
                    }
                    return Value::Object(new_tool);
                }
            }
            // Non-function tool: direct passthrough
            tool.clone()
        })
        .collect();

    json!(converted)
}

// ═══════════════════════════════════════════════════════════════════
//  Responses API response -> chat.completions response
// ═══════════════════════════════════════════════════════════════════

/// Translate Responses API response into chat.completions response.
///
/// Core mapping:
/// - message item in `output[]` -> choices[0].message.content
/// - function_call item in `output[]` -> choices[0].message.tool_calls
/// - `usage.input_tokens` → `usage.prompt_tokens`
/// - `usage.output_tokens` → `usage.completion_tokens`
/// - `status` -> finish_reason mapping
/// - Unknown fields passthrough
fn transform_response_from_responses(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // Extract output array
    let output = obj.remove("output");
    let mut content_text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(Value::Array(items)) = output {
        for item in items {
            let Some(item_obj) = item.as_object() else {
                continue;
            };
            let typ = item_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match typ {
                "message" => {
                    // Concatenate output_text in message.content[]
                    if let Some(parts) = item_obj.get("content").and_then(|v| v.as_array()) {
                        for part in parts {
                            let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if part_type == "output_text" || part_type == "text" {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    content_text.push_str(text);
                                }
                            }
                        }
                    }
                }
                "function_call" => {
                    let call_id = item_obj
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .or_else(|| item_obj.get("id").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let name = item_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args = item_obj
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    tool_calls.push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args,
                        }
                    }));
                }
                "reasoning" => {
                    if let Some(reasoning) = extract_reasoning_from_responses_item(&item) {
                        reasoning_text.push_str(reasoning);
                    }
                }
                _ => {}
            }
        }
    }

    // Construct message
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), json!(content_text));
    if !reasoning_text.is_empty() {
        message.insert("reasoning_text".to_string(), json!(reasoning_text));
        message.insert("reasoning_content".to_string(), json!(reasoning_text));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), json!(tool_calls));
    }

    // finish_reason mapping
    let finish_reason = match obj.get("status").and_then(|v| v.as_str()) {
        Some("completed") => {
            if !tool_calls.is_empty() {
                "tool_calls"
            } else {
                "stop"
            }
        }
        Some("incomplete") => {
            // Inspect incomplete_details.reason
            obj.get("incomplete_details")
                .and_then(|d| d.get("reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("length")
        }
        Some(other) => other,
        None => "stop",
    };

    let choice = json!({
        "index": 0,
        "message": Value::Object(message),
        "finish_reason": finish_reason,
    });

    // usage mapping
    let usage_src = obj.remove("usage").unwrap_or(json!({}));
    let prompt_tokens = usage_src
        .get("input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion_tokens = usage_src
        .get("output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total_tokens = usage_src
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(prompt_tokens + completion_tokens);
    let mut usage_out = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    });
    // cached_tokens / reasoning_tokens preserved
    if let Some(cached) = usage_src
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_i64)
    {
        if cached > 0 {
            usage_out["prompt_tokens_details"] = json!({ "cached_tokens": cached });
        }
    }
    if let Some(reasoning) = usage_src
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_i64)
    {
        if reasoning > 0 {
            usage_out["completion_tokens_details"] = json!({ "reasoning_tokens": reasoning });
        }
    }

    // Construct chat.completions response skeleton
    let mut chat_response = serde_json::Map::new();
    if let Some(id) = obj.remove("id") {
        chat_response.insert("id".to_string(), id);
    }
    chat_response.insert("object".to_string(), json!("chat.completion"));
    if let Some(created) = obj.remove("created_at").or_else(|| obj.remove("created")) {
        chat_response.insert("created".to_string(), created);
    } else {
        chat_response.insert("created".to_string(), json!(chrono::Utc::now().timestamp()));
    }
    if let Some(model) = obj.remove("model") {
        chat_response.insert("model".to_string(), model);
    }
    chat_response.insert("choices".to_string(), json!([choice]));
    chat_response.insert("usage".to_string(), usage_out);

    // Whitelist passthrough: only preserve declared Responses extension fields
    for key in RESPONSES_RESPONSE_EXTENSION_FIELDS {
        if let Some(value) = obj.get(*key) {
            if !chat_response.contains_key(*key) {
                chat_response.insert((*key).to_string(), value.clone());
            }
        }
    }

    *body = Value::Object(chat_response);
}

// ═══════════════════════════════════════════════════════════════════
//  Unit tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_points_to_v1_responses() {
        let a = ResponsesAdapter;
        assert_eq!(
            a.build_chat_url("https://api.openai.com", "gpt-4o"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            a.build_chat_url("https://api.openai.com/v1", "gpt-4o"),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn auth_is_bearer_token() {
        let a = ResponsesAdapter;
        let headers = a.build_auth_headers("sk-abc");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer sk-abc");
    }

    #[test]
    fn transform_request_basic_user_message() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        a.transform_request(&mut body, "gpt-4o");

        assert_eq!(body["model"], "gpt-4o");
        assert!(body.get("input").is_some());
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn transform_request_system_becomes_instructions() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "Hi"}
            ]
        });
        a.transform_request(&mut body, "gpt-4o");

        assert_eq!(body["instructions"], "Be brief.");
        let input = body["input"].as_array().unwrap();
        // system moved to instructions, only user remains in input
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
    }

    #[test]
    fn transform_request_max_tokens_becomes_max_output_tokens() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 1000
        });
        a.transform_request(&mut body, "gpt-4o");

        assert_eq!(body["max_output_tokens"], 1000);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn responses_to_chat_downgrades_incomplete_json_schema_format() {
        let req = json!({
            "model": "gpt-5.4-mini",
            "input": "Return JSON",
            "text": {"format": {"type": "json_schema"}}
        });

        let (body, _, _) = responses_to_openai_chat_request(&req);

        assert_eq!(body["response_format"], json!({"type": "json_object"}));
    }

    #[test]
    fn responses_to_chat_drops_empty_tools_and_tool_controls() {
        let req = json!({
            "model": "gpt-5.4-mini",
            "input": "Hi",
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "max_tool_calls": 1
        });

        let (body, _, _) = responses_to_openai_chat_request(&req);

        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
        assert!(body.get("max_tool_calls").is_none());
    }

    #[test]
    fn responses_to_chat_drops_hosted_tools_instead_of_catch_all_passthrough() {
        let req = json!({
            "model": "gpt-5.4-mini",
            "input": "Search web",
            "tools": [{"type": "web_search_preview"}],
            "tool_choice": "auto"
        });

        let (body, _, _) = responses_to_openai_chat_request(&req);

        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn transform_request_tools_unnest_function() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Weather?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object"}
                }
            }]
        });
        a.transform_request(&mut body, "gpt-4o");

        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        // Unwrapped directly at top level
        assert_eq!(tool["name"], "get_weather");
        assert_eq!(tool["description"], "Get weather");
        // No longer has nested function field
        assert!(tool.get("function").is_none());
    }

    #[test]
    fn transform_request_output_boundary_filters_unknown_fields() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "x_custom_tracking": "abc-123",
            "x_future_openai_field": {"nested": true},
            "temperature": 0.2,
            "tool_choice": "auto"
        });
        a.transform_request(&mut body, "gpt-4o");

        assert!(body.get("input").is_some());
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["tool_choice"], "auto");
        assert!(body.get("messages").is_none());
        assert!(body.get("x_custom_tracking").is_none());
        assert!(body.get("x_future_openai_field").is_none());
    }

    #[test]
    fn transform_request_to_responses_drops_chat_only_fields() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "response_format": {"type": "json_object"},

            "n": 2,
            "logit_bias": {"123": 1},
            "logprobs": true,
            "top_logprobs": 3,
            "presence_penalty": 0.5,
            "frequency_penalty": 0.5,
            "seed": 42,
            "stream_options": {"include_usage": true},
            "modalities": ["text"],
            "audio": {"voice": "alloy"},
            "prediction": {"type": "content", "content": "x"}
        });
        a.transform_request(&mut body, "gpt-4o");

        assert!(body.get("input").is_some());
        assert!(body.get("text").is_some());
        assert_eq!(body["temperature"], 0.2);

        assert!(body.get("messages").is_none());
        assert!(body.get("response_format").is_none());
        assert!(body.get("n").is_none());
        assert!(body.get("logit_bias").is_none());
        assert!(body.get("logprobs").is_none());
        // top_logprobs is standard field supported by Responses API, should be preserved
        assert!(body.get("top_logprobs").is_some());
        assert!(body.get("presence_penalty").is_none());
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("seed").is_none());
        assert!(body.get("stream_options").is_none());
        assert!(body.get("modalities").is_none());
        assert!(body.get("audio").is_none());
        assert!(body.get("prediction").is_none());
    }

    #[test]
    fn transform_request_assistant_tool_calls_become_function_call_items() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "Weather?"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"SF\"}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_abc", "content": "Sunny"}
            ]
        });
        a.transform_request(&mut body, "gpt-4o");

        let input = body["input"].as_array().unwrap();
        // user + function_call + function_call_output
        assert_eq!(input.len(), 3);
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_abc");
        assert_eq!(input[1]["name"], "get_weather");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_abc");
        assert_eq!(input[2]["output"], "Sunny");
    }

    #[test]
    fn transform_response_basic_text() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "id": "resp_123",
            "object": "response",
            "status": "completed",
            "model": "gpt-4o",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello!"}]
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        });
        a.transform_response(&mut body);

        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["id"], "resp_123");
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(body["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["prompt_tokens"], 10);
        assert_eq!(body["usage"]["completion_tokens"], 5);
        assert_eq!(body["usage"]["total_tokens"], 15);
    }

    #[test]
    fn transform_response_function_call_becomes_tool_calls() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "id": "resp_456",
            "status": "completed",
            "model": "gpt-4o",
            "output": [{
                "type": "function_call",
                "call_id": "call_xyz",
                "name": "get_weather",
                "arguments": "{\"city\":\"Tokyo\"}"
            }],
            "usage": {"input_tokens": 20, "output_tokens": 10}
        });
        a.transform_response(&mut body);

        let tool_calls = body["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_xyz");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert_eq!(
            tool_calls[0]["function"]["arguments"],
            "{\"city\":\"Tokyo\"}"
        );
        // When tool_calls exist, finish_reason should be tool_calls
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn transform_response_unknown_fields_passthrough() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "id": "resp_789",
            "status": "completed",
            "model": "gpt-4o",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hi"}]
            }],
            "usage": {"input_tokens": 5, "output_tokens": 3},
            "x_future_response_field": "preserve_me",
            "reasoning": {"effort": "high"}
        });
        a.transform_response(&mut body);

        // Axiom 2: response direction unknown fields must also passthrough
        assert_eq!(body["x_future_response_field"], "preserve_me");
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn completed_response_drops_foreign_protocol_fields() {
        let req_body = json!({
            "model": "gpt-4o",
            "input": "hi",
            "temperature": 0.1
        });
        let usage = json!({"input_tokens": 1, "output_tokens": 1, "total_tokens": 2});
        let mut response = responses_completed_response(
            "resp_1",
            123,
            "completed",
            json!(null),
            &req_body,
            "gpt-4o",
            vec![responses_message_output_item("msg_1", "hi", "completed")],
            Some("hi"),
            usage,
        );

        if let Some(obj) = response.as_object_mut() {
            obj.insert("choices".to_string(), json!([]));
            obj.insert("candidates".to_string(), json!([]));
            obj.insert("usageMetadata".to_string(), json!({}));
            obj.insert("stop_reason".to_string(), json!("end_turn"));
        }

        filter_responses_response_fields(response.as_object_mut().unwrap());

        assert!(response.get("choices").is_none());
        assert!(response.get("candidates").is_none());
        assert!(response.get("usageMetadata").is_none());
        assert!(response.get("stop_reason").is_none());
        assert_eq!(response["object"], "response");
        assert!(response.get("output").is_some());
    }

    #[test]
    fn completed_response_filters_to_responses_response_whitelist() {
        let req_body = json!({
            "model": "gpt-4o",
            "input": "hi",
            "instructions": "be concise",
            "max_output_tokens": 128,
            "parallel_tool_calls": false,
            "text": {"format": {"type": "text"}},
            "temperature": 0.2,
            "tool_choice": "auto",
            "tools": [],
            "top_p": 0.9,
            "truncation": "auto",
            "store": false,
            "metadata": {"trace": "abc"}
        });
        let usage = json!({"input_tokens": 1, "output_tokens": 1, "total_tokens": 2});

        let response = responses_completed_response(
            "resp_whitelist",
            123,
            "completed",
            json!(null),
            &req_body,
            "gpt-4o",
            vec![responses_message_output_item("msg_1", "hi", "completed")],
            Some("hi"),
            usage,
        );

        assert_eq!(response["instructions"], "be concise");
        assert_eq!(response["parallel_tool_calls"], false);
        assert_eq!(response["temperature"], 0.2);
        assert_eq!(response["tool_choice"], "auto");
        assert_eq!(response["tools"], json!([]));
        assert_eq!(response["top_p"], 0.9);
        assert_eq!(response["truncation"], "auto");
        assert_eq!(response["store"], false);
        assert_eq!(response["metadata"]["trace"], "abc");

        assert!(response.get("max_output_tokens").is_none());
        assert!(response.get("output_text").is_none());
        assert!(response.get("text").is_none());
        assert!(response.get("reasoning").is_some());
    }

    #[test]
    fn transform_response_incomplete_maps_to_length() {
        let a = ResponsesAdapter;
        let mut body = json!({
            "id": "resp_inc",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "model": "gpt-4o",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "partial"}]
            }],
            "usage": {"input_tokens": 5, "output_tokens": 100}
        });
        a.transform_response(&mut body);

        assert_eq!(body["choices"][0]["finish_reason"], "max_output_tokens");
    }

    #[test]
    fn responses_sse_lifecycle_events_do_not_leak_to_chat_stream() {
        let a = ResponsesAdapter;

        for event in [
            json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}}),
            json!({"type": "response.in_progress", "response": {"id": "resp_1", "status": "in_progress"}}),
            json!({"type": "response.content_part.added", "item_id": "msg_1", "content_index": 0}),
            json!({"type": "response.output_text.done", "item_id": "msg_1", "text": "done"}),
            json!({"type": "response.content_part.done", "item_id": "msg_1", "content_index": 0}),
        ] {
            let line = serde_json::to_string(&event).unwrap();
            assert_eq!(a.transform_sse_line(&line), None);
        }
    }

    #[test]
    fn responses_sse_text_delta_still_maps_to_chat_choices() {
        let a = ResponsesAdapter;
        let line = serde_json::to_string(&json!({
            "type": "response.output_text.delta",
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "delta": "hello"
        }))
        .unwrap();

        let transformed = a.transform_sse_line(&line).unwrap();
        let value: Value = serde_json::from_str(&transformed).unwrap();

        assert_eq!(value["choices"][0]["delta"]["content"], "hello");
    }

    #[test]
    fn responses_sse_failed_maps_to_chat_error_envelope() {
        let a = ResponsesAdapter;
        let line = serde_json::to_string(&json!({
            "type": "response.failed",
            "response": {
                "id": "resp_1",
                "status": "failed",
                "error": {
                    "message": "upstream failed",
                    "type": "server_error",
                    "code": "boom"
                }
            }
        }))
        .unwrap();

        let transformed = a.transform_sse_line(&line).unwrap();
        let value: Value = serde_json::from_str(&transformed).unwrap();

        assert!(value.get("choices").is_none());
        assert_eq!(value["error"]["message"], "upstream failed");
        assert_eq!(value["error"]["type"], "server_error");
        assert_eq!(value["error"]["code"], "boom");
    }

    #[test]
    fn responses_sse_non_failed_response_events_do_not_leak_to_openai_clients() {
        let a = ResponsesAdapter;

        // These response.* events should not leak to OpenAI compatible clients
        for event in [
            // response.output_item.added - output item added event
            json!({"type": "response.output_item.added", "response_id": "resp_1", "output_index": 0, "item": {"type": "message"}}),
            // response.output_item.done - output item done event
            json!({"type": "response.output_item.done", "response_id": "resp_1", "output_index": 0, "item": {"type": "message"}}),
            // response.function_call_arguments.done - function call arguments done event
            json!({"type": "response.function_call_arguments.done", "response_id": "resp_1", "item_id": "call_1", "output_index": 0, "arguments": "{}"}),
            // response.output_text.done - output text done event
            json!({"type": "response.output_text.done", "response_id": "resp_1", "item_id": "msg_1", "text": "done"}),
            // response.reasoning_summary_text.delta - reasoning summary delta (not error)
            json!({"type": "response.reasoning_summary_text.delta", "response_id": "resp_1", "item_id": "rsn_1", "output_index": 0, "summary_index": 0, "delta": "thinking"}),
        ] {
            let line = serde_json::to_string(&event).unwrap();
            let result = a.transform_sse_line(&line);
            // Non-failure events should not leak to downstream Chat
            assert_eq!(result, None, "Event {} should be filtered", event["type"]);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Downstream: Responses API -> chat.completions format conversion
// ═══════════════════════════════════════════════════════════════════

/// Convert Responses API `input` field to Chat Completions `messages` array.
///
/// `input` can be:
/// - Raw string -> single user message
/// - Messages array: string, message object, function_call, function_call_output
/// - Object (serialized as user message)
///
/// Multi-turn tool usage: function_call -> assistant tool_calls,
/// function_call_output → tool message。
pub fn input_to_messages(input: &Value, instructions: Option<&str>) -> Vec<Value> {
    let mut msgs: Vec<Value> = Vec::new();

    // Optional system message comes from `instructions`
    if let Some(inst) = instructions {
        if !inst.is_empty() {
            msgs.push(json!({ "role": "system", "content": inst }));
        }
    }

    match input {
        Value::String(s) => {
            msgs.push(json!({ "role": "user", "content": s }));
        }
        Value::Array(items) => {
            // Pair consecutive function_call + function_call_output
            // combine into assistant tool_calls message + separate tool message
            let mut i = 0;
            while i < items.len() {
                let item = &items[i];

                if let Value::Object(obj) = item {
                    let typ = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    match typ {
                        // -- input_image -> user message with image_url --
                        "input_image" => {
                            let detail =
                                obj.get("detail").and_then(|v| v.as_str()).unwrap_or("auto");

                            // Process image_url (URL or data URL)
                            if let Some(image_url) = obj.get("image_url").and_then(|v| v.as_str()) {
                                if !image_url.is_empty() {
                                    msgs.push(json!({
                                        "role": "user",
                                        "content": [{
                                            "type": "image_url",
                                            "image_url": { "url": image_url, "detail": detail }
                                        }]
                                    }));
                                }
                            }
                            // Process image_data (base64) -> convert to data URL
                            else if let Some(image_data) =
                                obj.get("image_data").and_then(|v| v.as_str())
                            {
                                if !image_data.is_empty() {
                                    // Default to PNG if media type unspecified
                                    let data_url = if image_data.starts_with("data:") {
                                        image_data.to_string()
                                    } else {
                                        format!("data:image/png;base64,{}", image_data)
                                    };
                                    msgs.push(json!({
                                        "role": "user",
                                        "content": [{
                                            "type": "image_url",
                                            "image_url": { "url": data_url, "detail": detail }
                                        }]
                                    }));
                                }
                            }
                            i += 1;
                            continue;
                        }

                        // -- input_file -> direct passthrough --
                        "input_file" => {
                            // Direct passthrough - let upstream decide handling
                            msgs.push(json!({
                                "role": "user",
                                "content": obj.clone()
                            }));
                            i += 1;
                            continue;
                        }

                        // -- function_call -> assistant message with tool_calls --
                        "function_call" => {
                            let call_id = obj.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let arguments = obj
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");

                            // Collect tool calls for this assistant turn
                            let mut tool_calls = vec![json!({
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": arguments,
                                }
                            })];

                            // If next item is also function_call (same turn), group them
                            let mut j = i + 1;
                            while j < items.len() {
                                if let Value::Object(next_obj) = &items[j] {
                                    let next_typ =
                                        next_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                    if next_typ == "function_call" {
                                        let next_call_id = next_obj
                                            .get("call_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let next_name = next_obj
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let next_args = next_obj
                                            .get("arguments")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("{}");
                                        tool_calls.push(json!({
                                            "id": next_call_id,
                                            "type": "function",
                                            "function": {
                                                "name": next_name,
                                                "arguments": next_args,
                                            }
                                        }));
                                        j += 1;
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }

                            msgs.push(json!({
                                "role": "assistant",
                                "content": null,
                                "tool_calls": tool_calls
                            }));
                            i = j;
                            continue;
                        }

                        // -- function_call_output -> tool message --
                        "function_call_output" => {
                            let call_id = obj.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                            let output = match obj.get("output") {
                                Some(Value::String(s)) => s.clone(),
                                Some(v) => {
                                    serde_json::to_string(v).unwrap_or_else(|_| String::new())
                                }
                                None => String::new(),
                            };

                            msgs.push(json!({
                                "role": "tool",
                                "tool_call_id": call_id,
                                "content": output,
                            }));
                            i += 1;
                            continue;
                        }

                        // -- reasoning skip (chain of thought metadata, not dialogue message) --
                        "reasoning" => {
                            i += 1;
                            continue;
                        }

                        // -- Regular message --
                        _ => {
                            let role = match obj.get("role") {
                                Some(Value::String(r)) => match r.as_str() {
                                    "system" | "developer" => "system".to_string(),
                                    "user" | "assistant" | "tool" => r.clone(),
                                    _ => {
                                        if matches!(typ, "message") {
                                            "assistant".to_string()
                                        } else {
                                            "user".to_string()
                                        }
                                    }
                                },
                                _ => {
                                    if matches!(typ, "message") {
                                        "assistant".to_string()
                                    } else {
                                        "user".to_string()
                                    }
                                }
                            };

                            let content_value = match obj.get("content") {
                                Some(Value::String(s)) => {
                                    if s.is_empty() {
                                        None
                                    } else {
                                        Some(json!(s))
                                    }
                                }
                                Some(Value::Array(parts)) => {
                                    let mut texts: Vec<String> = Vec::new();
                                    let mut image_parts: Vec<Value> = Vec::new();
                                    let mut raw_parts: Vec<Value> = Vec::new();

                                    for p in parts {
                                        match p {
                                            Value::String(s) => texts.push(s.clone()),
                                            Value::Object(o) => {
                                                let part_type = o
                                                    .get("type")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("");
                                                if part_type == "input_image" {
                                                    let image_url = o
                                                        .get("image_url")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("");
                                                    let detail = o
                                                        .get("detail")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("auto");
                                                    if !image_url.is_empty() {
                                                        image_parts.push(json!({
                                                            "type": "image_url",
                                                            "image_url": {
                                                                "url": image_url,
                                                                "detail": detail
                                                            }
                                                        }));
                                                    } else {
                                                        raw_parts.push(p.clone());
                                                    }
                                                } else {
                                                    let t = o
                                                        .get("text")
                                                        .or_else(|| o.get("input_text"))
                                                        .or_else(|| o.get("output_text"))
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("");
                                                    if !t.is_empty() {
                                                        texts.push(t.to_string());
                                                    } else {
                                                        raw_parts.push(p.clone());
                                                    }
                                                }
                                            }
                                            _ => raw_parts.push(p.clone()),
                                        }
                                    }

                                    if image_parts.is_empty() && raw_parts.is_empty() {
                                        // No structured part - join text into plain string (backward compatible)
                                        let joined = texts.join("\n");
                                        if joined.is_empty() {
                                            None
                                        } else {
                                            Some(json!(joined))
                                        }
                                    } else {
                                        // Has image or unknown parts - build structured content array
                                        let mut content_parts: Vec<Value> = texts
                                            .iter()
                                            .map(|t| json!({"type": "text", "text": t}))
                                            .collect();
                                        content_parts.extend(image_parts);
                                        content_parts.extend(raw_parts);
                                        if content_parts.is_empty() {
                                            None
                                        } else {
                                            Some(json!(content_parts))
                                        }
                                    }
                                }
                                _ => None,
                            };

                            if let Some(content) = content_value {
                                msgs.push(json!({ "role": role, "content": content }));
                            } else if matches!(typ, "function_call" | "function_call_output") {
                                // Already handled above; skip empty message fallback
                            } else if !typ.is_empty() {
                                // Preserve unknown structured Responses input items instead of dropping
                                // or stringifying; upstream can determine support.
                                msgs.push(json!({ "role": role, "content": obj.clone() }));
                            }

                            i += 1;
                        }
                    }
                } else if let Value::String(s) = item {
                    msgs.push(json!({ "role": "user", "content": s }));
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
        other => {
            // Return empty content for null or other types without autofilling
            if other.is_null() {
                // This should be caught in handler validation;
                // if reached, return empty message for caller handling
                return msgs;
            }
            let text = serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string());
            if !text.is_empty() {
                msgs.push(json!({ "role": "user", "content": text }));
            }
        }
    }

    // Only add default user message when actual input content exists
    // (null/missing input should be rejected at handler level)
    if msgs.is_empty() && instructions.is_none() {
        // No content or instructions - should be caught in validation
        // Return empty for caller handling
        return msgs;
    }

    msgs
}

/// Convert Responses API tool definition to Chat Completions format.
///
/// Responses API: `{ type: "function", name, description, parameters, strict }`
/// Chat API:      `{ type: "function", function: { name, description, parameters, strict } }`
///
/// Temporary strategy: Responses -> Chat fallback path,
/// skipping non-function tool types to prevent Chat upstream schema errors.
/// Responses-specific tools (custom, web_search, file_search, image_generation)
/// only retain function tools that map safely; other types are skipped with warnings.
/// Capability router will re-evaluate upstream capacity for these tools.
pub fn convert_tools(tools: &[Value]) -> Option<Value> {
    let converted: Vec<Value> = tools
        .iter()
        .filter_map(|t| {
            let typ = t.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // Direct passthrough if already in Chat format
            if typ == "function" && t.get("function").is_some() {
                return Some(t.clone());
            }

            // Convert Responses format function tool to Chat format,
            // while preserving unknown top-level fields for compatibility.
            if typ == "function" {
                let mut tool = t.clone();
                let Some(tool_obj) = tool.as_object_mut() else {
                    return Some(t.clone());
                };

                let mut function = serde_json::Map::new();
                if let Some(name) = tool_obj.remove("name") {
                    function.insert("name".to_string(), name);
                } else {
                    function.insert("name".to_string(), json!("tool"));
                }
                if let Some(description) = tool_obj.remove("description") {
                    function.insert("description".to_string(), description);
                }
                if let Some(parameters) = tool_obj.remove("parameters") {
                    function.insert("parameters".to_string(), parameters);
                } else {
                    function.insert(
                        "parameters".to_string(),
                        json!({ "type": "object", "properties": {} }),
                    );
                }
                if let Some(strict) = tool_obj.remove("strict") {
                    function.insert("strict".to_string(), strict);
                }

                tool_obj.insert("function".to_string(), Value::Object(function));
                return Some(tool);
            }

            // Chat Completions upstream generally does not accept Responses-specific tool types.
            // Continuing passthrough causes upstream errors like `function is not set`.
            // Skip temporarily to avoid error pollution.
            log::warn!(
                "Responses to Chat temporarily skipping unsupported tool type: {}",
                if typ.is_empty() { "<missing>" } else { typ }
            );
            None
        })
        .collect();

    if converted.is_empty() {
        None
    } else {
        Some(Value::Array(converted))
    }
}

/// Determine whether item in tool_calls is a function tool call
pub fn is_function_tool_call(tc: &Value) -> bool {
    tc.get("function").is_some()
        && (tc.get("type").and_then(|v| v.as_str()) == Some("function") || tc.get("type").is_none())
}

/// Convert item in tool_calls to Responses API output item format
pub fn passthrough_output_item(tc: &Value, status: Option<&str>) -> Value {
    let mut item = tc.clone();
    if let Some(obj) = item.as_object_mut() {
        obj.remove("index");
        if !obj.contains_key("type") {
            obj.insert("type".to_string(), json!("tool_call"));
        }
        if let Some(status) = status {
            obj.insert("status".to_string(), json!(status));
        }
    }
    item
}

/// Merge incremental updates of tool_calls (for streaming responses)
pub fn merge_tool_delta(item: &mut Value, delta: &Value) {
    if let (Some(item_obj), Some(delta_obj)) = (item.as_object_mut(), delta.as_object()) {
        for (key, value) in delta_obj {
            if key == "index" {
                continue;
            }
            if key == "function" {
                match (item_obj.get_mut("function"), value) {
                    (Some(Value::Object(existing)), Value::Object(delta_fn)) => {
                        for (fn_key, fn_value) in delta_fn {
                            if fn_key == "arguments" {
                                let existing_args = existing
                                    .get("arguments")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let delta_args = match fn_value {
                                    Value::String(s) => s.clone(),
                                    Value::Object(_) | Value::Array(_) => {
                                        serde_json::to_string(fn_value)
                                            .unwrap_or_else(|_| String::new())
                                    }
                                    _ => String::new(),
                                };
                                if !delta_args.is_empty() {
                                    existing.insert(
                                        "arguments".to_string(),
                                        json!(format!("{}{}", existing_args, delta_args)),
                                    );
                                }
                            } else if !fn_value.is_null() {
                                existing.insert(fn_key.clone(), fn_value.clone());
                            }
                        }
                    }
                    _ => {
                        item_obj.insert(key.clone(), value.clone());
                    }
                }
            } else if !value.is_null() {
                item_obj.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod reasoning_merge_tests {
    use super::*;

    #[test]
    fn transform_request_to_responses_merges_reasoning_effort_into_existing_reasoning() {
        let mut body = json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning": {"effort": "low", "summary": "auto"},
            "reasoning_effort": "high"
        });

        transform_request_to_responses(&mut body, "gpt-test");

        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn transform_request_to_responses_keeps_native_reasoning_without_flat_effort() {
        let mut body = json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning": {"effort": "medium", "summary": "auto"}
        });

        transform_request_to_responses(&mut body, "gpt-test");

        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn transform_request_to_responses_keeps_native_responses_request_lossless() {
        let mut body = json!({
            "model": "auto",
            "input": "hi",
            "reasoning": {"effort": "high", "summary": "detailed"},
            "include": ["reasoning.encrypted_content"],
            "metadata": {"trace": "keep"}
        });

        transform_request_to_responses(&mut body, "gpt-responses");

        assert_eq!(body["model"], "gpt-responses");
        assert_eq!(body["input"], "hi");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "detailed");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["metadata"]["trace"], "keep");
    }
}
