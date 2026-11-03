//! Protocol translator round-trip test suite
//!
//! Based on the two axioms in REFACTOR_PLAN.md:
//! 1. We are a proxy and translator, not a content modifier.
//! 2. Protocols adhere to official documentation: input must match output semantics.
//!
//! These tests verify expected behavior according to the axioms.
//! Test status:
//!   - Passing = conforms to axioms, refactoring must not break
//!   - Target = planned behavior for subsequent phases
//!
//! Tests verify architecture consistency across phases.

use serde_json::{json, Value};

/// JSON equality assertion, outputting structured diff on failure
fn assert_json_eq(actual: &Value, expected: &Value, context: &str) {
    if actual != expected {
        panic!(
            "{context}\n─── expected ───\n{}\n─── actual ───\n{}\n",
            serde_json::to_string_pretty(expected).unwrap_or_default(),
            serde_json::to_string_pretty(actual).unwrap_or_default(),
        );
    }
}

/// Remove specified top-level fields from JSON object to ignore expected changes during comparison
fn without_fields(mut value: Value, fields: &[&str]) -> Value {
    if let Some(obj) = value.as_object_mut() {
        for field in fields {
            obj.remove(*field);
        }
    }
    value
}

// ═══════════════════════════════════════════════════════════════════
// Claude protocol round-trip tests
//
// Verification pipelines:
//    Downstream: Claude request A -> OpenAI intermediate format B -> Claude request A'
//             Satisfies A == A'
//    Upstream: Claude response X -> OpenAI intermediate format Y -> Claude response X'
//             Satisfies X == X'
// ═══════════════════════════════════════════════════════════════════

mod claude_roundtrip {
    use super::*;
    use crate::proxy::protocol::{
        claude::ClaudeAdapter, claude_to_openai_request, openai_to_claude_response,
        ClaudeSSETransformer, ProtocolAdapter,
    };

    const TEST_MODEL: &str = "claude-3-sonnet-20240229";

    // --- Request direction: downstream ingress -> upstream adapter round-trip ---

    /// Basic scenario: single-turn dialogue, text messages
    #[test]
    fn request_basic_text_message() {
        let claude_original = json!({
            "model": TEST_MODEL,
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1024,
            "stream": false
        });

        // Downstream ingress translation: Claude -> OpenAI
        let openai_intermediate = claude_to_openai_request(&claude_original);

        // Upstream adapter translation: OpenAI -> Claude
        let adapter = ClaudeAdapter;
        let mut back_to_claude = openai_intermediate.clone();
        adapter.transform_request(&mut back_to_claude, TEST_MODEL);

        assert_eq!(back_to_claude["model"], json!(TEST_MODEL));
        assert_eq!(back_to_claude["messages"][0]["role"], "user");
        assert_eq!(back_to_claude["messages"][0]["content"], "Hello");
        assert_eq!(back_to_claude["max_tokens"], 1024);
    }

    /// system message: Claude top-level system -> OpenAI messages[0] role=system -> Claude top-level system
    #[test]
    fn request_system_message_roundtrip() {
        let claude_original = json!({
            "model": TEST_MODEL,
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "Hi"}
            ],
            "max_tokens": 1024
        });

        let openai_intermediate = claude_to_openai_request(&claude_original);

        // Verify intermediate format: system becomes first message
        assert_eq!(openai_intermediate["messages"][0]["role"], "system");
        assert_eq!(
            openai_intermediate["messages"][0]["content"],
            "You are helpful."
        );

        let adapter = ClaudeAdapter;
        let mut back_to_claude = openai_intermediate.clone();
        adapter.transform_request(&mut back_to_claude, TEST_MODEL);

        // Verify restoration: system returns to top level
        // Note: Current adapter implementation stores system as array (Claude 4.5+ compatible),
        // which is permitted by official documentation
        let system = &back_to_claude["system"];
        let system_text = match system {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        assert_eq!(system_text, "You are helpful.");
        assert_eq!(back_to_claude["messages"][0]["role"], "user");
    }

    /// tools definition: Claude input_schema <-> OpenAI function.parameters
    #[test]
    fn request_tools_roundtrip() {
        let claude_original = json!({
            "model": TEST_MODEL,
            "messages": [{"role": "user", "content": "What's the weather?"}],
            "max_tokens": 1024,
            "tools": [{
                "name": "get_weather",
                "description": "Get weather for a city",
                "input_schema": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            }]
        });

        let openai_intermediate = claude_to_openai_request(&claude_original);

        // Verify intermediate format: tools in function form
        assert_eq!(openai_intermediate["tools"][0]["type"], "function");
        assert_eq!(
            openai_intermediate["tools"][0]["function"]["name"],
            "get_weather"
        );

        let adapter = ClaudeAdapter;
        let mut back_to_claude = openai_intermediate.clone();
        adapter.transform_request(&mut back_to_claude, TEST_MODEL);

        // Verify restoration: tools back in Claude form
        let tool = &back_to_claude["tools"][0];
        assert_eq!(tool["name"], "get_weather");
        assert_eq!(tool["description"], "Get weather for a city");
        assert_eq!(tool["input_schema"]["type"], "object");
        assert_eq!(tool["input_schema"]["properties"]["city"]["type"], "string");
    }

    /// tool_choice：Claude {type:"auto"} ↔ OpenAI "auto"
    #[test]
    fn request_tool_choice_auto_roundtrip() {
        let claude_original = json!({
            "model": TEST_MODEL,
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 1024,
            "tool_choice": {"type": "auto"}
        });

        let openai_intermediate = claude_to_openai_request(&claude_original);
        assert_eq!(openai_intermediate["tool_choice"], "auto");

        let adapter = ClaudeAdapter;
        let mut back_to_claude = openai_intermediate.clone();
        adapter.transform_request(&mut back_to_claude, TEST_MODEL);

        assert_eq!(back_to_claude["tool_choice"]["type"], "auto");
    }

    /// Core verification of Axiom 2: unknown fields passthrough
    /// Claude request with custom field not in official docs,
    /// must still exist after claude -> openai -> claude round-trip.
    #[test]
    fn request_unknown_field_passthrough_top_level() {
        let claude_original = json!({
            "model": TEST_MODEL,
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 1024,
            "x_api_switch_tracking_id": "abc-123",
            "x_future_anthropic_field": {"nested": "value"}
        });

        let openai_intermediate = claude_to_openai_request(&claude_original);

        // Custom field should remain in intermediate format
        assert_eq!(
            openai_intermediate["x_api_switch_tracking_id"], "abc-123",
            "claude_to_openai lost top-level custom field x_api_switch_tracking_id"
        );
        assert_eq!(
            openai_intermediate["x_future_anthropic_field"]["nested"], "value",
            "claude_to_openai lost top-level nested custom field"
        );

        let adapter = ClaudeAdapter;
        let mut back_to_claude = openai_intermediate.clone();
        adapter.transform_request(&mut back_to_claude, TEST_MODEL);

        // Blacklist mode:
        // Unknown/future fields passthrough, only dropping known OpenAI proprietary fields.
        // Preserves custom fields to ensure passthrough of new native fields.
        assert_eq!(
            back_to_claude
                .get("x_api_switch_tracking_id")
                .and_then(|v| v.as_str()),
            Some("abc-123"),
            "ClaudeAdapter blacklist mode should passthrough unknown field x_api_switch_tracking_id"
        );
        assert_eq!(
            back_to_claude["x_future_anthropic_field"]["nested"], "value",
            "ClaudeAdapter blacklist mode should passthrough unknown nested field x_future_anthropic_field"
        );
    }

    /// mid-conversation system（Opus 4.8 + Claude Code v2.1.154）：
    /// mid-conversation role:"system" in messages must be preserved in place
    /// without being hoisted to top-level system.
    /// Note: Claude->Claude direction is handled directly by forwarder passthrough
    /// without going through this function.
    #[test]
    fn request_mid_conversation_system_preserved_to_openai() {
        let claude = json!({
            "model": TEST_MODEL,
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "write code"},
                {"role": "assistant", "content": "ok"},
                {"role": "system", "content": "NEW RULE: from here, extra permissions"},
                {"role": "user", "content": "continue"}
            ]
        });

        let openai = claude_to_openai_request(&claude);
        let msgs = openai["messages"].as_array().expect("messages array");

        let sys_idx = msgs
            .iter()
            .position(|m| m["role"] == "system")
            .expect("mid-conversation system message should not be lost");
        assert_eq!(
            msgs[sys_idx]["content"], "NEW RULE: from here, extra permissions",
            "mid system content should be preserved"
        );
        // Position semantics: immediately follows assistant, followed by user turn
        assert_eq!(
            msgs[sys_idx - 1]["role"],
            "assistant",
            "mid system should immediately follow assistant"
        );
        assert!(
            msgs[sys_idx + 1..].iter().any(|m| m["role"] == "user"),
            "user turn should follow mid system"
        );
    }

    // --- Response direction: upstream adapter -> downstream ingress round-trip ---

    /// Basic response: upstream Claude response -> OpenAI intermediate -> downstream Claude response
    #[test]
    fn response_basic_text() {
        let claude_original = json!({
            "id": "msg_abc",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-sonnet-20240229",
            "content": [
                {"type": "text", "text": "Hello!"}
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        // Upstream adapter translation: Claude -> OpenAI
        let mut openai_intermediate = claude_original.clone();
        let adapter = ClaudeAdapter;
        adapter.transform_response(&mut openai_intermediate);

        // Downstream ingress translation: OpenAI -> Claude
        let back_to_claude = openai_to_claude_response(&openai_intermediate);

        assert_eq!(back_to_claude["id"], "msg_abc");
        assert_eq!(back_to_claude["type"], "message");
        assert_eq!(back_to_claude["role"], "assistant");
        // content must be in array format containing text blocks
        let content = back_to_claude["content"]
            .as_array()
            .expect("content should be array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello!");
        assert_eq!(back_to_claude["stop_reason"], "end_turn");
        assert_eq!(back_to_claude["usage"]["input_tokens"], 10);
        assert_eq!(back_to_claude["usage"]["output_tokens"], 5);
    }

    /// tool_use response
    #[test]
    fn response_tool_use() {
        let claude_original = json!({
            "id": "msg_tool",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-sonnet-20240229",
            "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Tokyo"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 10}
        });

        let mut openai_intermediate = claude_original.clone();
        let adapter = ClaudeAdapter;
        adapter.transform_response(&mut openai_intermediate);

        let back_to_claude = openai_to_claude_response(&openai_intermediate);

        assert_eq!(back_to_claude["stop_reason"], "tool_use");
        let content = back_to_claude["content"]
            .as_array()
            .expect("content should be array");
        let tool_use = content
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("tool_use block");
        assert_eq!(tool_use["id"], "toolu_1");
        assert_eq!(tool_use["name"], "get_weather");
        assert_eq!(tool_use["input"]["city"], "Tokyo");
    }

    /// Core verification of Axiom 2 #2: unknown fields passthrough in response direction
    /// Verifies passthrough in response direction:
    /// Ensures responses do not drop non-whitelisted fields.
    /// All non-whitelisted fields returned by upstream are preserved.
    /// Test target for phase 2.
    #[test]
    fn response_unknown_field_passthrough() {
        let claude_original = json!({
            "id": "msg_xyz",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-sonnet-20240229",
            "content": [{"type": "text", "text": "Hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3},
            "stop_sequence": null,
            "x_anthropic_future_field": "preserve_me",
            "container": {"id": "container_abc"}
        });

        let mut openai_intermediate = claude_original.clone();
        let adapter = ClaudeAdapter;
        adapter.transform_response(&mut openai_intermediate);

        let back_to_claude = openai_to_claude_response(&openai_intermediate);

        // Axiom 2: unknown fields must passthrough
        assert!(
            back_to_claude.get("x_anthropic_future_field").is_some(),
            "Response direction unknown field x_anthropic_future_field lost"
        );
        assert_eq!(
            back_to_claude["x_anthropic_future_field"], "preserve_me",
            "Response direction unknown field value was altered"
        );
        assert!(
            back_to_claude.get("container").is_some(),
            "Response direction official non-whitelist field container lost"
        );
    }

    // --- SSE streaming round-trip ----------------------------------

    /// Target behavior for P1:
    /// Upstream OpenAI streaming response usage-only frame (final frame, choices=[])
    /// should allow ClaudeSSETransformer to pass output_tokens to Claude client.
    ///
    /// ClaudeSSETransformer::transform_chunk handling for choices=[]:
    /// properly records usage to self.usage_output_tokens,
    /// ensuring message_delta carries accurate output_tokens.
    ///
    /// Phase 3 test target.
    #[test]
    fn sse_claude_usage_tokens_not_dropped() {
        let mut transformer =
            ClaudeSSETransformer::new("msg_test".to_string(), TEST_MODEL.to_string());

        // Standard OpenAI streaming frame sequence (stream_options.include_usage enabled):
        // 1. Initial frame: role=assistant
        let _ = transformer.transform_chunk(
            r#"{"id":"c1","choices":[{"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        );

        // 2. Text frame
        let _ = transformer.transform_chunk(
            r#"{"id":"c1","choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#,
        );

        // 3. finish_reason frame (message_delta emitted)
        let finish_events = transformer
            .transform_chunk(r#"{"id":"c1","choices":[{"delta":{},"finish_reason":"stop"}]}"#);

        // 4. usage-only frame (OpenAI spec allows choices=[] with usage only)
        let usage_events = transformer.transform_chunk(
            r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#,
        );

        // Assert: Claude client receives output_tokens=20
        // either in finish_events (known prior to usage-only frame),
        // or in usage_events (re-emitted message_delta on usage-only frame)
        let all_events: Vec<Value> = finish_events
            .iter()
            .chain(usage_events.iter())
            .filter_map(|e| serde_json::from_str::<Value>(e).ok())
            .collect();

        let has_correct_usage = all_events.iter().any(|v| {
            v["type"] == "message_delta"
                && v.get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_i64)
                    == Some(20)
        });

        assert!(
            has_correct_usage,
            "Claude client output_tokens is 0, real usage dropped\n\
             actual emitted events: {:#?}",
            all_events
        );
    }

    /// message_start event usage must also be retrievable
    #[test]
    fn sse_claude_input_tokens_preserved() {
        let mut transformer =
            ClaudeSSETransformer::new("msg_test".to_string(), TEST_MODEL.to_string());

        // Initial frame carries prompt_tokens
        let start_events = transformer.transform_chunk(
            r#"{"id":"c1","choices":[{"delta":{"role":"assistant"},"finish_reason":null}],"usage":{"prompt_tokens":15}}"#,
        );

        let found = start_events.iter().any(|e| {
            let v: Value = serde_json::from_str(e).unwrap_or(Value::Null);
            v["type"] == "message_start"
                && v["message"]["usage"]["input_tokens"].as_i64() == Some(15)
        });

        assert!(found, "message_start event did not carry input_tokens=15");
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helper function tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod helpers {
    use super::*;

    #[test]
    fn without_fields_removes_specified_keys() {
        let original = json!({"a": 1, "b": 2, "c": 3});
        let filtered = without_fields(original, &["b"]);
        assert_eq!(filtered, json!({"a": 1, "c": 3}));
    }
}

// ═══════════════════════════════════════════════════════════════════
// Gemini protocol round-trip tests
//
// Note: Gemini upstream adapter uses OpenAI compatible endpoint (v1beta/openai/),
// so upstream direction is mostly direct passthrough. Here we verify downstream:
// Gemini client -> OpenAI intermediate -> restore Gemini.
// ═══════════════════════════════════════════════════════════════════

mod gemini_roundtrip {
    use super::*;
    use crate::proxy::protocol::{gemini_to_openai_request, openai_to_gemini_response};

    /// Basic text request round-trip
    #[test]
    fn request_basic_text() {
        let gemini_original = json!({
            "model": "gemini-1.5-pro",
            "contents": [
                {"role": "user", "parts": [{"text": "Hello"}]}
            ],
            "generationConfig": {
                "temperature": 0.7,
                "maxOutputTokens": 512
            }
        });

        let openai_intermediate = gemini_to_openai_request(&gemini_original);

        // Verify translation to OpenAI format
        assert_eq!(openai_intermediate["model"], "gemini-1.5-pro");
        assert_eq!(openai_intermediate["messages"][0]["role"], "user");
        assert_eq!(openai_intermediate["messages"][0]["content"], "Hello");
        assert_eq!(openai_intermediate["temperature"], 0.7);
        assert_eq!(openai_intermediate["max_tokens"], 512);
    }

    /// Response direction basic round-trip
    #[test]
    fn response_basic_text() {
        let openai = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "gemini-1.5-pro",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hi there"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });

        let gemini = openai_to_gemini_response(&openai);

        assert_eq!(gemini["candidates"][0]["content"]["role"], "model");
        assert_eq!(
            gemini["candidates"][0]["content"]["parts"][0]["text"],
            "Hi there"
        );
        assert_eq!(gemini["candidates"][0]["finishReason"], "STOP");
        assert_eq!(gemini["usageMetadata"]["promptTokenCount"], 10);
        assert_eq!(gemini["usageMetadata"]["candidatesTokenCount"], 5);
    }

    /// Axiom 2: request direction unknown field passthrough
    /// Gemini `safetySettings`, `cachedContent`, etc. have no direct counterpart in OpenAI,
    /// and must be preserved via passthrough.
    #[test]
    fn request_unknown_field_passthrough() {
        let gemini_original = json!({
            "model": "gemini-1.5-pro",
            "contents": [{"role": "user", "parts": [{"text": "Hi"}]}],
            "safetySettings": [
                {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_ONLY_HIGH"}
            ],
            "x_future_gemini_field": "preserve_me"
        });

        let openai_intermediate = gemini_to_openai_request(&gemini_original);

        assert!(
            openai_intermediate.get("safetySettings").is_some(),
            "gemini_to_openai lost official field safetySettings"
        );
        assert!(
            openai_intermediate.get("x_future_gemini_field").is_some(),
            "gemini_to_openai lost custom field x_future_gemini_field"
        );
    }

    /// Output boundary rule: Gemini output direction only preserves standard/explicit extension fields,
    /// OpenAI lingering fields should not appear in Gemini output.
    #[test]
    fn response_output_boundary_filters_foreign_fields() {
        let openai = json!({
            "id": "chatcmpl-xyz",
            "model": "gemini-1.5-pro",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Reply"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3},
            "x_openai_future_field": "preserve_me",
            "system_fingerprint": "fp_abc123"
        });

        let gemini = openai_to_gemini_response(&openai);

        assert!(gemini.get("candidates").is_some());
        assert!(gemini.get("usageMetadata").is_some());
        // Whitelist mode: x_openai_future_field is not in Gemini extension whitelist and should be filtered
        assert!(
            gemini.get("x_openai_future_field").is_none(),
            "Gemini whitelist mode should filter non-whitelist field x_openai_future_field"
        );
        assert!(gemini.get("system_fingerprint").is_none());
    }
}

// ═══════════════════════════════════════════════════════════════════
// Azure protocol round-trip tests
//
// Azure OpenAI is OpenAI protocol + different URL + different auth,
// body is mostly passthrough. Verifies minimal transformation and field passthrough.
// ═══════════════════════════════════════════════════════════════════

mod azure_roundtrip {
    use super::*;
    use crate::proxy::protocol::{azure::AzureAdapter, azure_to_openai_request, ProtocolAdapter};

    const TEST_DEPLOYMENT: &str = "gpt-4o-deployment";

    /// Basic request round-trip
    #[test]
    fn request_basic_roundtrip() {
        let azure_original = json!({
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "temperature": 0.7
        });

        let openai_intermediate = azure_to_openai_request(&azure_original, TEST_DEPLOYMENT);
        assert_eq!(openai_intermediate["model"], TEST_DEPLOYMENT);
        assert_eq!(openai_intermediate["messages"][0]["content"], "Hi");

        // Upstream direction: AzureAdapter removes model field
        let mut back_to_azure = openai_intermediate.clone();
        let adapter = AzureAdapter;
        adapter.transform_request(&mut back_to_azure, TEST_DEPLOYMENT);

        // Azure upstream does not accept model in body (passed in URL)
        assert!(back_to_azure.get("model").is_none());
        assert_eq!(back_to_azure["messages"][0]["content"], "Hi");
    }

    /// Axiom 2: request direction unknown field passthrough
    #[test]
    fn request_unknown_field_passthrough() {
        let azure_original = json!({
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "x_azure_custom_header": "track-123",
            "dataSources": [{"type": "AzureCognitiveSearch", "parameters": {}}]
        });

        let openai_intermediate = azure_to_openai_request(&azure_original, TEST_DEPLOYMENT);

        assert!(
            openai_intermediate.get("x_azure_custom_header").is_some(),
            "azure_to_openai_request should preserve custom fields"
        );
        assert!(
            openai_intermediate.get("dataSources").is_some(),
            "azure_to_openai_request should preserve Azure proprietary field dataSources"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// OpenAI / Custom: base protocols, no translation needed
//
// OpenAI and Custom serve as baseline protocols without translation; verifies adapter core behaviors
// (setting model, URL construction, etc.) do not corrupt body.
// ═══════════════════════════════════════════════════════════════════

mod openai_roundtrip {
    use super::*;

    /// OpenAI adapter transform_request should only modify model field, keeping rest as-is
    #[test]
    fn openai_adapter_preserves_body() {
        // Construct via factory (keeps test independent of openai module public API)
        let adapter = crate::proxy::protocol::get_adapter("openai");
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.5,
            "x_custom": "preserve_me"
        });
        let original_messages = body["messages"].clone();
        let original_custom = body["x_custom"].clone();

        adapter.transform_request(&mut body, "gpt-4o");

        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"], original_messages);
        assert_eq!(body["temperature"], 0.5);
        // Blacklist mode: unknown fields passthrough,
        // no longer dropped by whitelists.
        assert_eq!(
            body.get("x_custom"),
            Some(&original_custom),
            "OpenAI blacklist mode should passthrough unknown field x_custom"
        );
    }

    /// Custom adapter: only modifies model and reuses OpenAI outbound boundary filter
    #[test]
    fn custom_adapter_preserves_body() {
        let adapter = crate::proxy::protocol::get_adapter("custom");
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "Hi"}],
            "x_deepseek_specific": "value"
        });

        adapter.transform_request(&mut body, "deepseek-chat");

        assert_eq!(body["model"], "deepseek-chat");
        // Blacklist mode: unknown/vendor proprietary fields passthrough (consumable by DeepSeek, etc.)
        assert_eq!(
            body["x_deepseek_specific"], "value",
            "Custom adapter blacklist mode should passthrough unknown field x_deepseek_specific"
        );
    }

    #[test]
    fn custom_adapter_uses_openai_boundary_filter() {
        let adapter = crate::proxy::protocol::get_adapter("custom");
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "input": "hi",
            "instructions": "x",
            "include": ["x"],
            "max_output_tokens": 10,
            "text": {"format": {"type": "text"}},
            "x_deepseek_specific": "value",
            "reasoning_effort": "high"
        });

        adapter.transform_request(&mut body, "deepseek-chat");

        assert_eq!(body["model"], "deepseek-chat");
        assert!(body.get("input").is_none());
        assert!(body.get("instructions").is_none());
        assert!(body.get("include").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("text").is_none());

        assert_eq!(body["messages"][0]["role"], "user");
        // Blacklist mode: known Responses proprietary fields dropped, unknown/vendor fields passthrough
        assert_eq!(
            body["x_deepseek_specific"], "value",
            "Custom adapter blacklist mode should passthrough unknown field x_deepseek_specific"
        );
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn openai_adapter_filters_responses_to_chat_intermediate_body() {
        let adapter = crate::proxy::protocol::get_adapter("openai");
        let mut body = json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "prompt": {"id": "pmpt_abc"},
            "include": ["web_search_call.action.sources"],
            "max_output_tokens": 256,
            "text": {"format": {"type": "json_object"}},
            "truncation": "auto",
            "previous_response_id": "resp_abc",
            "temperature": 0.2,
            "tools": []
        });

        adapter.transform_request(&mut body, "gpt-4o-mini");

        assert_eq!(body["model"], "gpt-4o-mini");
        assert!(body.get("prompt").is_none());
        assert!(body.get("include").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("text").is_none());
        assert!(body.get("truncation").is_none());
        assert!(body.get("previous_response_id").is_none());

        assert!(body.get("messages").is_some());
        assert_eq!(body["temperature"], 0.2);
        assert!(body.get("tools").is_some());
    }
}

// ═══════════════════════════════════════════════════════════════════
// Responses protocol round-trip tests
//
// Current status:
//    Downstream (client sends Responses -> OpenAI intermediate): handled in responses_handler.rs
//    Upstream (OpenAI intermediate -> Responses upstream): implemented in protocol/responses.rs
//
// Downstream tests verify request field mapping, tools conversion, unknown field passthrough.
// Upstream tests verify responses assertions.
//
// Unit tests covering adapter trait paths
// and JSON-level verification of public behaviors.
// Deep unit tests integrated into protocol/responses.rs.
// ═══════════════════════════════════════════════════════════════════

mod responses_roundtrip {
    use super::*;

    /// Responses upstream adapter verification.
    /// Through get_adapter("responses") we obtain
    /// an adapter performing real translation.
    #[test]
    fn adapter_registered_for_responses() {
        let adapter = crate::proxy::protocol::get_adapter("responses");
        // ResponsesAdapter.build_chat_url generates /v1/responses
        let url = adapter.build_chat_url("https://api.openai.com", "gpt-4o");
        assert!(
            url.ends_with("/v1/responses"),
            "Responses adapter chat URL should point to /v1/responses, got: {url}"
        );
    }

    /// Upstream adapter translates OpenAI chat.completions request
    /// into Responses request.
    #[test]
    fn request_openai_to_responses_upstream() {
        let adapter = crate::proxy::protocol::get_adapter("responses");
        let mut body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "Hi"}
            ],
            "temperature": 0.7,
            "stream": false
        });
        adapter.transform_request(&mut body, "gpt-4o");

        // Responses format: input replaces messages, instructions replaces system
        assert!(
            body.get("input").is_some(),
            "transform_request should generate input field"
        );
        assert_eq!(
            body["instructions"], "Be brief.",
            "system message should map to instructions"
        );
    }

    /// Response direction: Responses -> OpenAI
    #[test]
    fn response_responses_to_openai_upstream() {
        let adapter = crate::proxy::protocol::get_adapter("responses");
        let mut body = json!({
            "id": "resp_abc",
            "object": "response",
            "status": "completed",
            "model": "gpt-4o",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello"}]
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        });
        adapter.transform_response(&mut body);

        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["content"], "Hello");
        assert_eq!(body["usage"]["prompt_tokens"], 10);
        assert_eq!(body["usage"]["completion_tokens"], 5);
    }

    /// Output boundary rule: Responses upstream direction only retains standard/explicit extension fields,
    /// unknown fields not belonging to Responses must be dropped.
    #[test]
    fn upstream_output_boundary_filters_foreign_fields() {
        let adapter = crate::proxy::protocol::get_adapter("responses");
        let mut body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hi"}],
            "x_future_field": "preserve_me",
            "some_openai_extension": {"nested": true},
            "temperature": 0.2,
            "response_format": {"type": "json_object"},
            "n": 2,
            "logit_bias": {"123": 1}
        });
        adapter.transform_request(&mut body, "gpt-4o");

        assert!(body.get("input").is_some());
        assert!(body.get("messages").is_none());
        assert!(body.get("text").is_some());
        assert!(body.get("n").is_none());
        assert!(body.get("logit_bias").is_none());
        assert!(body.get("x_future_field").is_none());
        assert!(body.get("some_openai_extension").is_none());
    }
}
