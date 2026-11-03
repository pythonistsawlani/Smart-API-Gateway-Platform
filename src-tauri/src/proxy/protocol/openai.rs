use super::{join_url, ProtocolAdapter};
/// OpenAI protocol adapter.
///
/// Handles all openai-type channels — both standard OpenAI endpoints
/// (which expect /v1/chat/completions) and custom endpoints with a
/// user-provided API path. When the base URL already contains a path
/// after the host, that path is treated as the authoritative API root.
use serde_json::Value;
pub struct OpenAiAdapter;

/// Returns `true` when the base URL already carries a path after the
/// host:port. Such URLs are treated as authoritative API roots — the
/// caller should append the endpoint directly without forcing a /v1/
/// prefix.
fn has_custom_api_path(base_url: &str) -> bool {
    let base = base_url.trim_end_matches('/');
    if let Some(scheme_end) = base.find("://") {
        let after_scheme = &base[scheme_end + 3..];
        if let Some(slash_pos) = after_scheme.find('/') {
            return !after_scheme[slash_pos + 1..].is_empty();
        }
    }
    false
}

// --- Blacklist constants + builders: see OPENAI_FOREIGN_DROP below ---

/// Known foreign protocol proprietary fields that OpenAI Chat Completions does not accept,
/// belonging to Anthropic / Gemini native / OpenAI Responses, which must be dropped on egress,
/// avoiding leakage to OpenAI upstream.
///
/// Blacklist decision: egress uses blacklist rather than whitelist -
/// preserving unknown/future fields while dropping known foreign fields
/// (whitelists inadvertently drop new valid protocol fields).
const OPENAI_FOREIGN_DROP: &[&str] = &[
    // OpenAI Responses API proprietary
    "input",
    "instructions",
    "include",
    "prompt",
    "max_output_tokens",
    "text",
    "truncation",
    "previous_response_id",
    "max_tool_calls",
    // Anthropic proprietary
    "anthropic_version",
    "anthropic_beta",
    "betas",
    "system",
    "max_tokens_to_sample",
    "top_k",
    "thinking",
    "context_management",
    "mcp_servers",
    "container",
    // Gemini native proprietary
    "contents",
    "generationConfig",
    "safetySettings",
    "systemInstruction",
    "cachedContent",
    // Internal scratch fields
    "__as_raw_claude_req",
    "__as_raw_responses_req",
    "__as_raw_gemini_req",
];

// --- Blacklist builder functions -------------------------------------

/// Build OpenAI request output object from intermediate protocol (blacklist: preserve all, drop foreign fields)
fn build_openai_request_output(src: &serde_json::Map<String, Value>, actual_model: &str) -> Value {
    let mut out = src.clone();
    for key in OPENAI_FOREIGN_DROP {
        out.remove(*key);
    }
    out.insert("model".to_string(), Value::String(actual_model.to_string()));
    Value::Object(out)
}

/// Build OpenAI response output object from intermediate protocol (blacklist: preserve all, drop foreign fields)
fn build_openai_response_output(src: &serde_json::Map<String, Value>) -> Value {
    let mut out = src.clone();
    for key in OPENAI_RESPONSE_FOREIGN_DROP {
        out.remove(*key);
    }
    Value::Object(out)
}

/// Foreign protocol proprietary fields in response direction (Gemini/Responses/Anthropic response structures)
const OPENAI_RESPONSE_FOREIGN_DROP: &[&str] = &[
    // OpenAI Responses API response proprietary
    "output",
    "output_text",
    "instructions",
    // Gemini native response proprietary
    "candidates",
    "usageMetadata",
    "promptFeedback",
    "modelVersion",
    // Anthropic response proprietary
    "stop_reason",
    "stop_sequence",
    // Internal scratch fields
    "__as_raw_claude_req",
    "__as_raw_responses_req",
    "__as_raw_gemini_req",
];

impl ProtocolAdapter for OpenAiAdapter {
    fn build_chat_url(&self, base_url: &str, _model: &str) -> String {
        if has_custom_api_path(base_url) {
            let base = base_url.trim_end_matches('/');
            format!("{}/chat/completions", base)
        } else {
            join_url(base_url, "v1/chat/completions")
        }
    }

    fn build_models_url(&self, base_url: &str, _api_key: &str) -> String {
        if has_custom_api_path(base_url) {
            let base = base_url.trim_end_matches('/');
            format!("{}/models", base)
        } else {
            join_url(base_url, "v1/models")
        }
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
        builder.bearer_auth(api_key)
    }

    fn transform_request(&self, body: &mut Value, actual_model: &str) {
        // Whitelist build: preserve OpenAI standard fields + extension fields, discard rest
        let Some(src) = body.as_object() else {
            return;
        };
        *body = build_openai_request_output(src, actual_model);
    }

    fn transform_response(&self, body: &mut Value) {
        // Whitelist build: preserve OpenAI standard fields + extension fields, discard rest
        let Some(src) = body.as_object() else {
            return;
        };
        *body = build_openai_response_output(src);
    }

    fn needs_sse_transform(&self) -> bool {
        false
    }

    fn extract_sse_usage(&self, data_line: &str) -> (i64, i64) {
        if data_line == "[DONE]" {
            return (0, 0);
        }
        let Ok(value) = serde_json::from_str::<Value>(data_line) else {
            return (0, 0);
        };
        let prompt = value
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let completion = value
            .get("usage")
            .and_then(|u| u.get("completion_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        (prompt, completion)
    }

    fn transform_sse_line(&self, data_line: &str) -> Option<String> {
        // Should never be called (needs_sse_transform = false).
        Some(data_line.to_string())
    }

    fn parse_models_response(&self, body: &Value) -> Vec<(String, Option<String>)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transform_request_preserves_openai_compatible_reasoning_extensions() {
        let adapter = OpenAiAdapter;
        let mut body = json!({
            "model": "auto",
            "reasoning_effort": "high",
            "thinking": {"type": "enabled", "budget_tokens": 4096},
            "messages": [{
                "role": "assistant",
                "content": "",
                "reasoning_content": "kept reasoning",
                "provider_specific": {"thinking": "kept provider thinking"}
            }]
        });

        adapter.transform_request(&mut body, "resolved-model");

        assert_eq!(body["model"], "resolved-model");
        assert_eq!(body["reasoning_effort"], "high");
        // Rule: top-level thinking is Anthropic name, not OpenAI standard/extension field,
        // egress should drop it (preserving reasoning_effort/reasoning_content/provider_specific).
        assert!(body.get("thinking").is_none());
        assert_eq!(body["messages"][0]["reasoning_content"], "kept reasoning");
        assert_eq!(
            body["messages"][0]["provider_specific"]["thinking"],
            "kept provider thinking"
        );
    }

    #[test]
    fn transform_request_drops_responses_only_fields() {
        let adapter = OpenAiAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],

            "input": "hi",
            "instructions": "be brief",
            "include": ["reasoning.encrypted_content"],
            "prompt": {"id": "pmpt_123"},
            "max_output_tokens": 100,
            "text": {"format": {"type": "text"}},
            "truncation": "auto",
            "previous_response_id": "resp_123",
            "max_tool_calls": 3,

            "temperature": 0.3,
            "metadata": {"k": "v"},
            "store": true,
            "prompt_cache_key": "cache-key",
            "safety_identifier": "safe-user",
            "reasoning_effort": "medium",
            "thinking": {"type": "enabled"}
        });

        adapter.transform_request(&mut body, "gpt-4o");

        assert_eq!(body["model"], "gpt-4o");

        assert!(body.get("input").is_none());
        assert!(body.get("instructions").is_none());
        assert!(body.get("include").is_none());
        assert!(body.get("prompt").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("text").is_none());
        assert!(body.get("truncation").is_none());
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("max_tool_calls").is_none());

        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["temperature"], 0.3);
        // metadata is standard field and should be preserved
        assert_eq!(body["metadata"]["k"], "v");
        assert_eq!(body["store"], true);
        assert_eq!(body["prompt_cache_key"], "cache-key");
        assert_eq!(body["safety_identifier"], "safe-user");
        assert_eq!(body["reasoning_effort"], "medium");
        // Top-level thinking (Anthropic name) dropped on egress; reasoning_effort preserved
        assert!(body.get("thinking").is_none());
    }

    /// Blacklist decision: unknown/future fields must passthrough.
    #[test]
    fn transform_request_passes_through_unknown_fields() {
        let adapter = OpenAiAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "x_future_openai_field": {"nested": "value"},
            "some_new_param": 42
        });

        adapter.transform_request(&mut body, "gpt-4o");

        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["x_future_openai_field"]["nested"], "value");
        assert_eq!(body["some_new_param"], 42);
    }

    #[test]
    fn transform_response_preserves_openai_compatible_reasoning_extensions() {
        let adapter = OpenAiAdapter;
        let mut body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "visible",
                    "reasoning_content": "kept reasoning"
                }
            }]
        });
        let original = body.clone();

        adapter.transform_response(&mut body);

        assert_eq!(body, original);
    }

    #[test]
    fn transform_response_drops_foreign_protocol_fields() {
        let adapter = OpenAiAdapter;
        let mut body = json!({
            "id": "chatcmpl_1",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "system_fingerprint": "fp_123",
            "reasoning_text": "kept extension",

            "output": [],
            "output_text": "wrong protocol",
            "candidates": [],
            "usageMetadata": {},
            "stop_reason": "end_turn",
            "instructions": "wrong protocol"
        });

        adapter.transform_response(&mut body);

        assert!(body.get("output").is_none());
        assert!(body.get("output_text").is_none());
        assert!(body.get("candidates").is_none());
        assert!(body.get("usageMetadata").is_none());
        assert!(body.get("stop_reason").is_none());
        assert!(body.get("instructions").is_none());

        assert_eq!(body["object"], "chat.completion");
        assert!(body.get("choices").is_some());
        assert_eq!(body["reasoning_text"], "kept extension");
    }
}
