use super::ProtocolAdapter;
/// Azure OpenAI protocol adapter.
///
/// Azure Chat Completions API is OpenAI-protocol compatible — the request/response
/// body format is identical to OpenAI. The differences are:
///
/// - **URL**: `https://<resource>.openai.azure.com/openai/deployments/<deployment>/chat/completions?api-version=2024-02-01`
/// - **Auth**: `api-key` header (not `Bearer` token)
/// - **Model**: The `model` field in the request body is *ignored* by Azure;
///   the deployment name is in the URL path.
/// - **Response**: Uses OpenAI-compatible format natively. SSE streaming is also
///   OpenAI-compatible (no transformation needed).
///
/// NOTE: `api-version` is configurable. We use `2024-02-01` as the default
/// because it is widely supported. Newer versions add features but this one
/// covers chat completions + tools + streaming.
use serde_json::{json, Value};

/// Default API version for Azure OpenAI.
const AZURE_API_VERSION: &str = "2024-02-01";

// --- Blacklist constants + builders -----------------------------------

/// Fields to drop for Azure request egress.
///
/// Blacklist decision: preserve unknown/future
/// fields, only dropping foreign protocol proprietary fields (Anthropic / Gemini native / OpenAI Responses),
/// plus `model` (Azure passes model via deployment in URL, body carries no model).
/// Azure is OpenAI-compatible and supports logit_bias / service_tier / store, so these are retained.
const AZURE_REQUEST_FOREIGN_DROP: &[&str] = &[
    // Azure specific: model in URL, not body
    "model",
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

/// Foreign protocol proprietary fields to drop for Azure response egress
const AZURE_RESPONSE_FOREIGN_DROP: &[&str] = &[
    "output",
    "output_text",
    "instructions",
    "candidates",
    "usageMetadata",
    "promptFeedback",
    "modelVersion",
    "stop_reason",
    "stop_sequence",
    "__as_raw_claude_req",
    "__as_raw_responses_req",
    "__as_raw_gemini_req",
];

/// Build Azure request output object from intermediate protocol (blacklist: preserve all, drop foreign fields and model)
fn build_azure_request_output(src: &serde_json::Map<String, Value>) -> Value {
    let mut out = src.clone();
    for key in AZURE_REQUEST_FOREIGN_DROP {
        out.remove(*key);
    }
    Value::Object(out)
}

/// Build Azure response output object from intermediate protocol (blacklist: preserve all, drop foreign fields)
fn build_azure_response_output(src: &serde_json::Map<String, Value>) -> Value {
    let mut out = src.clone();
    for key in AZURE_RESPONSE_FOREIGN_DROP {
        out.remove(*key);
    }
    Value::Object(out)
}

pub struct AzureAdapter;

impl ProtocolAdapter for AzureAdapter {
    fn build_chat_url(&self, base_url: &str, model: &str) -> String {
        // Azure format:
        //   {base_url}/openai/deployments/{deployment}/chat/completions?api-version=...
        // `model` from the API entry is used as the deployment name.
        let base = base_url.trim_end_matches('/');
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            base, model, AZURE_API_VERSION
        )
    }

    fn build_models_url(&self, base_url: &str, _api_key: &str) -> String {
        let base = base_url.trim_end_matches('/');
        format!(
            "{}/openai/deployments?api-version={}",
            base, AZURE_API_VERSION
        )
    }

    fn uses_query_auth(&self) -> bool {
        false
    }

    fn build_auth_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![("api-key".to_string(), api_key.to_string())]
    }

    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        builder.header("api-key", api_key)
    }

    fn transform_request(&self, body: &mut Value, _actual_model: &str) {
        // Whitelist build: only preserve Azure standard fields + extension fields (excluding model, which Azure passes via URL)
        let Some(src) = body.as_object() else {
            return;
        };
        *body = build_azure_request_output(src);
    }

    fn transform_response(&self, body: &mut Value) {
        // Whitelist build: only preserve Azure standard fields + extension fields
        let Some(src) = body.as_object() else {
            return;
        };
        *body = build_azure_response_output(src);
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
        // Azure returns: { data: [{ id: "deployment-name", model: "gpt-4o", ... }] }
        // The `id` is the deployment name, `model` is the underlying model.
        // We return deployment name as the id, model as the display name.
        body.get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str()?.to_string();
                        // Use "model" field as display name if available
                        let owned_by = m
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .or_else(|| {
                                m.get("owned_by").and_then(|v| v.as_str()).map(String::from)
                            });
                        Some((id, owned_by))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Public API: Azure OpenAI -> OpenAI request conversion
// ═══════════════════════════════════════════════════════════════════

/// Convert Azure OpenAI request format to standard OpenAI format.
///
/// Azure and OpenAI request formats are identical except Azure puts the
/// deployment name (model) in the URL path and ignores `model` in the body.
/// This function takes an Azure request and returns an OpenAI-compatible request,
/// ensuring the `model` field is set from the deployment name if not present.
///
/// - model: set from deployment name if missing in body
/// - messages, max_tokens, temperature, etc.: passthrough as-is
/// - tools, tool_choice, stream, stop: passthrough as-is
pub fn azure_to_openai_request(azure: &Value, deployment: &str) -> Value {
    let mut openai = azure.clone();

    // Azure puts model in URL path, not body. Ensure model field exists for OpenAI.
    if openai.get("model").is_none() {
        openai["model"] = json!(deployment);
    }

    openai
}

// ═══════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── azure_to_openai_request tests ────────────────────────────

    #[test]
    fn basic_azure_to_openai() {
        let azure = json!({
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "temperature": 0.7
        });

        let openai = azure_to_openai_request(&azure, "my-gpt4-deployment");

        // model should be injected from deployment name
        assert_eq!(openai["model"], "my-gpt4-deployment");
        assert_eq!(openai["messages"][0]["role"], "user");
        assert_eq!(openai["messages"][0]["content"], "Hello");
        assert_eq!(openai["max_tokens"], 100);
        assert_eq!(openai["temperature"], 0.7);
    }

    #[test]
    fn azure_to_openai_preserves_existing_model() {
        let azure = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hi"}
            ]
        });

        let openai = azure_to_openai_request(&azure, "my-deployment");

        // Existing model in body should be kept (not overwritten)
        assert_eq!(openai["model"], "gpt-4");
    }

    #[test]
    fn azure_to_openai_passthrough_all_fields() {
        let azure = json!({
            "messages": [{"role": "user", "content": "test"}],
            "max_tokens": 512,
            "temperature": 0.9,
            "top_p": 0.95,
            "stream": true,
            "stop": ["END"],
            "tools": [{"type": "function", "function": {"name": "search"}}],
            "tool_choice": "auto"
        });

        let openai = azure_to_openai_request(&azure, "deployment-1");

        assert_eq!(openai["max_tokens"], 512);
        assert_eq!(openai["temperature"], 0.9);
        assert_eq!(openai["top_p"], 0.95);
        assert_eq!(openai["stream"], true);
        assert_eq!(openai["stop"], json!(["END"]));
        assert_eq!(openai["tools"][0]["function"]["name"], "search");
        assert_eq!(openai["tool_choice"], "auto");
    }

    #[test]
    fn azure_to_openai_empty_body() {
        let azure = json!({});
        let openai = azure_to_openai_request(&azure, "gpt-4-deploy");
        assert_eq!(openai["model"], "gpt-4-deploy");
    }

    #[test]
    fn azure_transform_request_drops_foreign_protocol_fields() {
        let adapter = AzureAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "input": "wrong",
            "instructions": "wrong",
            "include": ["wrong"],
            "prompt": {"id": "pmpt_1"},
            "max_output_tokens": 10,
            "text": {"format": {"type": "text"}},
            "truncation": "auto",
            "previous_response_id": "resp_1",
            "max_tool_calls": 2
        });

        adapter.transform_request(&mut body, "deployment-name");

        assert!(body.get("model").is_none());
        assert!(body.get("input").is_none());
        assert!(body.get("instructions").is_none());
        assert!(body.get("include").is_none());
        assert!(body.get("prompt").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("text").is_none());
        assert!(body.get("truncation").is_none());
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("max_tool_calls").is_none());

        assert!(body.get("messages").is_some());
        assert_eq!(body["temperature"], 0.2);
    }

    /// Blacklist decision: Azure builder should passthrough unknown/future fields, only dropping foreign protocol fields and model.
    /// Azure is OpenAI-compatible; standard fields like logit_bias/service_tier/store should be preserved.
    #[test]
    fn azure_transform_request_passes_through_unknown_keeps_standard() {
        let adapter = AzureAdapter;
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "logit_bias": {"1": 1},
            "service_tier": "auto",
            "store": true,
            "x_future_azure_field": {"nested": "value"},
            "anthropic_version": "2023-06-01"
        });

        adapter.transform_request(&mut body, "deployment-name");

        // model via URL, not in body
        assert!(body.get("model").is_none());
        // Preserve Azure standard fields
        assert_eq!(body["logit_bias"]["1"], 1);
        assert_eq!(body["service_tier"], "auto");
        assert_eq!(body["store"], true);
        // Passthrough unknown fields
        assert_eq!(body["x_future_azure_field"]["nested"], "value");
        // Drop foreign protocol fields
        assert!(body.get("anthropic_version").is_none());
    }

    #[test]
    fn azure_transform_response_drops_foreign_protocol_fields() {
        let adapter = AzureAdapter;
        let mut body = json!({
            "id": "chatcmpl_1",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "output": [],
            "usageMetadata": {},
            "instructions": "wrong protocol"
        });

        adapter.transform_response(&mut body);

        assert!(body.get("output").is_none());
        assert!(body.get("usageMetadata").is_none());
        assert!(body.get("instructions").is_none());
        assert!(body.get("choices").is_some());
    }
}
