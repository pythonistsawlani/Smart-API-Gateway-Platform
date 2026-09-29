use super::circuit_breaker::CircuitBreaker;
use super::handlers::ProxyError;
use super::keyword_log;
use super::middleware::{CallerKind, RequestContext};
use super::protocol::get_adapter;
use super::server::ProxyState;
use crate::database::{AccessKey, ApiEntry, AppSettings, Database};
use crate::services::api_key_utils::primary_api_key;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::Stream;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

const STREAMING_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const STREAMING_PING_INTERVAL: Duration = Duration::from_secs(10);

// ============================================================================
// 统一同协议直穿配置（Phase 1: Core Abstraction）
// ============================================================================

/// 协议直穿配置
///
/// 定义单个协议的直穿行为：判定条件、字段名、响应头、中间件策略等。
/// 所有协议使用统一的直穿机制，新增协议只需在配置表中添加一项。
#[derive(Debug, Clone)]
struct PassthroughConfig {
    /// 调用方类型（入口协议）
    caller_kind: CallerKind,

    /// 上游协议类型列表（支持多个，如 Claude 支持 "claude" 和 "anthropic"）
    upstream_types: &'static [&'static str],

    /// 原始请求暂存字段名（如 "__as_raw_claude_req"）
    raw_field: &'static str,

    /// 响应头标记名（如 "x-api-switch-claude-passthrough"）
    header_name: HeaderName,

    /// 是否跳过中间件注入
    ///
    /// 语义说明：
    /// - true: 跳过所有 ForwarderMiddleware（如 StreamOptionsMiddleware）
    ///   典型场景：上游不认识 OpenAI 专有字段（如 Claude 不支持 stream_options）
    /// - false: 执行所有中间件
    ///   典型场景：上游原生兼容 OpenAI（如 OpenAI/Azure/Custom 支持 stream_options）
    ///
    /// 注意：未来如需细粒度控制（如只跳过某个中间件），需扩展为位图或枚举集合
    skip_middleware: bool,
}

impl PassthroughConfig {
    /// 检查响应头是否匹配此配置
    fn matches_header(&self, header_value: &str) -> bool {
        self.header_name.as_str() == header_value
    }
}

/// 协议直穿配置表
///
/// 以 CallerKind 为 key，存储每个协议的直穿配置。
/// 新增协议直穿只需在此表中添加一项，无需修改核心逻辑。
static PASSTHROUGH_CONFIGS: Lazy<HashMap<CallerKind, PassthroughConfig>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // Responses API 直穿配置
    m.insert(
        CallerKind::Responses,
        PassthroughConfig {
            caller_kind: CallerKind::Responses,
            upstream_types: &["responses"],
            raw_field: "__as_raw_responses_req",
            header_name: HeaderName::from_static("x-api-switch-responses-passthrough"),
            skip_middleware: true, // Responses 专有字段，跳过 OpenAI 中间件
        },
    );

    // Claude Messages 直穿配置
    // 注意：数据库层会将 "claude" 规范化为 "anthropic"，因此只需匹配 "anthropic"
    m.insert(
        CallerKind::ClaudeMessages,
        PassthroughConfig {
            caller_kind: CallerKind::ClaudeMessages,
            upstream_types: &["anthropic"], // 已包含 claude（规范化后）
            raw_field: "__as_raw_claude_req",
            header_name: HeaderName::from_static("x-api-switch-claude-passthrough"),
            skip_middleware: true, // Claude 不认识 stream_options
        },
    );

    // OpenAI Chat 直穿配置
    // 注意：数据库层会将 "custom" 规范化为 "openai"，因此只需匹配 "openai"
    m.insert(
        CallerKind::OpenAiChat,
        PassthroughConfig {
            caller_kind: CallerKind::OpenAiChat,
            upstream_types: &["openai"], // 已包含 custom（规范化后）
            raw_field: "__as_raw_openai_req",
            header_name: HeaderName::from_static("x-api-switch-openai-passthrough"),
            skip_middleware: false, // 不跳过，stream_options 是 OpenAI 原生字段
        },
    );

    // Gemini Native 直穿配置
    m.insert(
        CallerKind::GeminiNative,
        PassthroughConfig {
            caller_kind: CallerKind::GeminiNative,
            upstream_types: &["gemini"],
            raw_field: "__as_raw_gemini_req",
            header_name: HeaderName::from_static("x-api-switch-gemini-passthrough"),
            skip_middleware: true, // Gemini native 不认识 stream_options
        },
    );

    // Azure Chat 直穿配置
    m.insert(
        CallerKind::AzureChat,
        PassthroughConfig {
            caller_kind: CallerKind::AzureChat,
            upstream_types: &["azure"],
            raw_field: "__as_raw_azure_req",
            header_name: HeaderName::from_static("x-api-switch-azure-passthrough"),
            skip_middleware: false, // 不跳过，Azure 兼容 OpenAI
        },
    );

    m
});

/// 判断是否启用同协议直穿
///
/// 检查三个条件：
/// 1. 调用方类型在配置表中
/// 2. 上游协议类型匹配配置
/// 3. 请求体中存在原始请求暂存字段
///
/// 返回匹配的配置，或 None（不启用直穿）
fn should_passthrough(
    caller_kind: &CallerKind,
    upstream_api_type: &str,
    body: &Value,
) -> Option<&'static PassthroughConfig> {
    let config = PASSTHROUGH_CONFIGS.get(caller_kind)?;

    // 检查上游类型是否匹配
    if !config.upstream_types.contains(&upstream_api_type) {
        return None;
    }

    // 检查是否存在原始请求暂存字段
    body.get(config.raw_field)?;

    Some(config)
}

/// 从响应头标记反查协议直穿配置
///
/// 用于流式处理：根据响应头判断协议类型，避免硬编码字符串比较。
/// 返回匹配的配置，或 None（非直穿响应）。
fn passthrough_config_from_header(header_value: &str) -> Option<&'static PassthroughConfig> {
    PASSTHROUGH_CONFIGS
        .values()
        .find(|config| config.matches_header(header_value))
}

/// 从中间态恢复原始请求体，并注入实际模型名
///
/// 用于同协议直穿：从中间态的暂存字段中恢复原始请求，
/// 同时注入路由解析后的实际模型名（支持 AUTO 场景）。
///
/// 注意：如果暂存字段不存在（理论上不应出现，因为 should_passthrough 已检查），
/// 则 fallback 到原始 body，但这表明配置逻辑存在漏洞。
fn restore_raw_request(body: &Value, config: &PassthroughConfig, actual_model: &str) -> Value {
    let mut raw = body
        .get(config.raw_field)
        .cloned()
        .unwrap_or_else(|| body.clone());

    if let Some(obj) = raw.as_object_mut() {
        // 注入实际模型名（关键：支持 AUTO 场景）
        obj.insert("model".to_string(), Value::String(actual_model.to_string()));
        // 移除暂存字段
        obj.remove(config.raw_field);
    }

    raw
}

/// 清理所有已知的暂存字段
///
/// 遍历配置表，移除所有协议的暂存字段，防止泄漏到上游。
/// 新增协议后自动包含，无需手动维护清理列表。
fn clean_all_raw_fields(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        for config in PASSTHROUGH_CONFIGS.values() {
            obj.remove(config.raw_field);
        }
    }
}

/// 为响应添加直穿标记头
///
/// 用于告知上层（handler）这是直穿响应，无需二次转换。
fn mark_passthrough_response(response: &mut axum::response::Response, config: &PassthroughConfig) {
    response
        .headers_mut()
        .insert(config.header_name.clone(), HeaderValue::from_static("true"));
}

#[derive(Debug, Default)]
struct SseDoneState {
    seen_done: bool,
    appended_model_info: bool,
    upstream_model_info_seen: bool,
}

#[derive(Debug, Clone, Copy)]
enum StreamEndReason {
    Done,
    UpstreamError,
    Timeout,
    DecodeTimeout,
    Dropped,
}

impl StreamEndReason {
    fn as_str(self) -> &'static str {
        match self {
            StreamEndReason::Done => "done",
            StreamEndReason::UpstreamError => "upstream_error",
            StreamEndReason::Timeout => "timeout",
            StreamEndReason::DecodeTimeout => "decode_timeout",
            StreamEndReason::Dropped => "dropped",
        }
    }
}

fn is_completed_stream_success(
    status_code: i32,
    has_sse_error: bool,
    has_text_delta: bool,
    has_tool_calls: bool,
    completion_tokens: i64,
) -> bool {
    // 仅在 HTTP 200、无 SSE 错误且存在有效输出时判定成功。
    // 有效输出定义为：出现文本 delta、出现工具调用、或返回的 completion_tokens>0。
    status_code == 200
        && !has_sse_error
        && (has_text_delta || has_tool_calls || completion_tokens > 0)
}

fn is_recoverable_decode_timeout_after_stream_started(
    status_code: i32,
    first_token_ms: i64,
    chunk_count: i64,
    streamed_bytes: i64,
    _has_text_delta: bool,
    _has_tool_calls: bool,
    _completion_tokens: i64,
    has_sse_error: bool,
    is_timeout: bool,
    is_decode: bool,
    is_body: bool,
) -> bool {
    let stream_started =
        status_code == 200 && (first_token_ms > 0 || chunk_count > 0 || streamed_bytes > 0);

    // 临时止血：HTTP 200 且已经开始推流后出现 body/decode timeout，
    // 更像传输中途不完整，而不是模型、Token 或上游入口不可用。
    // 本次请求仍记录失败，但不触发普通冷却/长期禁用阈值。
    // 即使已经出现有效输出，也不应把长流尾部超时归类为入口不可用。
    stream_started && is_timeout && is_decode && !is_body && !has_sse_error
}

fn is_decode_timeout_after_stream_started(
    err: &reqwest::Error,
    status_code: i32,
    first_token_ms: i64,
    chunk_count: i64,
    streamed_bytes: i64,
    has_text_delta: bool,
    has_tool_calls: bool,
    completion_tokens: i64,
    has_sse_error: bool,
) -> bool {
    is_recoverable_decode_timeout_after_stream_started(
        status_code,
        first_token_ms,
        chunk_count,
        streamed_bytes,
        has_text_delta,
        has_tool_calls,
        completion_tokens,
        has_sse_error,
        err.is_timeout(),
        err.is_decode(),
        err.is_body(),
    )
}

/// 判断客户端断开的流是否算成功。
///
/// 下游主动断开本身不是上游故障；但 HTTP 200 不能单独证明模型已有效输出。
/// 对 dropped stream，只有观测到文本 delta、工具调用，或输出 token，才记为成功。
/// 这避免 `200 + IN/OUT 0 + has_valid_output=false` 被 success=1 掩盖。
fn is_dropped_stream_success(
    status_code: i32,
    prompt_tokens: i64,
    completion_tokens: i64,
    has_text_delta: bool,
    has_tool_calls: bool,
) -> bool {
    let _ = prompt_tokens; // prompt_tokens 是输入消耗，不代表下游收到有效输出。
    status_code == 200 && (has_text_delta || has_tool_calls || completion_tokens > 0)
}

fn should_persist_raw_protocol_for_log(
    success: bool,
    prompt_tokens: i64,
    completion_tokens: i64,
    has_text_delta: bool,
    has_tool_calls: bool,
) -> bool {
    // 用户明确要求：错误的 IN/OUT 0/无有效输出场景必须保留原始记录，
    // 便于复现调试；正常 200 成功不保留，避免日志膨胀。
    if success {
        return false;
    }

    let has_valid_output = has_text_delta || has_tool_calls || completion_tokens > 0;
    prompt_tokens == 0 || completion_tokens == 0 || !has_valid_output
}

#[derive(Debug, Clone)]
struct AttemptInfo {
    entry_id: String,
    channel_name: String,
    model: String,
    status_code: i32,
    success: bool,
    error: Option<String>,
}

fn attempt_path_json(attempts: &[AttemptInfo]) -> String {
    serde_json::to_string(
        &attempts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "entry_id": a.entry_id,
                    "channel": a.channel_name,
                    "model": a.model,
                    "status_code": a.status_code,
                    "success": a.success,
                    "error": a.error,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string())
}

/// 把 attempt_path JSON 渲染成短文本，便于直接展示在 content 头部。
/// 输出形如 `api.beyfish/gpt-5.5[200] -> other/x[502]`，
/// 多个失败重试用 `->` 连接；解析失败时回退原文。
fn format_attempt_path_short(attempt_path: Option<&str>) -> String {
    let Some(json_str) = attempt_path else {
        return String::new();
    };
    if json_str.trim().is_empty() {
        return String::new();
    }
    let arr: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return json_str.to_string(),
    };
    arr.iter()
        .filter_map(|v| {
            let channel = v.get("channel")?.as_str()?;
            let model = v.get("model")?.as_str()?;
            let status = v.get("status_code")?.as_i64()?;
            Some(format!("{}/{}[{}]", channel, model, status))
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn push_attempt(
    attempts: &mut Vec<AttemptInfo>,
    entry: &ApiEntry,
    status_code: i32,
    success: bool,
    error: Option<String>,
) {
    attempts.push(AttemptInfo {
        entry_id: entry.id.clone(),
        channel_name: entry
            .channel_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        model: entry.model.clone(),
        status_code,
        success,
        error,
    });
}

fn attempt_path_with_current(
    prior_attempts: &[AttemptInfo],
    entry: &ApiEntry,
    status_code: i32,
    success: bool,
    error: Option<String>,
) -> String {
    let mut attempts = prior_attempts.to_vec();
    push_attempt(&mut attempts, entry, status_code, success, error);
    attempt_path_json(&attempts)
}

fn sanitize_url_for_log(url: &str) -> String {
    let mut sanitized = url.to_string();
    for key in ["key", "api_key", "access_token", "token"] {
        let marker = format!("{key}=");
        let mut search_from = 0;
        while let Some(pos) = sanitized[search_from..].find(&marker) {
            let value_start = search_from + pos + marker.len();
            let value_end = sanitized[value_start..]
                .find('&')
                .map(|offset| value_start + offset)
                .unwrap_or(sanitized.len());
            sanitized.replace_range(value_start..value_end, "***");
            search_from = value_start + 3;
        }
    }
    sanitized
}

fn caller_kind_label(caller_kind: &CallerKind) -> &'static str {
    match caller_kind {
        CallerKind::OpenAiChat => "openai_chat",
        CallerKind::ClaudeMessages => "claude_messages",
        CallerKind::GeminiNative => "gemini_native",
        CallerKind::AzureChat => "azure_chat",
        CallerKind::Responses => "responses",
    }
}

fn response_header_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

#[allow(clippy::too_many_arguments)]
fn build_raw_protocol_request_log(
    caller_kind: &CallerKind,
    requested_model: &str,
    entry: &ApiEntry,
    channel_api_type: &str,
    url: &str,
    is_stream: bool,
    passthrough_header: Option<&str>,
    gateway_body: &Value,
    upstream_body: &Value,
) -> Value {
    serde_json::json!({
        "event": "upstream_request",
        "timestamp_ms": chrono::Utc::now().timestamp_millis(),
        "caller_kind": caller_kind_label(caller_kind),
        "requested_model": requested_model,
        "entry_id": &entry.id,
        "entry_model": &entry.model,
        "channel_id": &entry.channel_id,
        "channel_name": entry.channel_name.as_deref().unwrap_or("unknown"),
        "channel_api_type": channel_api_type,
        "url": url,
        "is_stream": is_stream,
        "passthrough_header": passthrough_header,
        "gateway_body": gateway_body,
        "upstream_body": upstream_body,
    })
}

fn with_raw_protocol_response(
    mut raw_protocol: Value,
    status_code: u16,
    response_headers: &reqwest::header::HeaderMap,
    upstream_response_body: Value,
) -> Value {
    if let Some(obj) = raw_protocol.as_object_mut() {
        obj.insert(
            "event".to_string(),
            Value::String("upstream_response".to_string()),
        );
        obj.insert(
            "response_timestamp_ms".to_string(),
            Value::Number(chrono::Utc::now().timestamp_millis().into()),
        );
        obj.insert(
            "status_code".to_string(),
            Value::Number(serde_json::Number::from(status_code)),
        );
        obj.insert(
            "content_type".to_string(),
            response_header_value(response_headers, "content-type")
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        obj.insert(
            "x_request_id".to_string(),
            response_header_value(response_headers, "x-request-id")
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        obj.insert("upstream_response_body".to_string(), upstream_response_body);
    }
    raw_protocol
}

fn with_raw_protocol_error(
    mut raw_protocol: Value,
    status_code: u16,
    response_headers: Option<&reqwest::header::HeaderMap>,
    error_body: Value,
) -> Value {
    if let Some(obj) = raw_protocol.as_object_mut() {
        obj.insert(
            "event".to_string(),
            Value::String("upstream_error".to_string()),
        );
        obj.insert(
            "response_timestamp_ms".to_string(),
            Value::Number(chrono::Utc::now().timestamp_millis().into()),
        );
        obj.insert(
            "status_code".to_string(),
            Value::Number(serde_json::Number::from(status_code)),
        );
        if let Some(headers) = response_headers {
            obj.insert(
                "content_type".to_string(),
                response_header_value(headers, "content-type")
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            obj.insert(
                "x_request_id".to_string(),
                response_header_value(headers, "x-request-id")
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
        }
        obj.insert("error_body".to_string(), error_body);
    }
    raw_protocol
}

fn with_raw_protocol_stream_capture(
    mut raw_protocol: Value,
    status_code: i32,
    response_headers: &reqwest::header::HeaderMap,
    stream_phase: &str,
    stream_end_reason: Option<StreamEndReason>,
    stream_summary: &str,
    raw_sse: Option<&str>,
) -> Value {
    if let Some(obj) = raw_protocol.as_object_mut() {
        obj.insert(
            "event".to_string(),
            Value::String("upstream_stream".to_string()),
        );
        obj.insert(
            "response_timestamp_ms".to_string(),
            Value::Number(chrono::Utc::now().timestamp_millis().into()),
        );
        obj.insert(
            "status_code".to_string(),
            Value::Number(serde_json::Number::from(status_code)),
        );
        obj.insert(
            "content_type".to_string(),
            response_header_value(response_headers, "content-type")
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        obj.insert(
            "x_request_id".to_string(),
            response_header_value(response_headers, "x-request-id")
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        obj.insert(
            "upstream_response_body".to_string(),
            serde_json::json!({
                "stream_phase": stream_phase,
                "stream_end_reason": stream_end_reason.map(StreamEndReason::as_str),
                "stream_summary": stream_summary,
                "raw_sse": raw_sse,
            }),
        );
    }
    raw_protocol
}

fn build_stream_diagnostic(
    stage: &str,
    err: Option<&reqwest::Error>,
    entry: &ApiEntry,
    url: &str,
    status_code: i32,
    headers: &reqwest::header::HeaderMap,
    chunk_count: i64,
    streamed_bytes: i64,
    buffered_sse_bytes: usize,
    first_token_ms: i64,
    elapsed_ms: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    has_text_delta: bool,
    has_tool_calls: bool,
    has_sse_error: bool,
    stream_success: Option<bool>,
) -> String {
    let has_valid_output = has_text_delta || has_tool_calls || completion_tokens > 0;
    let detail = serde_json::json!({
        "stage": stage,
        "channel": entry.channel_name.clone().unwrap_or_else(|| "unknown".to_string()),
        "entry_id": entry.id,
        "resolved_model": entry.model,
        "url": sanitize_url_for_log(url),
        "status_code": status_code,
        "elapsed_ms": elapsed_ms,
        "first_token_ms": first_token_ms,
        "chunk_count": chunk_count,
        "streamed_bytes": streamed_bytes,
        "buffered_sse_bytes": buffered_sse_bytes,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "has_text_delta": has_text_delta,
        "has_tool_calls": has_tool_calls,
        "has_sse_error": has_sse_error,
        "has_valid_output": has_valid_output,
        "stream_success": stream_success,
        "content_type": response_header_value(headers, "content-type"),
        "content_encoding": response_header_value(headers, "content-encoding"),
        "transfer_encoding": response_header_value(headers, "transfer-encoding"),
        "server": response_header_value(headers, "server"),
        "via": response_header_value(headers, "via"),
        "cf_ray": response_header_value(headers, "cf-ray"),
        "x_request_id": response_header_value(headers, "x-request-id"),
        "x_accel_buffering": response_header_value(headers, "x-accel-buffering"),
        "error": err.map(|e| serde_json::json!({
            "kind": reqwest_error_kind(e),
            "is_timeout": e.is_timeout(),
            "is_connect": e.is_connect(),
            "is_request": e.is_request(),
            "is_body": e.is_body(),
            "is_decode": e.is_decode(),
            "is_redirect": e.is_redirect(),
            "source_chain": reqwest_source_chain(e),
            "message": e.to_string(),
        })),
    });
    format!("Stream diagnostic: {detail}")
}

fn reqwest_error_kind(err: &reqwest::Error) -> &'static str {
    if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_request() {
        "request"
    } else if err.is_body() {
        "body"
    } else if err.is_decode() {
        "decode"
    } else if err.is_redirect() {
        "redirect"
    } else {
        "unknown"
    }
}

fn reqwest_source_chain(err: &reqwest::Error) -> Vec<String> {
    let mut chain = Vec::new();
    let mut source = err.source();
    while let Some(current) = source {
        chain.push(current.to_string());
        source = current.source();
    }
    chain
}

fn format_reqwest_error(stage: &str, url: &str, err: reqwest::Error) -> String {
    let detail = serde_json::json!({
        "stage": stage,
        "url": sanitize_url_for_log(url),
        "kind": reqwest_error_kind(&err),
        "is_timeout": err.is_timeout(),
        "is_connect": err.is_connect(),
        "is_request": err.is_request(),
        "is_body": err.is_body(),
        "is_decode": err.is_decode(),
        "is_redirect": err.is_redirect(),
        "source_chain": reqwest_source_chain(&err),
        "message": err.to_string(),
    });
    format!("Request failed: {detail}")
}

/// Forward error with upstream status code (0 = connection failure).
struct ForwardError {
    message: String,
    status: u16,
    raw_protocol: Option<Value>,
}

impl ForwardError {
    fn new(message: impl Into<String>, status: u16, raw_protocol: Option<Value>) -> Self {
        Self {
            message: message.into(),
            status,
            raw_protocol,
        }
    }
}

struct ForwardResult {
    response: axum::response::Response,
    prompt_tokens: i64,
    completion_tokens: i64,
    reasoning_tokens: i64,
    first_token_ms: i64,
    status_code: i32,
}

/// StreamLogGuard: safety net for writing usage log when stream is dropped
/// without reaching Poll::Ready(None) (e.g. client disconnect).
/// Primary log writing happens in Poll::Ready(None) — this guard is fallback only.
struct StreamLogGuard {
    logged: Arc<AtomicBool>,
    db: Arc<Database>,
    app_handle: Option<crate::AppEventHandle>,
    access_key: Option<AccessKey>,
    entry: ApiEntry,
    requested_model: String,
    prompt_tokens: Arc<AtomicI64>,
    completion_tokens: Arc<AtomicI64>,
    has_text_delta: Arc<AtomicBool>,
    has_tool_calls: Arc<AtomicBool>,
    first_token_ms: Arc<AtomicI64>,
    chunk_count: Arc<AtomicI64>,
    streamed_bytes: Arc<AtomicI64>,
    status_code: i32,
    start: Instant,
    prior_attempts: Vec<AttemptInfo>,
    upstream_url: String,
    response_headers: reqwest::header::HeaderMap,
    raw_protocol: Option<Value>,
    /// 空流检测冷却时长（秒），取自 settings.circuit_recovery_secs
    empty_stream_cooldown_secs: i64,
}

impl Drop for StreamLogGuard {
    fn drop(&mut self) {
        if !self.logged.swap(true, Ordering::SeqCst) {
            let prompt_tokens = self.prompt_tokens.load(Ordering::SeqCst);
            let completion_tokens = self.completion_tokens.load(Ordering::SeqCst);
            let chunk_total = self.chunk_count.load(Ordering::SeqCst);
            let byte_total = self.streamed_bytes.load(Ordering::SeqCst);
            let first_token_ms = self.first_token_ms.load(Ordering::SeqCst);
            let has_text_delta = self.has_text_delta.load(Ordering::SeqCst);
            let has_tool_calls = self.has_tool_calls.load(Ordering::SeqCst);
            let latency_ms = self.start.elapsed().as_millis() as i64;
            let success = is_dropped_stream_success(
                self.status_code,
                prompt_tokens,
                completion_tokens,
                has_text_delta,
                has_tool_calls,
            );
            let attempt_path = attempt_path_with_current(
                &self.prior_attempts,
                &self.entry,
                self.status_code,
                success,
                None,
            );
            let stream_summary = build_stream_diagnostic(
                "stream_dropped",
                None,
                &self.entry,
                &self.upstream_url,
                self.status_code,
                &self.response_headers,
                chunk_total,
                byte_total,
                0, // sse_buffer not available in drop context
                first_token_ms,
                latency_ms,
                prompt_tokens,
                completion_tokens,
                has_text_delta,
                has_tool_calls,
                false,
                Some(success),
            );
            let db = self.db.clone();
            let app_handle = self.app_handle.clone();
            let access_key = self.access_key.clone();
            let entry = self.entry.clone();
            let requested_model = self.requested_model.clone();
            let status_code = self.status_code;
            let empty_stream_cooldown_secs = self.empty_stream_cooldown_secs;
            let raw_protocol = should_persist_raw_protocol_for_log(
                success,
                prompt_tokens,
                completion_tokens,
                has_text_delta,
                has_tool_calls,
            )
            .then(|| self.raw_protocol.clone())
            .flatten();
            tokio::spawn(async move {
                log_usage(
                    &db,
                    &app_handle,
                    access_key.as_ref(),
                    &entry,
                    &requested_model,
                    true,
                    prompt_tokens,
                    completion_tokens,
                    0,
                    first_token_ms,
                    latency_ms,
                    status_code,
                    success,
                    Some(stream_summary.as_str()),
                    Some(attempt_path.as_str()),
                    Some(StreamEndReason::Dropped),
                    raw_protocol.as_ref(),
                );

                // 空流检测：上游返回 200 但 SSE 流中无实际输出数据。
                // 此类流被丢弃后，如果仅标记 success=true 不触发冷却，
                // 坏通道会持续留在路由池中反复被选中，导致下游反复中断。
                // 此处设置 DB cooldown 让路由跳过该 entry，但不递增
                // failure_counts，防止触发 6h 自动禁用。
                if status_code == 200
                    && byte_total < 512
                    && chunk_total < 5
                    && prompt_tokens == 0
                    && completion_tokens == 0
                {
                    let cooldown_until =
                        chrono::Utc::now().timestamp() + empty_stream_cooldown_secs;
                    let _ = db.set_entry_cooldown(&entry.id, Some(cooldown_until));
                    if let Some(h) = &app_handle {
                        crate::event::emit(h, "entries-changed");
                    }
                    crate::state_version::bump("pool");
                    log::warn!(
                        "Entry {} cooled down for {}s after empty stream drop.",
                        entry.id,
                        empty_stream_cooldown_secs
                    );
                }
            });
        }
    }
}

/// Forward a request to the resolved entries with retry/failover.
///
/// Personal-version cooldown strategy:
/// 1. Any upstream failure is considered abnormal for this model entry.
/// 2. Failed entries are cooled down for `circuit_recovery_secs` seconds and skipped by routing.
/// 3. Unrecoverable status codes can disable an entry automatically.
pub async fn forward_with_retry(
    state: &ProxyState,
    entries: &[ApiEntry],
    body: &Value,
    original_headers: &HeaderMap,
    requested_model: &str,
    access_key: Option<&AccessKey>,
    is_stream: bool,
    middleware: &[Arc<dyn super::middleware::ForwarderMiddleware>],
    caller_kind: CallerKind,
) -> Result<axum::response::Response, ProxyError> {
    let mut last_error: Option<(String, u16)> = None;
    let mut attempts: Vec<AttemptInfo> = Vec::new();

    for entry in entries {
        let start = Instant::now();

        // Check circuit breaker
        {
            let breakers = state.circuit_breakers.read().await;
            if let Some(cb) = breakers.get(&entry.id) {
                if !cb.is_available() {
                    continue;
                }
            }
        }

        match forward_single(
            state,
            entry,
            body,
            requested_model,
            access_key,
            is_stream,
            attempts.clone(),
            middleware,
            &caller_kind,
            original_headers,
        )
        .await
        {
            Ok(result) => {
                let elapsed = start.elapsed();

                if !is_stream {
                    record_circuit_success(state, &entry.id).await;
                    push_attempt(&mut attempts, entry, result.status_code, true, None);
                    let attempt_path = attempt_path_json(&attempts);
                    let latency_ms = elapsed.as_millis() as i64;
                    log_usage(
                        &state.db,
                        &state.app_handle,
                        access_key,
                        entry,
                        requested_model,
                        is_stream,
                        result.prompt_tokens,
                        result.completion_tokens,
                        result.reasoning_tokens,
                        result.first_token_ms,
                        latency_ms,
                        result.status_code,
                        true,
                        None,
                        Some(attempt_path.as_str()),
                        None,
                        None,
                    );
                }
                return Ok(result.response);
            }
            Err(err) => {
                let e = err.message;
                let status = err.status;
                let raw_protocol = err.raw_protocol;
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as i64;
                let log_status = if status > 0 { status as i32 } else { 502 };
                let settings = state.settings.read().await.clone();
                push_attempt(&mut attempts, entry, log_status, false, Some(e.clone()));
                let attempt_path = attempt_path_json(&attempts);

                // Step 1: Always write usage log for every failed attempt
                log_usage(
                    &state.db,
                    &state.app_handle,
                    access_key,
                    entry,
                    requested_model,
                    is_stream,
                    0,
                    0,
                    0,
                    0,
                    latency_ms,
                    log_status,
                    false,
                    Some(&e),
                    Some(attempt_path.as_str()),
                    None,
                    raw_protocol.as_ref(),
                );

                // Step 2: disable unrecoverable status codes or error messages
                // matching disable keywords; otherwise cool down briefly.
                // Connection failures report status=0 and must remain recoverable.
                let disable_by_status = status > 0
                    && should_disable_entry_for_status(&settings.circuit_disable_codes, status);
                let disable_by_keyword = status > 0
                    && should_disable_entry_for_message(effective_disable_keywords(&settings), &e);

                if disable_by_keyword {
                    log::info!(
                        "Cooldown entry {} (keyword match), status={}: {}",
                        entry.id,
                        status,
                        e
                    );
                    if settings.keyword_freeze_scope == "channel" {
                        freeze_channel_entries(state, entry).await;
                    } else {
                        cool_down_entry(state, entry).await;
                    }
                } else if disable_by_status {
                    disable_entry(state, entry).await;
                } else {
                    // 既未命中禁用状态码，也未命中已知 disable_keywords：
                    // 记录状态码+原文，供分析后增补 disable_keywords（keyword 越健壮，
                    // 落入重试环节的失败越少）。状态码一并记录，便于判断哪类值得重试。
                    let status_for_log = if status > 0 { status } else { 0 };
                    if should_record_keyword_candidate(status_for_log, &e, &settings) {
                        keyword_log::record_unknown_failure(status_for_log, &e).await;
                    }
                    cool_down_entry(state, entry).await;
                }

                last_error = Some((e, status));
                continue;
            }
        }
    }

    Err(last_error
        .map(|(msg, status)| {
            if status > 0 {
                ProxyError::Upstream {
                    status,
                    message: msg,
                }
            } else {
                ProxyError::Internal(msg)
            }
        })
        .unwrap_or(ProxyError::AllProvidersFailed))
}

fn is_inside_unclosed_fenced_code(text: &str, byte_pos: usize) -> bool {
    let before = &text[..byte_pos.min(text.len())];
    before
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
}

fn strip_model_info_suffix(text: &str) -> String {
    let trimmed_end = text.trim_end();
    let Some(last_line_start) = trimmed_end.rfind('\n').map(|idx| idx + 1) else {
        return text.to_string();
    };
    let last_line = &trimmed_end[last_line_start..];
    let candidate = last_line.trim();
    if !candidate.starts_with("model: ") || candidate["model: ".len()..].trim().is_empty() {
        return text.to_string();
    }
    if is_inside_unclosed_fenced_code(trimmed_end, last_line_start) {
        return text.to_string();
    }

    text[..last_line_start].trim_end().to_string()
}

fn clean_text_value_model_suffix(value: &mut Value) {
    if let Some(text) = value.as_str() {
        let cleaned = strip_model_info_suffix(text);
        if cleaned != text {
            *value = Value::String(cleaned);
        }
    }
}

fn clean_assistant_content_model_suffix(content: &mut Value) {
    match content {
        Value::String(_) => clean_text_value_model_suffix(content),
        Value::Array(parts) => {
            for part in parts.iter_mut().rev() {
                if let Some(obj) = part.as_object_mut() {
                    for key in ["text", "content"] {
                        if let Some(text_value) = obj.get_mut(key) {
                            let before = text_value.clone();
                            clean_text_value_model_suffix(text_value);
                            if *text_value != before {
                                return;
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn clean_model_info_from_inbound_body(body: &mut Value) {
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
                if let Some(content) = message.get_mut("content") {
                    clean_assistant_content_model_suffix(content);
                }
            }
        }
    }

    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item.get("role").and_then(Value::as_str) == Some("assistant") {
                if let Some(content) = item.get_mut("content") {
                    clean_assistant_content_model_suffix(content);
                }
            }
        }
    }
}
fn remove_reasoning_trigger_fields(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    obj.remove("thinking");
    obj.remove("reasoning");
    obj.remove("reasoning_content");
    obj.remove("reasoning_text");
    obj.remove("reasoning_details");
    obj.remove("reasoning_effort");
}

fn apply_disable_reasoning(body: &mut Value) {
    remove_reasoning_trigger_fields(body);

    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            remove_reasoning_trigger_fields(message);
        }
    }
}
async fn forward_single(
    state: &ProxyState,
    entry: &ApiEntry,
    body: &Value,
    requested_model: &str,
    access_key: Option<&AccessKey>,
    is_stream: bool,
    prior_attempts: Vec<AttemptInfo>,
    middleware: &[Arc<dyn super::middleware::ForwarderMiddleware>],
    caller_kind: &CallerKind,
    original_headers: &HeaderMap,
) -> Result<ForwardResult, ForwardError> {
    let settings = state.settings.read().await.clone();
    let channel = state
        .db
        .get_channel(&entry.channel_id)
        .map_err(|e| ForwardError::new(format!("DB error: {e}"), 502, None))?;

    let adapter = get_adapter(&channel.api_type);
    let url = adapter.build_chat_url(&channel.base_url, &entry.model);

    // 统一直穿判定（Phase 2: 替换现有实现）
    let passthrough_config = should_passthrough(caller_kind, &channel.api_type, body);

    // 处理 disable_reasoning
    // 设计说明：直穿场景下也应用 disable_reasoning，因为这是用户全局设置，
    // 不应因协议直穿而绕过。直穿只是避免协议转换损耗，不是跳过业务逻辑。
    let body_for_transform = if settings.disable_reasoning {
        let mut modified = body.clone();
        apply_disable_reasoning(&mut modified);
        modified
    } else {
        body.clone()
    };

    let mut upstream_body = if let Some(config) = passthrough_config {
        // 统一恢复逻辑：先恢复原始请求，再应用 disable_reasoning
        let mut restored = restore_raw_request(body, config, &entry.model);
        if settings.disable_reasoning {
            apply_disable_reasoning(&mut restored);
        }
        restored
    } else {
        let mut converted = body_for_transform.clone();
        adapter.transform_request(&mut converted, &entry.model);
        converted
    };

    // 统一清理所有暂存字段
    clean_all_raw_fields(&mut upstream_body);

    clean_model_info_from_inbound_body(&mut upstream_body);

    // Call middleware on_request
    let ctx = RequestContext {
        caller_kind: caller_kind.clone(),
        requested_model: Arc::<str>::from(requested_model.to_string()),
    };
    // 根据配置决定是否跳过中间件
    if !passthrough_config.is_some_and(|c| c.skip_middleware) {
        for mw in middleware.iter() {
            mw.on_request(&mut upstream_body, &ctx);
        }
    }

    let mut request = adapter
        .apply_auth(
            state.http_client.post(&url),
            primary_api_key(&channel.api_key),
        )
        .json(&upstream_body);

    // 同协议直通：透传客户端(真实 Claude Code CLI)的身份/能力头，使上游能识别
    // CLI 身份（认证层）并开启对应能力（如 anthropic-beta 里的
    // mid-conversation-system）。否则上游回退旧行为，拒收 messages 数组中的
    // role:"system" → 400。注意：不透传 authorization（鉴权用渠道自己的 api_key），
    // 也不透传 host/content-length/connection 等逐跳/长度相关头。
    // 暂时保留 Claude 特殊处理（P1 再配置化）
    if matches!(
        passthrough_config.map(|c| c.caller_kind),
        Some(CallerKind::ClaudeMessages)
    ) {
        const PASSTHROUGH_HEADER_PREFIXES: &[&str] = &["x-stainless-", "x-claude-code-"];
        // 注意：不含 anthropic-version —— apply_auth 已设置（reqwest 的 header() 是
        // 追加而非覆盖，重复透传会产生重复头）。仅透传 apply_auth 未设置的身份/能力头。
        const PASSTHROUGH_HEADER_EXACT: &[&str] = &["anthropic-beta", "user-agent", "x-app"];
        for (name, value) in original_headers.iter() {
            let n = name.as_str().to_ascii_lowercase();
            let allow = PASSTHROUGH_HEADER_EXACT.contains(&n.as_str())
                || PASSTHROUGH_HEADER_PREFIXES.iter().any(|p| n.starts_with(p));
            if allow {
                if let Ok(v) = value.to_str() {
                    request = request.header(name.as_str(), v);
                }
            }
        }
    }

    // 先透传客户端 Header，再按设置决定是否应用渠道 Header；同名时以渠道配置为准。
    if passthrough_config.is_none() || settings.passthrough_header_injection {
        request = crate::services::upstream_headers::apply_upstream_headers(
            request,
            channel.upstream_headers.as_deref(),
        )
        .map_err(|e| ForwardError::new(e.to_string(), 502, None))?;
    }

    if is_stream {
        request = request.header("Accept", "text/event-stream");
        request = request.header("Accept-Encoding", "identity");
    }

    // 始终在本次请求内临时构造 raw protocol 上下文；是否写入 DB 由失败/空输出
    // 场景决定。这样错误 IN/OUT 0 能复现，正常 200 成功不会持久化 raw。
    let raw_protocol_request = Some(build_raw_protocol_request_log(
        caller_kind,
        requested_model,
        entry,
        &channel.api_type,
        &url,
        is_stream,
        passthrough_config.map(|c| c.header_name.as_str()),
        &body,
        &upstream_body,
    ));

    // Start timer BEFORE sending request — this measures true TTFB
    let request_start = std::time::Instant::now();

    let response = request.send().await.map_err(|e| {
        let message = format_reqwest_error("send", &url, e);
        let raw_protocol = raw_protocol_request
            .clone()
            .map(|raw| with_raw_protocol_error(raw, 0, None, Value::String(message.clone())));
        ForwardError::new(message, 0, raw_protocol)
    })?;

    let status = response.status().as_u16();
    let response_headers = response.headers().clone();

    if !response.status().is_success() {
        // Read full error body for debugging
        let raw_body = response.bytes().await.unwrap_or_default();
        let error_body = String::from_utf8_lossy(&raw_body);

        // If error body is empty, just use status code
        let error_msg = if error_body.is_empty() {
            format!("Upstream error {status}")
        } else {
            format!("Upstream error {status}: {error_body}")
        };
        let raw_protocol = raw_protocol_request.clone().map(|raw| {
            with_raw_protocol_error(
                raw,
                status,
                Some(&response_headers),
                Value::String(error_body.to_string()),
            )
        });
        return Err(ForwardError::new(error_msg, status, raw_protocol));
    }

    let status_code = status as i32;

    if is_stream {
        let needs_transform = passthrough_config.is_none() && adapter.needs_sse_transform();
        let append_model_info = should_append_model_info(state, &body, caller_kind);
        let mut response = build_streaming_response(
            state,
            entry,
            access_key,
            requested_model,
            &url,
            response,
            status_code,
            needs_transform,
            adapter,
            request_start,
            prior_attempts,
            append_model_info,
            passthrough_config.map(|c| c.header_name.as_str()),
            raw_protocol_request.clone(),
            middleware,
            &ctx,
        );
        // 统一添加直穿响应头
        if let Some(config) = passthrough_config {
            mark_passthrough_response(&mut response, config);
        }
        Ok(ForwardResult {
            response,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            first_token_ms: 0,
            status_code,
        })
    } else {
        let mut response_body: Value = response.json().await.map_err(|e| {
            let message = format!("Failed to parse response: {e}");
            let raw_protocol = raw_protocol_request.clone().map(|raw| {
                with_raw_protocol_error(
                    raw,
                    502,
                    Some(&response_headers),
                    Value::String(message.clone()),
                )
            });
            ForwardError::new(message, 502, raw_protocol)
        })?;

        if passthrough_config.is_none() {
            adapter.transform_response(&mut response_body);

            // Normalize reasoning fields in response messages (reasoning_content ↔ reasoning_text)
            if let Some(choices) = response_body
                .get_mut("choices")
                .and_then(|c| c.as_array_mut())
            {
                for choice in choices.iter_mut() {
                    if let Some(message) = choice.get_mut("message") {
                        normalize_reasoning_fields(message);
                    }
                }
            }
        }

        for mw in middleware.iter() {
            mw.on_response_complete(&mut response_body, &ctx);
        }
        let (prompt_tokens, completion_tokens, reasoning_tokens) =
            extract_usage_tokens(&response_body);

        // 验证输出有效性（根据协议类型）
        let has_valid_output = if let Some(config) = passthrough_config {
            match config.caller_kind {
                CallerKind::Responses => responses_response_has_valid_output(&response_body),
                CallerKind::ClaudeMessages => claude_response_has_valid_output(&response_body),
                CallerKind::GeminiNative => gemini_response_has_valid_output(&response_body),
                _ => nonstream_response_has_valid_output(&response_body),
            }
        } else {
            nonstream_response_has_valid_output(&response_body)
        };
        if !has_valid_output {
            let message = "upstream HTTP 200 completed without valid output".to_string();
            let raw_protocol = raw_protocol_request.clone().map(|raw| {
                with_raw_protocol_response(raw, status, &response_headers, response_body.clone())
            });
            return Err(ForwardError::new(message, 502, raw_protocol));
        }

        let mut response = axum::Json(response_body).into_response();
        // 统一添加直穿响应头
        if let Some(config) = passthrough_config {
            mark_passthrough_response(&mut response, config);
        }

        Ok(ForwardResult {
            response,
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            first_token_ms: 0,
            status_code,
        })
    }
}

fn extract_usage_tokens(body: &Value) -> (i64, i64, i64) {
    // 兼容三种位置：OpenAI 顶层 usage、Responses 的 /response/usage、
    // Anthropic message_start 的 /message/usage。
    let usage = body
        .get("usage")
        .or_else(|| body.pointer("/response/usage"))
        .or_else(|| body.pointer("/message/usage"));
    let prompt_tokens = usage
        .and_then(|v| v.get("prompt_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            // Anthropic：input_tokens + 缓存 token（高缓存命中时 input_tokens 可能为 0）
            anthropic_prompt_tokens(usage)
        });
    let completion_tokens = usage
        .and_then(|v| v.get("completion_tokens"))
        .or_else(|| usage.and_then(|v| v.get("output_tokens")))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let reasoning_tokens = usage
        .and_then(|v| v.pointer("/completion_tokens_details/reasoning_tokens"))
        .or_else(|| usage.and_then(|v| v.pointer("/output_tokens_details/reasoning_tokens")))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    (prompt_tokens, completion_tokens, reasoning_tokens)
}

fn responses_response_has_valid_output(body: &Value) -> bool {
    body.get("object").and_then(Value::as_str) == Some("response")
        && body
            .get("output")
            .and_then(Value::as_array)
            .is_some_and(|output| !output.is_empty())
}

fn nonstream_response_has_valid_output(body: &Value) -> bool {
    body.get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|choice| {
            let message = choice.get("message");
            let has_content = message
                .and_then(|msg| msg.get("content"))
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty());
            let has_tool_calls = message
                .and_then(|msg| msg.get("tool_calls"))
                .and_then(Value::as_array)
                .is_some_and(|tool_calls| !tool_calls.is_empty());
            let has_function_call = message
                .and_then(|msg| msg.get("function_call"))
                .is_some_and(|function_call| !function_call.is_null());
            // 推理模型可能返回 reasoning_content 而无 content
            let has_reasoning = message.is_some_and(|msg| {
                msg.get("reasoning_content")
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.is_empty())
                    || msg
                        .get("reasoning_text")
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty())
                    || msg
                        .get("reasoning_details")
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty())
            });

            has_content || has_tool_calls || has_function_call || has_reasoning
        })
}

/// 校验直通的 Gemini 原生非流式响应是否有有效输出。
/// Gemini generateContent 响应形如 `{"candidates":[{"content":{"parts":[...]}}]}`，
/// 与 OpenAI 的 `choices[]` 结构不同，需独立判断。
fn gemini_response_has_valid_output(body: &Value) -> bool {
    body.get("candidates")
        .and_then(Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .pointer("/content/parts")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            let has_text = part
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|text| !text.is_empty());
                            let has_function_call =
                                part.get("functionCall").is_some_and(|v| !v.is_null());
                            let has_inline_data =
                                part.get("inlineData").is_some_and(|v| !v.is_null());
                            let has_file_data = part.get("fileData").is_some_and(|v| !v.is_null());

                            // 未知/未来 Gemini part 类型：保守视为有效，避免误判直通响应无效。
                            let has_unknown_nonempty_part = part.as_object().is_some_and(|obj| {
                                obj.iter().any(|(key, value)| {
                                    key != "text"
                                        && key != "functionCall"
                                        && key != "inlineData"
                                        && key != "fileData"
                                        && !value.is_null()
                                })
                            });

                            has_text
                                || has_function_call
                                || has_inline_data
                                || has_file_data
                                || has_unknown_nonempty_part
                        })
                    })
            })
        })
}

/// 校验直通的 Claude 原生响应是否有有效输出。
/// Claude 响应形如 `{"content":[{"type":"text"|"tool_use"|"thinking", ...}], ...}`，
/// 与 OpenAI 的 `choices[]` 结构不同，需独立判断。
fn claude_response_has_valid_output(body: &Value) -> bool {
    body.get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") | Some("thinking") => block
                        .get("text")
                        .or_else(|| block.get("thinking"))
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty()),
                    Some("tool_use") => true,
                    // 未知/未来块类型：保守视为有效，避免误判直通响应无效
                    Some(_) => true,
                    None => false,
                }
            })
        })
}

fn request_uses_structured_output(body: &Value) -> bool {
    body.get("response_format").is_some()
}

/// 递归扫描 body 的任一位置是否存在 tool calling 相关字段。
///
/// **保留但不在生产路径使用**——历史上 `should_append_model_info` 曾用此函数
/// 在请求侧屏蔽 model 注入（commit 3f5825d），但这会误屏蔽所有 agent 客户端
/// （Cursor / Claude Code 等几乎每次请求都带 `tools` 字段，即使本轮不实际调用）。
/// 现在请求侧检查已撤销，真正的 tool-calling 屏蔽在响应侧 `has_tool_calls`。
///
/// 函数保留供未来可能的请求侧精细化判断重用，不是死代码。
#[allow(dead_code)]
fn contains_tool_calling_field(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("tools").is_some()
                || map.get("tool_choice").is_some()
                || map.get("functions").is_some()
                || map.get("function_call").is_some()
                || map.get("parallel_tool_calls").is_some()
                || map.get("max_tool_calls").is_some()
                || map.get("tool_calls").is_some()
            {
                return true;
            }

            map.values().any(contains_tool_calling_field)
        }
        Value::Array(items) => items.iter().any(contains_tool_calling_field),
        _ => false,
    }
}

#[allow(dead_code)]
fn request_uses_tool_calling(body: &Value) -> bool {
    contains_tool_calling_field(body)
}

fn should_append_model_info(
    state: &ProxyState,
    body: &Value,
    _caller_kind: &super::middleware::CallerKind,
) -> bool {
    // 调试开关开启时，Responses 调用方也需要可见模型标记。
    // 直通流走 Responses 原生 output_text.delta 注入；非直通转换路径仍由下游边界决定。
    let setting_enabled = state
        .settings
        .try_read()
        .map(|settings| settings.show_conversation_model)
        .unwrap_or(true);

    // 结构化输出（response_format=json_object/json_schema）必须屏蔽注入：
    // 正文会被客户端当成 JSON 解析，追加 "model: xxx" 会破坏 JSON 合法性。
    //
    // 工具调用上下文不在请求侧屏蔽：agent 客户端（Cursor、Claude Code 等）
    // 几乎每次请求都带 `tools` 字段声明可用工具，即使本轮不实际调用。
    // 如果在请求侧一律屏蔽，大部分 agent 场景永远看不到模型名。
    // 真正的屏蔽发生在响应侧 `has_tool_calls`（参见 build_streaming_response
    // 和 append_and_parse_sse / transform_sse_chunk 的 gate 条件）：
    // 只有上游实际返回 tool_calls / function_call 时才跳过注入，避免 CALL
    // 循环里累积多个 `model: xxx`。
    setting_enabled && !request_uses_structured_output(body)
}

fn build_streaming_response(
    state: &ProxyState,
    entry: &ApiEntry,
    access_key: Option<&AccessKey>,
    requested_model: &str,
    upstream_url: &str,
    response: reqwest::Response,
    status_code: i32,
    needs_transform: bool,
    adapter: Box<dyn super::protocol::ProtocolAdapter + Send + Sync>,
    request_start: std::time::Instant,
    prior_attempts: Vec<AttemptInfo>,
    append_model_info: bool,
    passthrough_header: Option<&str>,
    raw_protocol: Option<Value>,
    middleware: &[Arc<dyn super::middleware::ForwarderMiddleware>],
    ctx: &RequestContext,
) -> axum::response::Response {
    let response_headers = response.headers().clone();
    let upstream_url = upstream_url.to_string();
    let start = request_start;
    let middleware = middleware.to_vec();
    let ctx = Arc::new(ctx.clone());
    let db = state.db.clone();
    let app_handle = state.app_handle.clone();
    let entry = entry.clone();
    let access_key = access_key.cloned();
    let requested_model = requested_model.to_string();
    let first_token_ms = Arc::new(AtomicI64::new(0));
    let prompt_tokens = Arc::new(AtomicI64::new(0));
    let completion_tokens = Arc::new(AtomicI64::new(0));
    let has_sse_error = Arc::new(AtomicBool::new(false));
    let chunk_count = Arc::new(AtomicI64::new(0));
    let streamed_bytes = Arc::new(AtomicI64::new(0));
    let has_text_delta = Arc::new(AtomicBool::new(false));
    let has_tool_calls = Arc::new(AtomicBool::new(false));
    let seen_first_chunk = Arc::new(AtomicBool::new(false));
    let logged = Arc::new(AtomicBool::new(false));
    let model_info = model_info_label(entry.model.as_str(), entry.channel_name.as_deref());

    // 判定协议类型（用于流式处理分支）
    // 通过响应头反查配置，避免硬编码字符串
    let passthrough_protocol = passthrough_header.and_then(passthrough_config_from_header);
    let is_responses_passthrough = passthrough_protocol
        .map(|c| matches!(c.caller_kind, CallerKind::Responses))
        .unwrap_or(false);
    let is_claude_passthrough = passthrough_protocol
        .map(|c| matches!(c.caller_kind, CallerKind::ClaudeMessages))
        .unwrap_or(false);
    let mut sse_buffer = String::new();
    let mut sse_utf8_remainder: Vec<u8> = Vec::new();
    let mut raw_stream_buffer = String::new();
    let mut raw_stream_utf8_remainder: Vec<u8> = Vec::new();
    let mut done_state = SseDoneState::default();
    let mut upstream_stream = Box::pin(response.bytes_stream());
    let mut idle_timeout = Box::pin(sleep(STREAMING_IDLE_TIMEOUT));
    let _ping_interval = Box::pin(sleep(STREAMING_PING_INTERVAL));
    let entry_id = entry.id.clone();
    let circuit_breakers = state.circuit_breakers.clone();
    let failure_counts = state.failure_counts.clone();
    let settings_cache = state.settings.clone();
    let entries_app_handle = state.app_handle.clone();
    let success_circuit_breakers = state.circuit_breakers.clone();
    let success_failure_counts = state.failure_counts.clone();

    // 读取 settings 中的冷却时长，用于空流检测冷却
    let empty_stream_cooldown_secs: i64 = state
        .settings
        .try_read()
        .map(|s| s.circuit_recovery_secs.max(1))
        .unwrap_or(300);

    // Guard captured by the move closure → lives as long as the stream body
    let guard = StreamLogGuard {
        logged: logged.clone(),
        db: db.clone(),
        app_handle: app_handle.clone(),
        access_key: access_key.clone(),
        entry: entry.clone(),
        requested_model: requested_model.clone(),
        prompt_tokens: prompt_tokens.clone(),
        completion_tokens: completion_tokens.clone(),
        has_text_delta: has_text_delta.clone(),
        has_tool_calls: has_tool_calls.clone(),
        first_token_ms: first_token_ms.clone(),
        chunk_count: chunk_count.clone(),
        streamed_bytes: streamed_bytes.clone(),
        status_code,
        start,
        prior_attempts: prior_attempts.clone(),
        upstream_url: upstream_url.clone(),
        response_headers: response_headers.clone(),
        raw_protocol: raw_protocol.clone(),
        empty_stream_cooldown_secs,
    };

    let body_stream =
        futures::stream::poll_fn(move |cx| -> Poll<Option<Result<Bytes, std::io::Error>>> {
            let _ = &guard; // keep guard alive in the closure's capture list

            // Temporarily disable downstream SSE ping injection.
            // Some clients do not correctly ignore SSE comment frames like `: PING\n\n`
            // and may concatenate them into JSON payloads, causing parse errors.
            // if ping_interval.as_mut().poll(cx).is_ready() {
            //     ping_interval.as_mut().reset(tokio::time::Instant::now() + STREAMING_PING_INTERVAL);
            //     return Poll::Ready(Some(Ok(Bytes::from_static(b": PING\n\n"))));
            // }

            if idle_timeout.as_mut().poll(cx).is_ready() {
                if !logged.swap(true, Ordering::SeqCst) {
                    let attempt_path = attempt_path_with_current(
                        &prior_attempts,
                        &entry,
                        504,
                        false,
                        Some("stream idle timeout".to_string()),
                    );
                    let db2 = db.clone();
                    let ah2 = app_handle.clone();
                    let ak2 = access_key.clone();
                    let e2 = entry.clone();
                    let rm2 = requested_model.clone();
                    let pt = prompt_tokens.load(Ordering::SeqCst);
                    let ct = completion_tokens.load(Ordering::SeqCst);
                    let ft = first_token_ms.load(Ordering::SeqCst);
                    let lat = start.elapsed().as_millis() as i64;
                    let has_text_output = has_text_delta.load(Ordering::SeqCst);
                    let has_tool_output = has_tool_calls.load(Ordering::SeqCst);
                    let raw_protocol_log = if should_persist_raw_protocol_for_log(
                        false,
                        pt,
                        ct,
                        has_text_output,
                        has_tool_output,
                    ) {
                        let raw_stream_snapshot =
                            raw_protocol.as_ref().map(|_| raw_stream_buffer.clone());
                        raw_protocol.clone().map(|raw| {
                            with_raw_protocol_stream_capture(
                                raw,
                                504,
                                &response_headers,
                                "stream_idle_timeout",
                                Some(StreamEndReason::Timeout),
                                "stream idle timeout",
                                raw_stream_snapshot.as_deref(),
                            )
                        })
                    } else {
                        None
                    };
                    tokio::spawn(async move {
                        log_usage(
                            &db2,
                            &ah2,
                            ak2.as_ref(),
                            &e2,
                            &rm2,
                            true,
                            pt,
                            ct,
                            0,
                            ft,
                            lat,
                            504,
                            false,
                            Some("stream idle timeout"),
                            Some(attempt_path.as_str()),
                            Some(StreamEndReason::Timeout),
                            raw_protocol_log.as_ref(),
                        );
                    });
                    spawn_cool_down_entry(
                        circuit_breakers.clone(),
                        failure_counts.clone(),
                        settings_cache.clone(),
                        db.clone(),
                        entries_app_handle.clone(),
                        entry_id.clone(),
                    );
                }
                return Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "stream idle timeout",
                ))));
            }

            match upstream_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    if raw_protocol.is_some() {
                        super::sse::append_utf8_safe(
                            &mut raw_stream_buffer,
                            &mut raw_stream_utf8_remainder,
                            &chunk,
                        );
                    }
                    idle_timeout
                        .as_mut()
                        .reset(tokio::time::Instant::now() + STREAMING_IDLE_TIMEOUT);
                    if !seen_first_chunk.swap(true, Ordering::SeqCst) {
                        first_token_ms.store(start.elapsed().as_millis() as i64, Ordering::SeqCst);
                    }
                    chunk_count.fetch_add(1, Ordering::SeqCst);
                    streamed_bytes.fetch_add(chunk.len() as i64, Ordering::SeqCst);

                    if super::sse::stream_buffer_exceeded(
                        &sse_buffer,
                        &sse_utf8_remainder,
                        streamed_bytes.load(Ordering::SeqCst) as usize,
                    ) {
                        if !logged.swap(true, Ordering::SeqCst) {
                            let attempt_path = attempt_path_with_current(
                                &prior_attempts,
                                &entry,
                                413,
                                false,
                                Some("stream buffer exceeds 10MB limit".to_string()),
                            );
                            let db2 = db.clone();
                            let ah2 = app_handle.clone();
                            let ak2 = access_key.clone();
                            let e2 = entry.clone();
                            let rm2 = requested_model.clone();
                            let pt = prompt_tokens.load(Ordering::SeqCst);
                            let ct = completion_tokens.load(Ordering::SeqCst);
                            let ft = first_token_ms.load(Ordering::SeqCst);
                            let lat = start.elapsed().as_millis() as i64;
                            let has_text_output = has_text_delta.load(Ordering::SeqCst);
                            let has_tool_output = has_tool_calls.load(Ordering::SeqCst);
                            let raw_protocol_log = if should_persist_raw_protocol_for_log(
                                false,
                                pt,
                                ct,
                                has_text_output,
                                has_tool_output,
                            ) {
                                let raw_stream_snapshot =
                                    raw_protocol.as_ref().map(|_| raw_stream_buffer.clone());
                                raw_protocol.clone().map(|raw| {
                                    with_raw_protocol_stream_capture(
                                        raw,
                                        413,
                                        &response_headers,
                                        "stream_buffer_exceeded",
                                        Some(StreamEndReason::Dropped),
                                        "stream buffer exceeds 10MB limit",
                                        raw_stream_snapshot.as_deref(),
                                    )
                                })
                            } else {
                                None
                            };
                            tokio::spawn(async move {
                                log_usage(
                                    &db2,
                                    &ah2,
                                    ak2.as_ref(),
                                    &e2,
                                    &rm2,
                                    true,
                                    pt,
                                    ct,
                                    0,
                                    ft,
                                    lat,
                                    413,
                                    false,
                                    Some("stream buffer exceeds 10MB limit"),
                                    Some(attempt_path.as_str()),
                                    Some(StreamEndReason::Dropped),
                                    raw_protocol_log.as_ref(),
                                );
                            });
                            spawn_cool_down_entry(
                                circuit_breakers.clone(),
                                failure_counts.clone(),
                                settings_cache.clone(),
                                db.clone(),
                                entries_app_handle.clone(),
                                entry_id.clone(),
                            );
                        }
                        return Poll::Ready(Some(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "stream buffer exceeds 10MB limit",
                        ))));
                    }

                    if needs_transform {
                        if let Some(transformed) = transform_sse_chunk(
                            &chunk,
                            &mut sse_buffer,
                            &mut sse_utf8_remainder,
                            &adapter,
                            &prompt_tokens,
                            &completion_tokens,
                            &has_sse_error,
                            &has_text_delta,
                            &has_tool_calls,
                            append_model_info.then_some(model_info.as_str()),
                            &mut done_state,
                        ) {
                            if let Ok(mut chunk_text) = String::from_utf8(transformed.to_vec()) {
                                for mw in &middleware {
                                    mw.on_sse_chunk(&mut chunk_text, ctx.as_ref());
                                }
                                return Poll::Ready(Some(Ok(Bytes::from(chunk_text))));
                            }
                            return Poll::Ready(Some(Ok(transformed)));
                        } else {
                            cx.waker().wake_by_ref();
                            return Poll::Pending;
                        }
                    } else if is_responses_passthrough {
                        // Responses 原生直通流：扫描事件，在 response.completed 前注入一次
                        // output_text.delta 模型标记。开关关闭、工具调用或无文本输出时不注入。
                        let saw_completed = scan_responses_passthrough_chunk(
                            &chunk,
                            &has_sse_error,
                            &has_text_delta,
                            &has_tool_calls,
                            &prompt_tokens,
                            &completion_tokens,
                        );
                        let should_inject = should_inject_responses_model_info(
                            append_model_info,
                            saw_completed,
                            has_text_delta.load(Ordering::Relaxed),
                            has_tool_calls.load(Ordering::Relaxed),
                            done_state.appended_model_info,
                        );
                        if should_inject {
                            done_state.appended_model_info = true;
                            return Poll::Ready(Some(Ok(inject_responses_model_info_chunk(
                                &chunk,
                                model_info.as_str(),
                            ))));
                        }
                        return Poll::Ready(Some(Ok(chunk)));
                    } else if is_claude_passthrough {
                        // Claude 原生直通流：扫描事件，在 message_stop 前注入一次
                        // 模型名（Anthropic 格式）。有工具调用则跳过，防 CALL 循环刷屏。
                        let saw_stop = scan_anthropic_passthrough_chunk(
                            &chunk,
                            &has_text_delta,
                            &has_tool_calls,
                            &prompt_tokens,
                            &completion_tokens,
                        );
                        let should_inject = append_model_info
                            && saw_stop
                            && has_text_delta.load(Ordering::Relaxed)
                            && !has_tool_calls.load(Ordering::Relaxed)
                            && !done_state.appended_model_info;
                        let chunk = normalize_reasoning_in_sse_chunk(&chunk).unwrap_or(chunk);
                        if should_inject {
                            done_state.appended_model_info = true;
                            // 在 message_stop 事件之前插入模型名 block
                            let marker = b"event: message_stop";
                            if let Some(pos) = chunk.windows(marker.len()).position(|w| w == marker)
                            {
                                let mut out = Vec::with_capacity(chunk.len() + 256);
                                out.extend_from_slice(&chunk[..pos]);
                                out.extend_from_slice(&anthropic_model_info_events(
                                    model_info.as_str(),
                                ));
                                out.extend_from_slice(&chunk[pos..]);
                                return Poll::Ready(Some(Ok(Bytes::from(out))));
                            }
                            // 没找到 message_stop 文本边界：注入放到 chunk 前
                            let mut out = anthropic_model_info_events(model_info.as_str());
                            out.extend_from_slice(&chunk);
                            return Poll::Ready(Some(Ok(Bytes::from(out))));
                        }
                        return Poll::Ready(Some(Ok(chunk)));
                    } else {
                        if let Some(with_model_info) = append_and_parse_sse(
                            &mut sse_buffer,
                            &mut sse_utf8_remainder,
                            &chunk,
                            &prompt_tokens,
                            &completion_tokens,
                            &has_sse_error,
                            &has_text_delta,
                            &has_tool_calls,
                            append_model_info.then_some(model_info.as_str()),
                            &mut done_state,
                        ) {
                            // Normalize reasoning fields in model-info-injected chunk
                            let with_model_info =
                                normalize_reasoning_in_sse_chunk(&with_model_info)
                                    .unwrap_or(with_model_info);
                            if let Ok(mut chunk_text) = String::from_utf8(with_model_info.to_vec())
                            {
                                for mw in &middleware {
                                    mw.on_sse_chunk(&mut chunk_text, ctx.as_ref());
                                }
                                return Poll::Ready(Some(Ok(Bytes::from(chunk_text))));
                            }
                            return Poll::Ready(Some(Ok(with_model_info)));
                        }
                        // Normalize reasoning fields in raw SSE chunk (passthrough)
                        let chunk = normalize_reasoning_in_sse_chunk(&chunk).unwrap_or(chunk);
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                }
                Poll::Ready(Some(Err(err))) => {
                    if !logged.swap(true, Ordering::SeqCst) {
                        let chunk_total = chunk_count.load(Ordering::SeqCst);
                        let byte_total = streamed_bytes.load(Ordering::SeqCst);
                        let ft = first_token_ms.load(Ordering::SeqCst);
                        let lat = start.elapsed().as_millis() as i64;
                        let pt = prompt_tokens.load(Ordering::SeqCst);
                        let ct = completion_tokens.load(Ordering::SeqCst);
                        let has_text_output = has_text_delta.load(Ordering::SeqCst);
                        let has_tool_output = has_tool_calls.load(Ordering::SeqCst);
                        let has_error = has_sse_error.load(Ordering::SeqCst);
                        let suppress_cooldown = is_decode_timeout_after_stream_started(
                            &err,
                            status_code,
                            ft,
                            chunk_total,
                            byte_total,
                            has_text_output,
                            has_tool_output,
                            ct,
                            has_error,
                        );
                        let stream_end_reason = if suppress_cooldown {
                            StreamEndReason::DecodeTimeout
                        } else {
                            StreamEndReason::UpstreamError
                        };
                        let diagnostic = build_stream_diagnostic(
                            "stream_read",
                            Some(&err),
                            &entry,
                            &upstream_url,
                            status_code,
                            &response_headers,
                            chunk_total,
                            byte_total,
                            sse_buffer.len(),
                            ft,
                            lat,
                            pt,
                            ct,
                            has_text_output,
                            has_tool_output,
                            has_error,
                            Some(false),
                        );
                        let error_message = format!("Stream error: {err}\n{diagnostic}");
                        let attempt_path = attempt_path_with_current(
                            &prior_attempts,
                            &entry,
                            502,
                            false,
                            Some(error_message.clone()),
                        );
                        let db2 = db.clone();
                        let ah2 = app_handle.clone();
                        let ak2 = access_key.clone();
                        let e2 = entry.clone();
                        let rm2 = requested_model.clone();
                        let keyword_settings = settings_cache.clone();
                        let raw_protocol_log = if should_persist_raw_protocol_for_log(
                            false,
                            pt,
                            ct,
                            has_text_output,
                            has_tool_output,
                        ) {
                            let raw_stream_snapshot =
                                raw_protocol.as_ref().map(|_| raw_stream_buffer.clone());
                            raw_protocol.clone().map(|raw| {
                                with_raw_protocol_stream_capture(
                                    raw,
                                    502,
                                    &response_headers,
                                    "stream_read_error",
                                    Some(stream_end_reason),
                                    error_message.as_str(),
                                    raw_stream_snapshot.as_deref(),
                                )
                            })
                        } else {
                            None
                        };
                        tokio::spawn(async move {
                            log_usage(
                                &db2,
                                &ah2,
                                ak2.as_ref(),
                                &e2,
                                &rm2,
                                true,
                                pt,
                                ct,
                                0,
                                ft,
                                lat,
                                502,
                                false,
                                Some(error_message.as_str()),
                                Some(attempt_path.as_str()),
                                Some(stream_end_reason),
                                raw_protocol_log.as_ref(),
                            );
                            let settings = keyword_settings.read().await.clone();
                            if should_record_keyword_candidate(502, &error_message, &settings) {
                                keyword_log::record_unknown_failure(502, &error_message).await;
                            }
                        });
                        if !suppress_cooldown {
                            spawn_cool_down_entry(
                                circuit_breakers.clone(),
                                failure_counts.clone(),
                                settings_cache.clone(),
                                db.clone(),
                                entries_app_handle.clone(),
                                entry_id.clone(),
                            );
                        }
                    }
                    Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        err,
                    ))))
                }
                Poll::Ready(None) => {
                    if !logged.swap(true, Ordering::SeqCst) {
                        let pt = prompt_tokens.load(Ordering::SeqCst);
                        let ct = completion_tokens.load(Ordering::SeqCst);
                        let has_error = has_sse_error.load(Ordering::SeqCst);
                        let chunk_total = chunk_count.load(Ordering::SeqCst);
                        let byte_total = streamed_bytes.load(Ordering::SeqCst);
                        let ft = first_token_ms.load(Ordering::SeqCst);
                        let sc = status_code;
                        let has_text_output = has_text_delta.load(Ordering::SeqCst);
                        let has_tool_output = has_tool_calls.load(Ordering::SeqCst);
                        let success = is_completed_stream_success(
                            sc,
                            has_error,
                            has_text_output,
                            has_tool_output,
                            ct,
                        );
                        let failure_reason = if success {
                            None
                        } else if has_error {
                            Some("upstream stream completed with SSE error".to_string())
                        } else {
                            Some("upstream stream completed without valid output".to_string())
                        };
                        let attempt_path = attempt_path_with_current(
                            &prior_attempts,
                            &entry,
                            status_code,
                            success,
                            failure_reason.clone(),
                        );
                        let stream_summary = build_stream_diagnostic(
                            "stream_complete",
                            None,
                            &entry,
                            &upstream_url,
                            status_code,
                            &response_headers,
                            chunk_total,
                            byte_total,
                            sse_buffer.len(),
                            ft,
                            start.elapsed().as_millis() as i64,
                            pt,
                            ct,
                            has_text_output,
                            has_tool_output,
                            has_error,
                            Some(success),
                        );
                        let log_message = failure_reason
                            .as_ref()
                            .map(|reason| format!("{reason}\n{stream_summary}"))
                            .unwrap_or(stream_summary);
                        let db2 = db.clone();
                        let ah2 = app_handle.clone();
                        let ak2 = access_key.clone();
                        let e2 = entry.clone();
                        let rm2 = requested_model.clone();
                        let ft = first_token_ms.load(Ordering::SeqCst);
                        let lat = start.elapsed().as_millis() as i64;
                        let sc = status_code;
                        let scb = success_circuit_breakers.clone();
                        let sfc = success_failure_counts.clone();
                        let eid = entry_id.clone();
                        let sdb = settings_cache.clone();
                        let keyword_settings = settings_cache.clone();
                        let eah = entries_app_handle.clone();
                        let raw_protocol_log = if should_persist_raw_protocol_for_log(
                            success,
                            pt,
                            ct,
                            has_text_output,
                            has_tool_output,
                        ) {
                            let raw_stream_snapshot =
                                raw_protocol.as_ref().map(|_| raw_stream_buffer.clone());
                            raw_protocol.clone().map(|raw| {
                                with_raw_protocol_stream_capture(
                                    raw,
                                    sc,
                                    &response_headers,
                                    "stream_complete",
                                    Some(StreamEndReason::Done),
                                    log_message.as_str(),
                                    raw_stream_snapshot.as_deref(),
                                )
                            })
                        } else {
                            None
                        };
                        tokio::spawn(async move {
                            log_usage(
                                &db2,
                                &ah2,
                                ak2.as_ref(),
                                &e2,
                                &rm2,
                                true,
                                pt,
                                ct,
                                0,
                                ft,
                                lat,
                                sc,
                                success,
                                Some(log_message.as_str()),
                                Some(attempt_path.as_str()),
                                Some(StreamEndReason::Done),
                                raw_protocol_log.as_ref(),
                            );
                            if success {
                                spawn_record_circuit_success(scb, sfc, sdb, db2.clone(), eah, eid);
                            } else {
                                let settings = keyword_settings.read().await.clone();
                                let status_for_log = if sc > 0 { sc as u16 } else { 0 };
                                if should_record_keyword_candidate(status_for_log, &log_message, &settings) {
                                    keyword_log::record_unknown_failure(status_for_log, &log_message).await;
                                }
                                spawn_cool_down_entry(scb, sfc, sdb, db2.clone(), eah, eid);
                            }
                        });
                        if let Some(reason) = failure_reason {
                            return Poll::Ready(Some(Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                reason,
                            ))));
                        }
                    }
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            }
        });

    axum::http::Response::builder()
        .status(
            axum::http::StatusCode::from_u16(status_code as u16)
                .unwrap_or(axum::http::StatusCode::OK),
        )
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(body_stream))
        .unwrap()
}

fn stream_chunk_has_text_delta(value: &Value) -> bool {
    let has_chat_text = value
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|choice| {
            choice
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty())
        });
    if has_chat_text {
        return true;
    }

    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "response.output_text.delta"
                | "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta"
        )
    ) && value
        .get("delta")
        .and_then(Value::as_str)
        .is_some_and(|delta| !delta.is_empty())
}

/// 在 message / delta 级别归一已有的 reasoning 等价字段。
/// - 有 `reasoning_content` 无 `reasoning_text` → 补 `reasoning_text`
/// - 有 `reasoning_text` 无 `reasoning_content` → 补 `reasoning_content`
/// - 有 `reasoning_details` 无 `reasoning_content` → 补 `reasoning_content`
///
/// 只翻译已存在的信息，不缓存、不回放、不凭空生成 reasoning 历史。
fn normalize_reasoning_fields(value: &mut Value) {
    let obj = match value.as_object_mut() {
        Some(obj) => obj,
        None => return,
    };
    let canonical = obj
        .get("reasoning_content")
        .cloned()
        .or_else(|| obj.get("reasoning_text").cloned())
        .or_else(|| {
            obj.get("reasoning_details")
                .and_then(Value::as_str)
                .map(|reasoning| Value::String(reasoning.to_string()))
        });

    if let Some(reasoning) = canonical {
        if !obj.contains_key("reasoning_content") {
            obj.insert("reasoning_content".into(), reasoning.clone());
        }
        if !obj.contains_key("reasoning_text") {
            obj.insert("reasoning_text".into(), reasoning);
        }
    }
}

/// 扫描 SSE chunk 字节中的 reasoning delta 等价字段，
/// 补全缺少的对应字段。返回 `Some(modified_bytes)` 如果有任何修改。
fn normalize_reasoning_in_sse_chunk(chunk: &Bytes) -> Option<Bytes> {
    let text = std::str::from_utf8(chunk).ok()?;
    if !text.contains("reasoning_content")
        && !text.contains("reasoning_text")
        && !text.contains("reasoning_details")
    {
        return None;
    }

    let mut output = Vec::with_capacity(chunk.len() + 256);
    let mut changed = false;

    for line in text.split_inclusive('\n') {
        let check = line.trim_end();
        if let Some(payload) = sse_data_payload_for_reasoning(check) {
            if let Ok(mut val) = serde_json::from_str::<Value>(payload) {
                let before = val.clone();
                if let Some(choices) = val.get_mut("choices").and_then(|c| c.as_array_mut()) {
                    for choice in choices {
                        if let Some(delta) = choice.get_mut("delta") {
                            normalize_reasoning_fields(delta);
                        }
                    }
                }
                if val != before {
                    if let Ok(modified) = serde_json::to_string(&val) {
                        output.extend_from_slice(b"data: ");
                        output.extend_from_slice(modified.as_bytes());
                        if line.ends_with("\r\n") {
                            output.extend_from_slice(b"\r\n");
                        } else {
                            output.push(b'\n');
                        }
                        changed = true;
                        continue;
                    }
                }
            }
        }
        output.extend_from_slice(line.as_bytes());
    }

    changed.then_some(Bytes::from(output))
}

fn sse_data_payload_for_reasoning(line: &str) -> Option<&str> {
    line.strip_prefix("data:").map(str::trim_start)
}

fn model_info_label(model: &str, channel_name: Option<&str>) -> String {
    let channel_name = channel_name.unwrap_or("").trim();
    if channel_name.is_empty() || channel_name == "unknown" {
        model.to_string()
    } else {
        format!("{model}/{channel_name}")
    }
}

fn stream_chunk_has_model_info_delta(value: &Value, model: &str) -> bool {
    value
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|choice| {
            choice
                .get("delta")
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
                .is_some_and(|content| {
                    let trimmed = content.trim();
                    trimmed == format!("model: {model}") || trimmed == format!("模型: {model}")
                })
        })
}

/// 检测 OpenAI 兼容 chunk 是否包含函数/工具调用信号。
/// 一旦响应中存在工具调用，本轮就是 CALL 中间态，不应附加模型信息，
/// 否则下游循环复用时会把 `model: ...` 当成对话内容重复显示。
fn stream_chunk_has_tool_calls(value: &Value) -> bool {
    let has_responses_tool = matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.function_call_arguments.delta")
    ) || (value.get("type").and_then(Value::as_str)
        == Some("response.output_item.added")
        && value
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            == Some("function_call"));
    if has_responses_tool {
        return true;
    }

    value
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|choice| {
            let delta = choice.get("delta");
            let message = choice.get("message");

            let has_tool_calls = |container: Option<&Value>| {
                container
                    .and_then(|item| item.get("tool_calls"))
                    .and_then(Value::as_array)
                    .is_some_and(|tool_calls| !tool_calls.is_empty())
            };

            let has_function_call = |container: Option<&Value>| {
                container
                    .and_then(|item| item.get("function_call"))
                    .is_some_and(|function_call| !function_call.is_null())
            };

            let finish_reason = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .unwrap_or("");
            let is_tool_finish = matches!(finish_reason, "tool_calls" | "function_call");

            has_tool_calls(delta)
                || has_tool_calls(message)
                || has_function_call(delta)
                || has_function_call(message)
                || is_tool_finish
        })
}

fn model_info_delta(model: &str) -> Vec<u8> {
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let payload = serde_json::json!({
        "id": format!("chatcmpl-api-switch-model-info-{created}"),
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "content": format!("\n\nmodel: {model}")
            },
            "finish_reason": null
        }]
    });
    format!("data: {payload}\n\n").into_bytes()
}

/// 为 Responses 同协议直通流生成“模型名”注入事件（Responses 原生 SSE 格式）。
///
/// 不依赖下游展示顶层 `response.model`，而是追加一段 output_text.delta，
/// 让开启调试开关的用户能在正文里看到本轮 API Switch 实际命中的条目。
fn responses_model_info_event(model: &str) -> Vec<u8> {
    let payload = serde_json::json!({
        "type": "response.output_text.delta",
        "delta": format!("\n\nmodel: {model}"),
    });
    format!("event: response.output_text.delta\ndata: {payload}\n\n").into_bytes()
}

fn should_inject_responses_model_info(
    append_model_info: bool,
    saw_completed: bool,
    has_text_delta: bool,
    has_tool_calls: bool,
    already_appended: bool,
) -> bool {
    append_model_info && saw_completed && has_text_delta && !has_tool_calls && !already_appended
}

fn inject_responses_model_info_chunk(chunk: &Bytes, model_info: &str) -> Bytes {
    let marker = b"event: response.completed";
    if let Some(pos) = chunk.windows(marker.len()).position(|w| w == marker) {
        let mut out = Vec::with_capacity(chunk.len() + 256 + model_info.len());
        out.extend_from_slice(&chunk[..pos]);
        out.extend_from_slice(&responses_model_info_event(model_info));
        out.extend_from_slice(&chunk[pos..]);
        return Bytes::from(out);
    }

    let mut out = responses_model_info_event(model_info);
    out.extend_from_slice(chunk);
    Bytes::from(out)
}

/// 为 Claude 同协议直通流生成"模型名"注入事件（Anthropic 原生 SSE 格式）。
///
/// AUTO 路由下用户需要知道实际命中的模型。直通流是 Anthropic 原生格式，
/// OpenAI 的 `model_info_delta` 不兼容，故用 content_block 三件套
/// （start→delta→stop）追加一段 `\n\nmodel: xxx` 文本，客户端按正文显示。
/// 使用一个高 index（99）独立 block，避免与上游 content block 冲突。
fn anthropic_model_info_events(model: &str) -> Vec<u8> {
    let idx = 99;
    let start = serde_json::json!({
        "type": "content_block_start",
        "index": idx,
        "content_block": {"type": "text", "text": ""}
    });
    let delta = serde_json::json!({
        "type": "content_block_delta",
        "index": idx,
        "delta": {"type": "text_delta", "text": format!("\n\nmodel: {model}")}
    });
    let stop = serde_json::json!({"type": "content_block_stop", "index": idx});
    format!(
        "event: content_block_start\ndata: {start}\n\nevent: content_block_delta\ndata: {delta}\n\nevent: content_block_stop\ndata: {stop}\n\n"
    )
    .into_bytes()
}

/// Anthropic 的 prompt 规模 = input_tokens + cache_read + cache_creation。
/// 高缓存命中时 input_tokens 可能为 0，必须把缓存 token 计入，否则 IN TOKEN 显示 0。
fn anthropic_prompt_tokens(usage: Option<&Value>) -> i64 {
    let Some(u) = usage else { return 0 };
    let get = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
    get("input_tokens") + get("cache_read_input_tokens") + get("cache_creation_input_tokens")
}

/// 直通流字节里查找 Anthropic 事件：判断是否含工具调用 / 已含文本 / 到达
/// message_stop，并提取 usage token（Anthropic 把 input_tokens 放在
/// message_start 的 /message/usage，output_tokens 放在 message_delta 顶层 usage）。
fn scan_anthropic_passthrough_chunk(
    chunk: &Bytes,
    has_text: &Arc<AtomicBool>,
    has_tool: &Arc<AtomicBool>,
    prompt_tokens: &Arc<AtomicI64>,
    completion_tokens: &Arc<AtomicI64>,
) -> bool {
    let text = String::from_utf8_lossy(chunk);
    let mut saw_message_stop = false;
    for line in text.lines() {
        let Some(payload) = crate::proxy::sse::sse_data_payload(line) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                // Anthropic 的 prompt 成本分散在 input_tokens + 缓存 token。
                // 高缓存命中时 input_tokens 可能为 0，但 cache_read/creation 很大，
                // 真实 prompt 规模 = 三者之和（否则 IN TOKEN 显示 0，误导）。
                let u = value.pointer("/message/usage");
                let prompt = anthropic_prompt_tokens(u);
                if prompt > 0 {
                    prompt_tokens.store(prompt, Ordering::Relaxed);
                }
                if let Some(c) = u
                    .and_then(|v| v.get("output_tokens"))
                    .and_then(Value::as_i64)
                {
                    if c > 0 {
                        completion_tokens.store(c, Ordering::Relaxed);
                    }
                }
            }
            Some("content_block_start") => {
                if value
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    == Some("tool_use")
                {
                    has_tool.store(true, Ordering::Relaxed);
                }
            }
            Some("content_block_delta") => {
                if let Some(dt) = value
                    .get("delta")
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                {
                    if dt == "text_delta" {
                        has_text.store(true, Ordering::Relaxed);
                    }
                }
            }
            Some("message_delta") => {
                // output_tokens 在 message_delta 顶层 usage
                if let Some(c) = value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_i64)
                {
                    if c > 0 {
                        completion_tokens.store(c, Ordering::Relaxed);
                    }
                }
                // prompt（含缓存 token）兜底：部分中转在 message_delta 才给全
                let prompt = anthropic_prompt_tokens(value.get("usage"));
                if prompt > 0 {
                    prompt_tokens.store(prompt, Ordering::Relaxed);
                }
            }
            Some("message_stop") => saw_message_stop = true,
            _ => {}
        }
    }
    saw_message_stop
}

fn scan_responses_passthrough_chunk(
    chunk: &Bytes,
    has_sse_error: &Arc<AtomicBool>,
    has_text: &Arc<AtomicBool>,
    has_tool: &Arc<AtomicBool>,
    prompt_tokens: &Arc<AtomicI64>,
    completion_tokens: &Arc<AtomicI64>,
) -> bool {
    let Ok(text) = std::str::from_utf8(chunk) else {
        return false;
    };
    let mut saw_completed = false;

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some(payload) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        else {
            continue;
        };
        let payload = payload.trim_start();
        if payload == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };

        if let Some(err) = value.get("error") {
            if !err.is_null() {
                has_sse_error.store(true, Ordering::Relaxed);
            }
        }
        if stream_chunk_has_text_delta(&value) {
            has_text.store(true, Ordering::Relaxed);
        }
        if stream_chunk_has_tool_calls(&value) {
            has_tool.store(true, Ordering::Relaxed);
        }
        let (prompt, completion, _reasoning) = extract_usage_tokens(&value);
        if prompt > 0 {
            prompt_tokens.store(prompt, Ordering::Relaxed);
        }
        if completion > 0 {
            completion_tokens.store(completion, Ordering::Relaxed);
        }
        if value.get("type").and_then(Value::as_str) == Some("response.completed") {
            saw_completed = true;
        }
    }

    saw_completed
}

fn transform_sse_chunk(
    chunk: &Bytes,
    buffer: &mut String,
    remainder: &mut Vec<u8>,
    adapter: &Box<dyn super::protocol::ProtocolAdapter + Send + Sync>,
    prompt_tokens: &Arc<AtomicI64>,
    completion_tokens: &Arc<AtomicI64>,
    has_sse_error: &Arc<AtomicBool>,
    has_text_delta: &Arc<AtomicBool>,
    has_tool_calls: &Arc<AtomicBool>,
    model_info: Option<&str>,
    done_state: &mut SseDoneState,
) -> Option<Bytes> {
    super::sse::append_utf8_safe(buffer, remainder, chunk);
    let mut output = Vec::new();

    while let Some(line_end) = buffer.find('\n') {
        let mut line = buffer.drain(..=line_end).collect::<String>();
        if line.ends_with('\n') {
            line.pop();
        }
        if line.ends_with('\r') {
            line.pop();
        }

        if let Some(payload) = line.strip_prefix("data: ") {
            if payload == "[DONE]" {
                if !done_state.seen_done {
                    done_state.seen_done = true;
                    if let Some(model) = model_info.filter(|_| {
                        has_text_delta.load(Ordering::Relaxed)
                            && !has_tool_calls.load(Ordering::Relaxed)
                            && !done_state.appended_model_info
                            && !done_state.upstream_model_info_seen
                    }) {
                        output.push(model_info_delta(model));
                        done_state.appended_model_info = true;
                    }
                    output.push(b"data: [DONE]\n\n".to_vec());
                }
                continue;
            }

            let (prompt, completion) = adapter.extract_sse_usage(payload);
            if prompt > 0 {
                prompt_tokens.store(prompt, Ordering::Relaxed);
            }
            if completion > 0 {
                completion_tokens.store(completion, Ordering::Relaxed);
            }

            if let Some(transformed) = adapter.transform_sse_line(payload) {
                if let Ok(mut value) = serde_json::from_str::<Value>(&transformed) {
                    if let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) {
                        for choice in choices {
                            if let Some(delta) = choice.get_mut("delta") {
                                normalize_reasoning_fields(delta);
                            }
                        }
                    }
                    if stream_chunk_has_text_delta(&value) {
                        has_text_delta.store(true, Ordering::Relaxed);
                    }
                    if stream_chunk_has_tool_calls(&value) {
                        has_tool_calls.store(true, Ordering::Relaxed);
                    }
                    if let Some(err) = value.get("error") {
                        if !err.is_null() {
                            has_sse_error.store(true, Ordering::Relaxed);
                        }
                    }
                    if let Some(model) = model_info {
                        if stream_chunk_has_model_info_delta(&value, model) {
                            done_state.upstream_model_info_seen = true;
                        }
                    }
                    if let Ok(normalized) = serde_json::to_string(&value) {
                        output.push(format!("data: {normalized}\n\n").into_bytes());
                    }
                } else {
                    output.push(format!("data: {transformed}\n\n").into_bytes());
                }
            }
        }
    }

    if output.is_empty() {
        None
    } else {
        Some(Bytes::from(output.concat()))
    }
}

fn append_and_parse_sse(
    buffer: &mut String,
    remainder: &mut Vec<u8>,
    chunk: &Bytes,
    prompt_tokens: &Arc<AtomicI64>,
    completion_tokens: &Arc<AtomicI64>,
    has_sse_error: &Arc<AtomicBool>,
    has_text_delta: &Arc<AtomicBool>,
    has_tool_calls: &Arc<AtomicBool>,
    model_info: Option<&str>,
    done_state: &mut SseDoneState,
) -> Option<Bytes> {
    super::sse::append_utf8_safe(buffer, remainder, chunk);
    let mut saw_done = false;

    while let Some(line_end) = buffer.find('\n') {
        let mut line = buffer.drain(..=line_end).collect::<String>();
        if line.ends_with('\n') {
            line.pop();
        }
        if line.ends_with('\r') {
            line.pop();
        }

        if let Some(payload) = line.strip_prefix("data: ") {
            if payload == "[DONE]" {
                if !done_state.seen_done {
                    done_state.seen_done = true;
                    saw_done = true;
                }
                continue;
            }

            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            // 只有 error 值是非 null 对象时才算真正的错误
            // 修复: value.get("error").is_some() 对 "error": null 也会返回 true，导致误判
            if let Some(err) = value.get("error") {
                if !err.is_null() {
                    has_sse_error.store(true, Ordering::Relaxed);
                }
            }
            if stream_chunk_has_text_delta(&value) {
                has_text_delta.store(true, Ordering::Relaxed);
            }
            if stream_chunk_has_tool_calls(&value) {
                has_tool_calls.store(true, Ordering::Relaxed);
            }
            if let Some(model) = model_info {
                if stream_chunk_has_model_info_delta(&value, model) {
                    done_state.upstream_model_info_seen = true;
                }
            }
            let (prompt, completion, _reasoning) = extract_usage_tokens(&value);
            if prompt > 0 {
                prompt_tokens.store(prompt, Ordering::Relaxed);
            }
            if completion > 0 {
                completion_tokens.store(completion, Ordering::Relaxed);
            }
        }
    }

    let Some(model) = model_info.filter(|_| {
        saw_done
            && has_text_delta.load(Ordering::Relaxed)
            && !has_tool_calls.load(Ordering::Relaxed)
            && !done_state.appended_model_info
            && !done_state.upstream_model_info_seen
    }) else {
        return None;
    };

    let marker = b"data: [DONE]";
    let Some(pos) = chunk
        .windows(marker.len())
        .position(|window| window == marker)
    else {
        return None;
    };

    done_state.appended_model_info = true;

    let mut output = Vec::with_capacity(chunk.len() + 66 + model.len());
    let prefix = &chunk[..pos];
    output.extend_from_slice(prefix);
    if !prefix.is_empty() && !prefix.ends_with(b"\n\n") {
        if prefix.ends_with(b"\n") {
            output.extend_from_slice(b"\n");
        } else {
            output.extend_from_slice(b"\n\n");
        }
    }
    output.extend_from_slice(&model_info_delta(model));
    output.extend_from_slice(b"data: [DONE]\n\n");
    Some(Bytes::from(output))
}

fn status_matches_rule(rule: &str, status: u16) -> bool {
    let rule = rule.trim();
    if rule.is_empty() {
        return false;
    }

    if let Some((start, end)) = rule.split_once('-') {
        let Ok(start) = start.trim().parse::<u16>() else {
            return false;
        };
        let Ok(end) = end.trim().parse::<u16>() else {
            return false;
        };
        return status >= start && status <= end;
    }

    rule.parse::<u16>() == Ok(status)
}

fn should_disable_entry_for_status(disable_codes: &str, status: u16) -> bool {
    disable_codes
        .split(',')
        .any(|rule| status_matches_rule(rule, status))
}

fn effective_disable_keywords(settings: &AppSettings) -> &str {
    if settings.disable_keywords.trim().is_empty() {
        "Your credit balance is too low\nThis organization has been disabled.\nYou exceeded your current quota\nPermission denied\nThe security token included in the request is invalid\nOperation not allowed\nYour account is not authorized\ninsufficient_quota\nquota_exceeded_error\ntoken plan limit exhausted\nUpstream rate limit exceeded\ninvalid api key\nUnauthorized - Invalid token"
    } else {
        &settings.disable_keywords
    }
}

fn should_record_keyword_candidate(status: u16, message: &str, settings: &AppSettings) -> bool {
    let disable_by_status = status > 0
        && should_disable_entry_for_status(&settings.circuit_disable_codes, status);
    let disable_by_keyword = status > 0
        && should_disable_entry_for_message(effective_disable_keywords(settings), message);
    !disable_by_status && !disable_by_keyword
}

/// Check if the upstream error message contains any disable keyword.
/// Keywords are separated by newlines and matched case-insensitively.
fn should_disable_entry_for_message(disable_keywords: &str, message: &str) -> bool {
    if disable_keywords.is_empty() || message.is_empty() {
        return false;
    }
    let lower_msg = message.to_lowercase();
    disable_keywords
        .split('\n')
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .any(|keyword| lower_msg.contains(&keyword.to_lowercase()))
}

async fn disable_entry(state: &ProxyState, entry: &ApiEntry) {
    let recovery_secs = state.settings.read().await.circuit_recovery_secs.max(1);
    let cooldown_until = chrono::Utc::now().timestamp() + recovery_secs;

    let _ = state.db.toggle_entry(&entry.id, false);
    let _ = state.db.set_entry_cooldown(&entry.id, Some(cooldown_until));
    if let Some(h) = &state.app_handle {
        crate::event::emit(h, "entries-changed");
    }
    crate::state_version::bump("pool");

    let mut breakers = state.circuit_breakers.write().await;
    breakers.remove(&entry.id);
}

async fn freeze_channel_entries(state: &ProxyState, entry: &ApiEntry) {
    // 限定词通常代表账号、额度、密钥或上游通道级故障；只冷冻同渠道 6 小时，不永久禁用用户开关。
    let cooldown_until = chrono::Utc::now().timestamp() + 21600;
    match state
        .db
        .freeze_entries_for_channel(&entry.channel_id, cooldown_until)
    {
        Ok(entry_ids) => {
            if let Some(h) = &state.app_handle {
                crate::event::emit(h, "entries-changed");
            }
            crate::state_version::bump("pool");

            let mut counts = state.failure_counts.write().await;
            let mut breakers = state.circuit_breakers.write().await;
            for entry_id in &entry_ids {
                counts.remove(entry_id);
                breakers.remove(entry_id);
            }

            log::warn!(
                "Channel {} frozen for 6h after keyword match. Frozen entries: {}.",
                entry.channel_id,
                entry_ids.len()
            );
        }
        Err(err) => {
            log::error!(
                "Failed to freeze channel {} after keyword match: {}",
                entry.channel_id,
                err
            );
        }
    }
}

async fn record_circuit_success(state: &ProxyState, entry_id: &str) {
    let _ = state.db.set_entry_cooldown(entry_id, None);
    if let Some(h) = &state.app_handle {
        crate::event::emit(h, "entries-changed");
    }
    crate::state_version::bump("pool");

    // Clear failure count
    state.failure_counts.write().await.remove(entry_id);

    let mut breakers = state.circuit_breakers.write().await;
    let recovery_secs = state.settings.read().await.circuit_recovery_secs as u64;

    let cb = breakers
        .entry(entry_id.to_string())
        .or_insert_with(|| CircuitBreaker::new(recovery_secs));
    cb.record_success();
}

async fn cool_down_entry(state: &ProxyState, entry: &ApiEntry) {
    let settings = state.settings.read().await.clone();
    let threshold = (settings.circuit_failure_threshold as u32).max(1);
    let recovery_secs_u64 = settings.circuit_recovery_secs.max(1) as u64;

    // Record failure in circuit breaker and get adaptive backoff delay
    let mut breakers = state.circuit_breakers.write().await;
    let cb = breakers
        .entry(entry.id.clone())
        .or_insert_with(|| CircuitBreaker::new(recovery_secs_u64));
    cb.set_recovery_secs(recovery_secs_u64);
    cb.record_failure(threshold);
    let effective_delay = cb.calculate_backoff_delay();
    let is_open = cb.get_state() == crate::proxy::circuit_breaker::CircuitState::Open;
    let open_cycles = cb.consecutive_opens();
    drop(breakers);

    // Increment failure count in memory
    let mut counts = state.failure_counts.write().await;
    let count = counts.entry(entry.id.clone()).or_insert(0);
    *count += 1;
    let current_count = *count;
    drop(counts);

    // Any failure is counted. Before threshold: temporary cooldown.
    // At/above threshold: long-term 6h freeze protection.
    if current_count >= threshold * 2 {
        let six_hours_later = chrono::Utc::now().timestamp() + 21600;
        let _ = state
            .db
            .set_entry_cooldown(&entry.id, Some(six_hours_later));
        let _ = state.db.toggle_entry(&entry.id, false);
        if let Some(h) = &state.app_handle {
            crate::event::emit(h, "entries-changed");
        }
        crate::state_version::bump("pool");

        log::warn!(
            "Entry {} disabled after {} consecutive failures. Long cooldown: 6h.",
            entry.id,
            current_count
        );
        return;
    }

    // Synchronize DB cooldown_until with CircuitBreaker adaptive recovery deadline
    let cooldown_until = chrono::Utc::now().timestamp() + effective_delay as i64;
    let _ = state.db.set_entry_cooldown(&entry.id, Some(cooldown_until));
    if let Some(h) = &state.app_handle {
        crate::event::emit(h, "entries-changed");
    }
    crate::state_version::bump("pool");

    log::warn!(
        "Entry {} circuit state={:?}, adaptive cooldown={}s (open cycle {}), failure count {}/{}.",
        entry.id,
        if is_open { "Open" } else { "Closed" },
        effective_delay,
        open_cycles,
        current_count,
        threshold
    );
}

fn spawn_record_circuit_success(
    circuit_breakers: Arc<tokio::sync::RwLock<std::collections::HashMap<String, CircuitBreaker>>>,
    failure_counts: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
    settings: Arc<tokio::sync::RwLock<AppSettings>>,
    db: Arc<Database>,
    app_handle: Option<crate::AppEventHandle>,
    entry_id: String,
) {
    tokio::spawn(async move {
        let recovery_secs = settings.read().await.circuit_recovery_secs as u64;

        let _ = db.set_entry_cooldown(&entry_id, None);
        crate::state_version::bump("pool");
        if let Some(h) = &app_handle {
            crate::event::emit(h, "entries-changed");
        }
        crate::state_version::bump("pool");

        // Clear failure count
        failure_counts.write().await.remove(&entry_id);

        let mut breakers = circuit_breakers.write().await;
        let cb = breakers
            .entry(entry_id)
            .or_insert_with(|| CircuitBreaker::new(recovery_secs));
        cb.set_recovery_secs(recovery_secs);
        cb.record_success();
    });
}

fn spawn_cool_down_entry(
    circuit_breakers: Arc<tokio::sync::RwLock<std::collections::HashMap<String, CircuitBreaker>>>,
    failure_counts: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
    settings: Arc<tokio::sync::RwLock<AppSettings>>,
    db: Arc<Database>,
    app_handle: Option<crate::AppEventHandle>,
    entry_id: String,
) {
    tokio::spawn(async move {
        let settings = settings.read().await.clone();
        let threshold = (settings.circuit_failure_threshold as u32).max(1);
        let recovery_secs = settings.circuit_recovery_secs as u64;

        let mut breakers = circuit_breakers.write().await;
        let cb = breakers
            .entry(entry_id.clone())
            .or_insert_with(|| CircuitBreaker::new(recovery_secs));
        cb.set_recovery_secs(recovery_secs);
        cb.record_failure(threshold);
        let effective_delay = cb.calculate_backoff_delay();
        let is_open = cb.get_state() == crate::proxy::circuit_breaker::CircuitState::Open;
        let open_cycles = cb.consecutive_opens();
        drop(breakers);

        // Increment failure count
        let mut counts = failure_counts.write().await;
        let count = counts.entry(entry_id.clone()).or_insert(0);
        *count += 1;
        let current_count = *count;
        drop(counts);

        // Any failure is counted. Before threshold: temporary cooldown.
        // At/above threshold: long-term 6h freeze protection.
        if current_count >= threshold * 2 {
            let six_hours_later = chrono::Utc::now().timestamp() + 21600;
            let _ = db.set_entry_cooldown(&entry_id, Some(six_hours_later));
            let _ = db.toggle_entry(&entry_id, false);
            if let Some(h) = &app_handle {
                crate::event::emit(h, "entries-changed");
            }
            crate::state_version::bump("pool");

            log::warn!(
                "Entry {} disabled after {} consecutive failures. Long cooldown: 6h.",
                entry_id,
                current_count
            );
            return;
        }

        let cooldown_until = chrono::Utc::now().timestamp() + effective_delay as i64;
        let _ = db.set_entry_cooldown(&entry_id, Some(cooldown_until));
        if let Some(h) = &app_handle {
            crate::event::emit(h, "entries-changed");
        }
        crate::state_version::bump("pool");

        log::warn!(
            "Entry {} cooled down for {}s after failure count {}/{}. Circuit open={:?}.",
            entry_id,
            effective_delay,
            current_count,
            threshold,
            is_open
        );
    });
}

fn split_raw_protocol_for_usage_log(raw_protocol: Option<&Value>) -> (String, String) {
    let Some(raw_protocol) = raw_protocol else {
        return (String::new(), String::new());
    };

    let mut raw_in = raw_protocol.clone();
    if let Some(obj) = raw_in.as_object_mut() {
        obj.insert(
            "event".to_string(),
            Value::String("upstream_request".to_string()),
        );
        obj.remove("response_timestamp_ms");
        obj.remove("status_code");
        obj.remove("content_type");
        obj.remove("x_request_id");
        obj.remove("upstream_response_body");
        obj.remove("error_body");
    }

    let mut raw_out = raw_protocol.clone();
    if let Some(obj) = raw_out.as_object_mut() {
        obj.remove("gateway_body");
        obj.remove("upstream_body");
        obj.remove("passthrough_header");
    }

    let has_out = raw_protocol
        .get("upstream_response_body")
        .or_else(|| raw_protocol.get("error_body"))
        .is_some();

    let content = if has_out {
        raw_out.to_string()
    } else {
        String::new()
    };
    (content, raw_in.to_string())
}

fn enrich_usage_log_in(
    other: &mut Value,
    first_token_ms: i64,
    status_code: i32,
    success: bool,
    reasoning_tokens: i64,
    attempt_path: Option<&str>,
    stream_end_reason: Option<StreamEndReason>,
) {
    if let Some(obj) = other.as_object_mut() {
        obj.insert(
            "first_token_ms".to_string(),
            Value::Number(first_token_ms.into()),
        );
        obj.insert("status_code".to_string(), Value::Number(status_code.into()));
        obj.insert("success".to_string(), Value::Bool(success));
        obj.insert(
            "reasoning_tokens".to_string(),
            Value::Number(reasoning_tokens.into()),
        );
        obj.insert(
            "attempt_path".to_string(),
            attempt_path
                .and_then(|path| serde_json::from_str::<Value>(path).ok())
                .unwrap_or(Value::Null),
        );
        obj.insert(
            "stream_end_reason".to_string(),
            stream_end_reason
                .map(StreamEndReason::as_str)
                .map(|reason| Value::String(reason.to_string()))
                .unwrap_or(Value::Null),
        );
    }
}

fn log_usage(
    db: &Database,
    app_handle: &Option<crate::AppEventHandle>,
    access_key: Option<&AccessKey>,
    entry: &ApiEntry,
    requested_model: &str,
    is_stream: bool,
    prompt_tokens: i64,
    completion_tokens: i64,
    reasoning_tokens: i64,
    first_token_ms: i64,
    latency_ms: i64,
    status_code: i32,
    success: bool,
    error_message: Option<&str>,
    attempt_path: Option<&str>,
    stream_end_reason: Option<StreamEndReason>,
    raw_protocol: Option<&Value>,
) {
    let log_type = if success { 2 } else { 5 };
    let (content_out, mut other_raw) = split_raw_protocol_for_usage_log(raw_protocol);
    let token_name = access_key.map(|ak| ak.name.as_str()).unwrap_or("NONE");
    let use_time = ((latency_ms as f64) / 1000.0).ceil() as i64;

    // 诊断前缀：abcc714 升级 IN/OUT 分离后，元数据写入 other 字段，
    // 但前端 LogPage.tsx 仅展示 log.content，导致请求/实际模型、尝试路径、
    // 流结束原因等关键信息在前端无法直接看到（历史 regression）。
    // 在此处把诊断信息前置到 content 头部，前端零改动即可恢复可见性。
    let mut content = String::new();
    content.push_str(&format!("请求模型:{}\n", requested_model));
    content.push_str(&format!("实际模型:{}\n", entry.model));
    let path_short = format_attempt_path_short(attempt_path);
    if !path_short.is_empty() {
        content.push_str(&format!("尝试路径:{}\n", path_short));
    }
    if let Some(reason) = stream_end_reason {
        content.push_str(&format!("流结束原因:{}\n", reason.as_str()));
    }
    if !content_out.is_empty() {
        content.push('\n');
        content.push_str(&content_out);
    }
    let mut other = if other_raw.trim().is_empty() {
        serde_json::json!({
            "event": "upstream_request",
            "requested_model": requested_model,
            "resolved_model": entry.model,
        })
    } else {
        serde_json::from_str::<Value>(&other_raw).unwrap_or_else(|_| {
            serde_json::json!({
                "event": "upstream_request",
                "requested_model": requested_model,
                "resolved_model": entry.model,
                "raw_in_parse_error": other_raw,
            })
        })
    };

    enrich_usage_log_in(
        &mut other,
        first_token_ms,
        status_code,
        success,
        reasoning_tokens,
        attempt_path,
        stream_end_reason,
    );

    other_raw = other.to_string();

    if db
        .insert_usage_log(
            log_type,
            &content,
            access_key.map(|ak| ak.id.as_str()),
            access_key.map(|ak| ak.name.as_str()).unwrap_or("NONE"),
            token_name,
            &entry.id,
            &entry.channel_id,
            entry.channel_name.as_deref().unwrap_or("unknown"),
            &entry.model,
            requested_model,
            0,
            is_stream,
            prompt_tokens,
            completion_tokens,
            latency_ms,
            first_token_ms,
            use_time,
            status_code,
            success,
            "",
            "default",
            &other_raw,
            error_message,
            None,
        )
        .is_ok()
    {
        crate::state_version::bump("log");
    }

    if let Some(h) = app_handle {
        crate::event::emit(h, "new-usage-log");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::protocol::get_adapter;

    fn test_proxy_state(show_conversation_model: bool) -> ProxyState {
        ProxyState {
            db: Arc::new(Database {
                conn: std::sync::Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
            }),
            settings: Arc::new(tokio::sync::RwLock::new(AppSettings {
                show_conversation_model,
                ..AppSettings::default()
            })),
            circuit_breakers: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            failure_counts: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            app_handle: None,
            http_client: reqwest::Client::new(),
            response_store: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    #[test]
    fn strip_model_info_suffix_removes_assistant_tail_only() {
        let input = "回答正文\n\nmodel: gpt-real";
        assert_eq!(strip_model_info_suffix(input), "回答正文");
    }

    #[test]
    fn strip_model_info_suffix_preserves_middle_and_code_block_model_text() {
        let middle = "这里提到 model: gpt-real 但不是尾部\n继续正文";
        assert_eq!(strip_model_info_suffix(middle), middle);

        let code = "```yaml\nmodel: gpt-real\n```";
        assert_eq!(strip_model_info_suffix(code), code);
    }

    #[test]
    fn clean_model_info_from_inbound_body_only_assistant_messages() {
        let mut body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "请保留这个 model: user-mentioned"},
                {"role": "assistant", "content": "上一轮回答\n\nmodel: gpt-real"},
                {"role": "tool", "content": "工具输出\n\nmodel: tool-mentioned"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "数组文本\n\nmodel: gpt-array"}
                ]}
            ]
        });

        clean_model_info_from_inbound_body(&mut body);

        assert_eq!(
            body["messages"][0]["content"],
            "请保留这个 model: user-mentioned"
        );
        assert_eq!(body["messages"][1]["content"], "上一轮回答");
        assert_eq!(
            body["messages"][2]["content"],
            "工具输出\n\nmodel: tool-mentioned"
        );
        assert_eq!(body["messages"][3]["content"][0]["text"], "数组文本");
    }

    #[test]
    fn clean_model_info_from_inbound_body_cleans_responses_and_claude_shapes() {
        let mut body = serde_json::json!({
            "input": [
                {"role": "assistant", "content": [
                    {"type": "output_text", "text": "responses 历史

model: gpt-responses"}
                ]}
            ],
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "claude 历史

model: claude-entry"}
                ]}
            ]
        });

        clean_model_info_from_inbound_body(&mut body);

        assert_eq!(body["input"][0]["content"][0]["text"], "responses 历史");
        assert_eq!(body["messages"][0]["content"][0]["text"], "claude 历史");
    }

    #[test]
    fn model_info_tool_loop_history_is_cleaned_before_final_text_injects_once() {
        let mut inbound_body = serde_json::json!({
            "model": "auto",
            "messages": [
                {"role": "assistant", "content": "上一轮可见回答

model: old-entry"},
                {"role": "user", "content": "继续"}
            ]
        });
        clean_model_info_from_inbound_body(&mut inbound_body);
        assert_eq!(inbound_body["messages"][0]["content"], "上一轮可见回答");

        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));

        let mut tool_buffer = String::new();
        let mut tool_remainder = Vec::new();
        let tool_has_text_delta = Arc::new(AtomicBool::new(false));
        let tool_has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut tool_done_state = SseDoneState::default();
        let tool_chunk = Bytes::from_static(
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}
data: [DONE]
"#,
        );
        let tool_output = append_and_parse_sse(
            &mut tool_buffer,
            &mut tool_remainder,
            &tool_chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &tool_has_text_delta,
            &tool_has_tool_calls,
            Some("new-entry"),
            &mut tool_done_state,
        );
        assert!(tool_has_tool_calls.load(Ordering::Relaxed));
        assert!(tool_output.is_none());
        assert!(!tool_done_state.appended_model_info);

        let mut text_buffer = String::new();
        let mut text_remainder = Vec::new();
        let text_has_text_delta = Arc::new(AtomicBool::new(false));
        let text_has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut text_done_state = SseDoneState::default();
        let text_chunk = Bytes::from_static(
            br#"data: {"choices":[{"delta":{"content":"final answer"}}]}
data: [DONE]
"#,
        );
        let text_output = append_and_parse_sse(
            &mut text_buffer,
            &mut text_remainder,
            &text_chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &text_has_text_delta,
            &text_has_tool_calls,
            Some("new-entry"),
            &mut text_done_state,
        )
        .expect("final text should inject exactly once");
        let text = String::from_utf8(text_output.to_vec()).expect("valid utf8");
        assert!(text_has_text_delta.load(Ordering::Relaxed));
        assert!(!text_has_tool_calls.load(Ordering::Relaxed));
        assert_eq!(text.matches("model: new-entry").count(), 1);
        assert_eq!(text.matches("model: old-entry").count(), 0);
    }

    /// Claude 直通的模型名注入：生成的应是合法 Anthropic content_block 事件，
    /// 含 `model: xxx` 文本。
    #[test]
    fn anthropic_model_info_events_are_valid() {
        let bytes = anthropic_model_info_events("claude-opus-4-8");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("event: content_block_start"));
        assert!(text.contains("event: content_block_delta"));
        assert!(text.contains("event: content_block_stop"));
        assert!(text.contains("model: claude-opus-4-8"));
        // 每个 data: 行都应是合法 JSON
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("data: ") {
                serde_json::from_str::<Value>(p).expect("data 行应为合法 JSON");
            }
        }
    }

    #[test]
    fn scan_anthropic_passthrough_supports_data_without_space() {
        let chunk = Bytes::from_static(
            br#"event: message_start
data:{"type":"message_start","message":{"usage":{"input_tokens":3}}}

event: content_block_delta
data:{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}

event: message_delta
data:{"type":"message_delta","usage":{"output_tokens":2}}

event: message_stop
data:{"type":"message_stop"}

"#,
        );
        let has_text = Arc::new(AtomicBool::new(false));
        let has_tool = Arc::new(AtomicBool::new(false));
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));

        let saw_stop = scan_anthropic_passthrough_chunk(
            &chunk,
            &has_text,
            &has_tool,
            &prompt_tokens,
            &completion_tokens,
        );

        assert!(saw_stop);
        assert!(has_text.load(Ordering::Relaxed));
        assert!(!has_tool.load(Ordering::Relaxed));
        assert_eq!(prompt_tokens.load(Ordering::Relaxed), 3);
        assert_eq!(completion_tokens.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn raw_protocol_capture_keeps_original_request_and_response() {
        let entry = ApiEntry {
            id: "entry-1".to_string(),
            channel_id: "channel-1".to_string(),
            model: "gpt-4.1".to_string(),
            display_name: "gpt-4.1".to_string(),
            sort_index: 0,
            enabled: true,
            cooldown_until: None,
            circuit_state: "closed".to_string(),
            created_at: 0,
            updated_at: 0,
            channel_name: Some("OpenAI".to_string()),
            channel_api_type: Some("responses".to_string()),
            owned_by: None,
            response_ms: None,
            provider_logo: None,
            release_date: None,
            model_meta_zh: None,
            model_meta_en: None,
            group_name: None,
            score: 0.0,
        };
        let gateway_body = serde_json::json!({"input": "hello"});
        let upstream_body = serde_json::json!({"model": "gpt-4.1", "input": "hello"});
        let response_body = serde_json::json!({"id": "resp_1", "output": [{"type": "message"}]});
        let headers = reqwest::header::HeaderMap::new();

        let request = build_raw_protocol_request_log(
            &CallerKind::Responses,
            "alias-model",
            &entry,
            "responses",
            "https://example.com/v1/responses?api_key=raw",
            false,
            Some("x-api-switch-responses-passthrough"),
            &gateway_body,
            &upstream_body,
        );
        let capture = with_raw_protocol_response(request, 200, &headers, response_body.clone());

        assert_eq!(capture["requested_model"], "alias-model");
        assert_eq!(
            capture["url"],
            "https://example.com/v1/responses?api_key=raw"
        );
        assert_eq!(capture["gateway_body"], gateway_body);
        assert_eq!(capture["upstream_body"], upstream_body);
        assert_eq!(capture["upstream_response_body"], response_body);
    }

    #[test]
    fn raw_protocol_persist_policy_keeps_only_failed_empty_output() {
        assert!(should_persist_raw_protocol_for_log(
            false, 0, 0, false, false
        ));
        assert!(should_persist_raw_protocol_for_log(
            false, 1048570, 0, false, false
        ));
        assert!(should_persist_raw_protocol_for_log(
            false, 10, 0, false, false
        ));
        assert!(!should_persist_raw_protocol_for_log(
            false, 10, 3, true, false
        ));
        assert!(should_persist_raw_protocol_for_log(
            false, 10, 0, false, true
        ));
        assert!(!should_persist_raw_protocol_for_log(
            true, 0, 0, false, false
        ));
        assert!(!should_persist_raw_protocol_for_log(
            true, 10, 0, false, false
        ));
    }

    #[test]
    fn raw_protocol_usage_log_fields_split_in_and_out() {
        let raw_protocol = serde_json::json!({
            "event": "upstream_response",
            "timestamp_ms": 1,
            "response_timestamp_ms": 2,
            "caller_kind": "responses",
            "requested_model": "auto",
            "entry_id": "entry-1",
            "entry_model": "gpt-5.5",
            "channel_id": "channel-1",
            "channel_name": "sharedchat",
            "channel_api_type": "responses",
            "url": "https://example.com/v1/responses?api_key=raw",
            "is_stream": false,
            "passthrough_header": "x-api-switch-responses-passthrough",
            "gateway_body": {"model": "auto", "input": "原始入口请求"},
            "upstream_body": {"model": "gpt-5.5", "input": "原始上游请求"},
            "status_code": 200,
            "content_type": "application/json",
            "x_request_id": "req-1",
            "upstream_response_body": {"output": [{"text": "原始上游返回"}]}
        });

        let (content, other) = split_raw_protocol_for_usage_log(Some(&raw_protocol));
        let content = serde_json::from_str::<Value>(&content).expect("content 应为 OUT JSON");
        let other = serde_json::from_str::<Value>(&other).expect("other 应为 IN JSON");

        assert_eq!(other["event"], "upstream_request");
        assert_eq!(other["gateway_body"]["input"], "原始入口请求");
        assert_eq!(other["upstream_body"]["input"], "原始上游请求");
        assert!(other.get("upstream_response_body").is_none());
        assert!(other.get("error_body").is_none());

        assert_eq!(content["event"], "upstream_response");
        assert_eq!(content["status_code"], 200);
        assert_eq!(
            content["upstream_response_body"]["output"][0]["text"],
            "原始上游返回"
        );
        assert!(content.get("gateway_body").is_none());
        assert!(content.get("upstream_body").is_none());
    }

    /// 扫描函数应正确识别 text_delta(有文本)、tool_use(工具调用)、message_stop，
    /// 并从 message_start / message_delta 提取 usage token。
    #[test]
    fn scan_anthropic_passthrough_detects_signals() {
        let has_text = Arc::new(AtomicBool::new(false));
        let has_tool = Arc::new(AtomicBool::new(false));
        let pt = Arc::new(AtomicI64::new(0));
        let ct = Arc::new(AtomicI64::new(0));

        // message_start 带 input_tokens（在 /message/usage）
        let start_chunk = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":123,\"output_tokens\":1}}}\n\n",
        );
        scan_anthropic_passthrough_chunk(&start_chunk, &has_text, &has_tool, &pt, &ct);
        assert_eq!(
            pt.load(Ordering::Relaxed),
            123,
            "应从 message_start 取 input_tokens"
        );

        // 高缓存命中：input_tokens=0 但 cache token 很大，prompt 应计入缓存
        let pt2 = Arc::new(AtomicI64::new(0));
        let cache_chunk = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":1,\"cache_creation_input_tokens\":490,\"cache_read_input_tokens\":429758}}}\n\n",
        );
        scan_anthropic_passthrough_chunk(&cache_chunk, &has_text, &has_tool, &pt2, &ct);
        assert_eq!(
            pt2.load(Ordering::Relaxed),
            430248,
            "高缓存命中时 prompt 应 = input + cache_read + cache_creation"
        );

        let text_chunk = Bytes::from_static(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        );
        let stop = scan_anthropic_passthrough_chunk(&text_chunk, &has_text, &has_tool, &pt, &ct);
        assert!(!stop);
        assert!(has_text.load(Ordering::Relaxed));
        assert!(!has_tool.load(Ordering::Relaxed));

        // message_delta 带 output_tokens
        let delta_chunk = Bytes::from_static(
            b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":70}}\n\n",
        );
        scan_anthropic_passthrough_chunk(&delta_chunk, &has_text, &has_tool, &pt, &ct);
        assert_eq!(
            ct.load(Ordering::Relaxed),
            70,
            "应从 message_delta 取 output_tokens"
        );

        let tool_chunk = Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"x\"}}\n\n",
        );
        scan_anthropic_passthrough_chunk(&tool_chunk, &has_text, &has_tool, &pt, &ct);
        assert!(has_tool.load(Ordering::Relaxed), "应检测到 tool_use");

        let stop_chunk =
            Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        assert!(
            scan_anthropic_passthrough_chunk(&stop_chunk, &has_text, &has_tool, &pt, &ct),
            "应检测到 message_stop"
        );
    }

    #[test]
    fn transformed_sse_chunks_are_standard_sse_frames() {
        let adapter = get_adapter("claude");
        let chunk = Bytes::from_static(
            b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-3\"}}\n\
data: [DONE]\n"
        );
        let mut buffer = String::new();
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));

        let mut remainder = Vec::new();
        let output = transform_sse_chunk(
            &chunk,
            &mut buffer,
            &mut remainder,
            &adapter,
            &prompt_tokens,
            &completion_tokens,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicBool::new(false)),
            None,
            &mut SseDoneState::default(),
        )
        .expect("transformed output");
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");

        assert!(output.contains("\n\n"));
        assert!(output.ends_with("data: [DONE]\n\n"));
        assert!(!output.contains("data: [DONE]\n\ndata: [DONE]"));
    }

    #[test]
    fn model_info_label_appends_channel_name_when_available() {
        assert_eq!(
            model_info_label("deepseek-ai/deepseek-v4-pro", Some("渠道信息")),
            "deepseek-ai/deepseek-v4-pro/渠道信息"
        );
        assert_eq!(model_info_label("gpt-test", None), "gpt-test");
        assert_eq!(model_info_label("gpt-test", Some("unknown")), "gpt-test");
    }

    #[test]
    fn model_info_delta_uses_openai_chat_completion_chunk_shape() {
        let frame = String::from_utf8(model_info_delta("gpt-test/渠道信息")).expect("valid utf8");
        let payload = frame
            .strip_prefix("data: ")
            .and_then(|s| s.strip_suffix("\n\n"))
            .expect("standard SSE data frame");
        let value: Value = serde_json::from_str(payload).expect("valid json");

        assert!(value["id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("chatcmpl-"));
        assert_eq!(value["object"], "chat.completion.chunk");
        assert!(value["created"].as_u64().is_some());
        assert_eq!(value["model"], "gpt-test/渠道信息");
        assert_eq!(value["choices"][0]["index"], 0);
        assert_eq!(
            value["choices"][0]["delta"]["content"],
            "\n\nmodel: gpt-test/渠道信息"
        );
        assert!(value["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn responses_model_info_event_uses_responses_output_text_delta_shape() {
        let frame =
            String::from_utf8(responses_model_info_event("gpt-test/渠道信息")).expect("valid utf8");
        assert!(frame.starts_with("event: response.output_text.delta\n"));
        let payload = frame
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("data frame");
        let value: Value = serde_json::from_str(payload).expect("valid json");

        assert_eq!(value["type"], "response.output_text.delta");
        assert_eq!(value["delta"], "\n\nmodel: gpt-test/渠道信息");
    }

    #[test]
    fn inject_responses_model_info_chunk_places_one_delta_before_completed() {
        let chunk = Bytes::from_static(
            br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello"}

event: response.completed
data: {"type":"response.completed","response":{}}

"#,
        );

        let injected = String::from_utf8(
            inject_responses_model_info_chunk(&chunk, "deepseek-ai/deepseek-v4-pro/渠道信息")
                .to_vec(),
        )
        .expect("valid utf8");

        assert_eq!(
            injected
                .matches("model: deepseek-ai/deepseek-v4-pro/渠道信息")
                .count(),
            1
        );
        assert_eq!(
            injected
                .matches("event: response.output_text.delta")
                .count(),
            2
        );
        let model_pos = injected
            .find("model: deepseek-ai/deepseek-v4-pro/渠道信息")
            .expect("model marker");
        let completed_pos = injected
            .find("event: response.completed")
            .expect("completed event");
        assert!(model_pos < completed_pos);
    }

    #[test]
    fn responses_model_info_injection_gate_skips_tool_or_duplicate() {
        assert!(should_inject_responses_model_info(
            true, true, true, false, false
        ));
        assert!(!should_inject_responses_model_info(
            false, true, true, false, false
        ));
        assert!(!should_inject_responses_model_info(
            true, false, true, false, false
        ));
        assert!(!should_inject_responses_model_info(
            true, true, false, false, false
        ));
        assert!(!should_inject_responses_model_info(
            true, true, true, true, false
        ));
        assert!(!should_inject_responses_model_info(
            true, true, true, false, true
        ));
    }

    #[test]
    fn scan_responses_passthrough_chunk_detects_text_completed_and_tool_calls() {
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text = Arc::new(AtomicBool::new(false));
        let has_tool = Arc::new(AtomicBool::new(false));
        let text_chunk = Bytes::from_static(
            br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello"}

event: response.completed
data: {"type":"response.completed","response":{"usage":{"input_tokens":2,"output_tokens":3}}}

"#,
        );

        assert!(scan_responses_passthrough_chunk(
            &text_chunk,
            &has_sse_error,
            &has_text,
            &has_tool,
            &prompt_tokens,
            &completion_tokens,
        ));
        assert!(has_text.load(Ordering::Relaxed));
        assert!(!has_tool.load(Ordering::Relaxed));
        assert_eq!(prompt_tokens.load(Ordering::Relaxed), 2);
        assert_eq!(completion_tokens.load(Ordering::Relaxed), 3);

        let tool_chunk = Bytes::from_static(
            br#"event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","name":"read"}}

event: response.completed
data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}

"#,
        );
        let has_text = Arc::new(AtomicBool::new(false));
        let has_tool = Arc::new(AtomicBool::new(false));
        assert!(scan_responses_passthrough_chunk(
            &tool_chunk,
            &has_sse_error,
            &has_text,
            &has_tool,
            &prompt_tokens,
            &completion_tokens,
        ));
        assert!(!has_text.load(Ordering::Relaxed));
        assert!(has_tool.load(Ordering::Relaxed));
    }

    #[test]
    fn appended_model_info_frame_is_openai_compatible_before_done() {
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\
data: [DONE]\n",
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();
        let mut remainder = Vec::new();

        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        )
        .expect("model info should be appended");
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");
        let model_frame = output
            .split("\n\n")
            .find(|frame| frame.contains("model: gpt-test"))
            .expect("model info frame exists");
        let payload = model_frame.strip_prefix("data: ").expect("data frame");
        let value: Value = serde_json::from_str(payload).expect("valid json");

        assert_eq!(value["object"], "chat.completion.chunk");
        assert_eq!(value["model"], "gpt-test");
        assert_eq!(value["choices"][0]["index"], 0);
        assert!(value["choices"][0]["finish_reason"].is_null());
        assert!(output.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn stream_failure_keyword_candidate_uses_same_front_guard_as_non_stream() {
        let settings = AppSettings::default();

        assert!(should_record_keyword_candidate(
            200,
            "upstream stream completed with SSE error: rate limit exceeded",
            &settings,
        ));
        assert!(!should_record_keyword_candidate(
            401,
            "Unauthorized - Invalid token",
            &settings,
        ));
    }

    #[test]
    fn decode_timeout_after_stream_started_suppresses_cooldown_classification() {
        assert!(is_recoverable_decode_timeout_after_stream_started(
            200, 3705, 1, 218, false, false, 0, false, true, true, false
        ));
    }

    #[test]
    fn decode_timeout_after_valid_output_suppresses_cooldown_classification() {
        assert!(is_recoverable_decode_timeout_after_stream_started(
            200, 5041, 33, 9803, true, false, 0, false, true, true, false
        ));
    }

    #[test]
    fn decode_timeout_before_stream_started_does_not_suppress_cooldown() {
        assert!(!is_recoverable_decode_timeout_after_stream_started(
            200, 0, 0, 0, false, false, 0, false, true, true, false
        ));
    }

    #[test]
    fn decode_timeout_with_sse_error_does_not_suppress_cooldown() {
        assert!(!is_recoverable_decode_timeout_after_stream_started(
            200, 1000, 1, 128, false, false, 0, true, true, true, false
        ));
    }

    #[test]
    fn dropped_stream_success_requires_valid_output_on_http_200() {
        assert!(
            !is_dropped_stream_success(200, 0, 0, false, false),
            "HTTP 200 + IN/OUT 0 + no observed output must not be logged as success"
        );
        assert!(
            !is_dropped_stream_success(200, 1048570, 0, false, false),
            "prompt_tokens are input only; prompt-only streams still have no valid output"
        );
        assert!(is_dropped_stream_success(200, 0, 1, false, false));
        assert!(is_dropped_stream_success(200, 0, 0, true, false));
        assert!(is_dropped_stream_success(200, 0, 0, false, true));
        assert!(!is_dropped_stream_success(500, 999, 999, true, true));
    }

    #[test]
    fn append_and_parse_sse_error_null_not_marked_as_error() {
        let mut buffer = String::new();
        let chunk = Bytes::from_static(b"data: {\"error\":null}\n");
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("gpt-test"),
            &mut done_state,
        );

        assert!(output.is_none());
        assert!(!has_sse_error.load(Ordering::Relaxed));
    }

    #[test]
    fn append_and_parse_sse_error_object_marked_as_error() {
        let mut buffer = String::new();
        let chunk = Bytes::from_static(b"data: {\"error\":{\"message\":\"boom\"}}\n");
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("gpt-test"),
            &mut done_state,
        );

        assert!(output.is_none());
        assert!(has_sse_error.load(Ordering::Relaxed));
    }

    #[test]
    fn append_and_parse_sse_duplicate_done_only_appends_model_once() {
        let mut buffer = String::new();
        let first_chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\
data: [DONE]\n",
        );
        let second_chunk = Bytes::from_static(b"data: [DONE]\n");
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let first_output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &first_chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("gpt-test"),
            &mut done_state,
        )
        .expect("first done should append model once");
        let second_output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &second_chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("gpt-test"),
            &mut done_state,
        );

        let first_text = String::from_utf8(first_output.to_vec()).expect("valid utf8");
        assert_eq!(first_text.matches("model: gpt-test").count(), 1);
        assert!(second_output.is_none());
    }

    #[test]
    fn append_and_parse_sse_does_not_duplicate_existing_model_info() {
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"\\n\\nmodel: gpt-test\"}}]}\n\
data: [DONE]\n",
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("gpt-test"),
            &mut done_state,
        );

        let text = String::from_utf8(output.unwrap_or(chunk).to_vec()).expect("valid utf8");
        assert_eq!(text.matches("model: gpt-test").count(), 1);
    }

    #[test]
    fn transform_sse_chunk_duplicate_done_only_outputs_single_done_and_model_once() {
        let adapter = get_adapter("claude");
        let chunk = Bytes::from_static(
            b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\
data: [DONE]\n\
data: [DONE]\n"
        );
        let mut buffer = String::new();
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = transform_sse_chunk(
            &chunk,
            &mut buffer,
            &mut remainder,
            &adapter,
            &prompt_tokens,
            &completion_tokens,
            &Arc::new(AtomicBool::new(false)),
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("claude-3"),
            &mut done_state,
        )
        .expect("transformed output");

        let output = String::from_utf8(output.to_vec()).expect("valid utf8");
        assert_eq!(output.matches("data: [DONE]").count(), 1);
        assert_eq!(output.matches("model: claude-3").count(), 1);
    }

    #[test]
    fn transform_sse_chunk_does_not_duplicate_existing_model_info() {
        let adapter = get_adapter("openai");
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"\\n\\nmodel: gpt-test\"}}]}\n\
data: [DONE]\n",
        );
        let mut buffer = String::new();
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = transform_sse_chunk(
            &chunk,
            &mut buffer,
            &mut remainder,
            &adapter,
            &prompt_tokens,
            &completion_tokens,
            &Arc::new(AtomicBool::new(false)),
            &has_text_delta,
            &Arc::new(AtomicBool::new(false)),
            Some("gpt-test"),
            &mut done_state,
        )
        .expect("transformed output");

        let output = String::from_utf8(output.to_vec()).expect("valid utf8");
        assert_eq!(output.matches("data: [DONE]").count(), 1);
        assert_eq!(output.matches("model: gpt-test").count(), 1);
    }

    #[test]
    fn passthrough_responses_usage_detection_supports_input_output_tokens() {
        let body = serde_json::json!({
            "object": "response",
            "output": [],
            "usage": {"input_tokens": 12, "output_tokens": 7}
        });
        let (prompt_tokens, completion_tokens, _reasoning) = extract_usage_tokens(&body);

        assert_eq!(prompt_tokens, 12);
        assert_eq!(completion_tokens, 7);
        assert!(!responses_response_has_valid_output(&body));
    }

    #[test]
    fn passthrough_responses_valid_output_requires_nonempty_output_array() {
        let valid = serde_json::json!({
            "object": "response",
            "output": [{"type": "message"}]
        });
        let invalid = serde_json::json!({
            "object": "response",
            "output": []
        });

        assert!(responses_response_has_valid_output(&valid));
        assert!(!responses_response_has_valid_output(&invalid));
    }

    #[test]
    fn passthrough_responses_text_delta_is_valid_stream_output() {
        let text = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "hi"
        });
        let reasoning = serde_json::json!({
            "type": "response.reasoning_text.delta",
            "delta": "think"
        });
        let empty = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": ""
        });

        assert!(stream_chunk_has_text_delta(&text));
        assert!(stream_chunk_has_text_delta(&reasoning));
        assert!(!stream_chunk_has_text_delta(&empty));
    }

    #[test]
    fn passthrough_responses_tool_call_chunks_are_valid_stream_output() {
        let tool_delta = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{\"city\":"
        });
        let item_added = serde_json::json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "id": "call_1"}
        });

        assert!(stream_chunk_has_tool_calls(&tool_delta));
        assert!(stream_chunk_has_tool_calls(&item_added));
    }

    #[test]
    fn normalize_reasoning_fields_maps_details_without_creating_history() {
        let mut details = serde_json::json!({
            "role": "assistant",
            "content": "",
            "reasoning_details": "kept reasoning"
        });
        normalize_reasoning_fields(&mut details);
        assert_eq!(details["reasoning_content"], "kept reasoning");
        assert_eq!(details["reasoning_text"], "kept reasoning");
        assert_eq!(details["reasoning_details"], "kept reasoning");

        let mut no_reasoning = serde_json::json!({
            "role": "assistant",
            "content": "answer"
        });
        normalize_reasoning_fields(&mut no_reasoning);
        assert!(no_reasoning.get("reasoning_content").is_none());
        assert!(no_reasoning.get("reasoning_text").is_none());
    }

    #[test]
    fn normalize_reasoning_in_sse_chunk_maps_details_delta() {
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"reasoning_details\":\"hidden\"}}]}\n\n",
        );

        let output = normalize_reasoning_in_sse_chunk(&chunk).expect("modified chunk");
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");

        assert!(output.contains("\"reasoning_details\":\"hidden\""));
        assert!(output.contains("\"reasoning_content\":\"hidden\""));
        assert!(output.contains("\"reasoning_text\":\"hidden\""));
    }

    #[test]
    fn normalize_reasoning_fields_keeps_structured_details_separate() {
        let mut details = serde_json::json!({
            "role": "assistant",
            "reasoning_details": [{"type": "summary", "text": "hidden"}]
        });

        normalize_reasoning_fields(&mut details);

        assert!(details.get("reasoning_content").is_none());
        assert!(details.get("reasoning_text").is_none());
        assert!(details.get("reasoning_details").is_some());
    }

    #[test]
    fn normalize_reasoning_in_sse_chunk_supports_data_without_space_and_crlf() {
        let chunk = Bytes::from_static(
            b"data:{\"choices\":[{\"delta\":{\"reasoning_content\":\"hidden\"}}]}\r\n\r\n",
        );

        let output = normalize_reasoning_in_sse_chunk(&chunk).expect("modified chunk");
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");

        assert!(output.contains("data: {"));
        assert!(output.contains("\"reasoning_text\":\"hidden\""));
        assert!(output.contains("\r\n"));
    }

    #[test]
    fn normalize_reasoning_in_sse_chunk_passes_non_utf8_through() {
        let chunk = Bytes::from_static(b"data: \xff\xfe\n");
        assert!(normalize_reasoning_in_sse_chunk(&chunk).is_none());
    }

    #[test]
    fn transform_sse_chunk_normalizes_reasoning_after_adapter_transform() {
        let adapter = get_adapter("responses");
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"reasoning_details\":\"hidden\"}}]}\n",
        );
        let mut buffer = String::new();
        let mut remainder = Vec::new();
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let output = transform_sse_chunk(
            &chunk,
            &mut buffer,
            &mut remainder,
            &adapter,
            &prompt_tokens,
            &completion_tokens,
            &Arc::new(AtomicBool::new(false)),
            &has_text_delta,
            &has_tool_calls,
            None,
            &mut done_state,
        )
        .expect("transformed output");
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");

        assert!(output.contains("\"reasoning_details\":\"hidden\""));
        assert!(output.contains("\"reasoning_content\":\"hidden\""));
        assert!(output.contains("\"reasoning_text\":\"hidden\""));
    }

    #[test]
    fn transform_sse_chunk_marks_responses_failed_as_sse_error() {
        let adapter = get_adapter("responses");
        let chunk = Bytes::from_static(
            b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\",\"type\":\"server_error\"}}}\n",
        );
        let mut buffer = String::new();
        let mut remainder = Vec::new();
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let output = transform_sse_chunk(
            &chunk,
            &mut buffer,
            &mut remainder,
            &adapter,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            None,
            &mut done_state,
        )
        .expect("transformed error output");
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");

        assert!(output.contains("\"error\""));
        assert!(has_sse_error.load(Ordering::Relaxed));
        assert!(!is_completed_stream_success(
            200,
            has_sse_error.load(Ordering::Relaxed),
            true,
            false,
            1
        ));
    }

    #[test]
    fn completed_stream_success_logic() {
        assert!(!is_completed_stream_success(200, false, false, false, 0));
        assert!(is_completed_stream_success(200, false, true, false, 0));
        assert!(is_completed_stream_success(200, false, false, true, 0));
        assert!(is_completed_stream_success(200, false, false, false, 5));
        assert!(!is_completed_stream_success(200, true, true, false, 5));
        assert!(!is_completed_stream_success(502, false, true, false, 5));
    }

    #[test]
    fn gemini_passthrough_response_accepts_native_output_parts() {
        let text = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "ok"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });
        assert!(gemini_response_has_valid_output(&text));

        let function_call = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "lookup",
                            "args": {"q": "x"}
                        }
                    }],
                    "role": "model"
                }
            }]
        });
        assert!(gemini_response_has_valid_output(&function_call));
    }

    #[test]
    fn gemini_passthrough_response_rejects_empty_native_output() {
        let empty_text = serde_json::json!({
            "candidates": [{
                "content": {"parts": [{"text": ""}], "role": "model"},
                "finishReason": "STOP"
            }]
        });
        assert!(!gemini_response_has_valid_output(&empty_text));

        let no_parts = serde_json::json!({
            "candidates": [{"content": {"parts": []}}]
        });
        assert!(!gemini_response_has_valid_output(&no_parts));
    }

    #[test]
    fn nonstream_response_requires_visible_output() {
        let empty_content_with_usage = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": ""}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        });
        assert!(!nonstream_response_has_valid_output(
            &empty_content_with_usage
        ));

        let text = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        });
        assert!(nonstream_response_has_valid_output(&text));

        let tool_call = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1"}]}}]
        });
        assert!(nonstream_response_has_valid_output(&tool_call));
    }

    #[test]
    fn nonstream_response_reasoning_only_is_valid_output() {
        let reasoning_only = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "", "reasoning_content": "hidden"}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        });
        assert!(nonstream_response_has_valid_output(&reasoning_only));
    }

    #[test]
    fn append_and_parse_sse_empty_done_has_no_valid_output() {
        let mut buffer = String::new();
        let chunk = Bytes::from_static(b"data: [DONE]\n");
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();
        let mut remainder = Vec::new();

        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        assert!(output.is_none());
        assert!(!has_text_delta.load(Ordering::Relaxed));
        assert!(!has_tool_calls.load(Ordering::Relaxed));
        assert_eq!(completion_tokens.load(Ordering::Relaxed), 0);
        assert!(!is_completed_stream_success(
            200,
            has_sse_error.load(Ordering::Relaxed),
            has_text_delta.load(Ordering::Relaxed),
            has_tool_calls.load(Ordering::Relaxed),
            completion_tokens.load(Ordering::Relaxed)
        ));
    }

    #[test]
    fn request_structured_output_and_tool_calling_detection_are_separate() {
        let tools_body = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "run"}}]
        });
        assert!(!request_uses_structured_output(&tools_body));
        assert!(request_uses_tool_calling(&tools_body));

        let tool_choice_body = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": "auto"
        });
        assert!(!request_uses_structured_output(&tool_choice_body));
        assert!(request_uses_tool_calling(&tool_choice_body));

        let structured_body = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {"type": "json_object"}
        });
        assert!(request_uses_structured_output(&structured_body));
        assert!(!request_uses_tool_calling(&structured_body));

        let plain_body = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(!request_uses_structured_output(&plain_body));
        assert!(!request_uses_tool_calling(&plain_body));
    }

    // ── tool_calls 场景测试 ─────────────────────────────────────────

    #[test]
    fn append_and_parse_sse_no_model_info_when_tool_calls_present() {
        // 场景：响应同时包含文本内容和工具调用（LLM 常见行为）
        // 预期：不附加模型信息，因为 tool_call 是中间步骤
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\
data: {\"choices\":[],\"finish_reason\":\"tool_calls\"}\n\
data: [DONE]\n"
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        // 有文本内容，所以 has_text_delta 应为 true
        assert!(has_text_delta.load(Ordering::Relaxed));
        // 有工具调用，所以 has_tool_calls 应为 true
        assert!(has_tool_calls.load(Ordering::Relaxed));
        // 不应附加模型信息
        assert!(output.is_none());
        assert!(!done_state.appended_model_info);
    }

    #[test]
    fn append_and_parse_sse_no_model_info_when_pure_tool_calls() {
        // 场景：纯工具调用响应（无文本内容）
        // 预期：不附加模型信息
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\
data: {\"choices\":[],\"finish_reason\":\"tool_calls\"}\n\
data: [DONE]\n"
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        // 无文本内容，has_text_delta 应为 false
        assert!(!has_text_delta.load(Ordering::Relaxed));
        // 有工具调用，has_tool_calls 应为 true
        assert!(has_tool_calls.load(Ordering::Relaxed));
        // 不应附加模型信息
        assert!(output.is_none());
        assert!(!done_state.appended_model_info);
    }

    #[test]
    fn append_and_parse_sse_appends_model_info_for_pure_text() {
        // 场景：纯文本响应（无工具调用）
        // 预期：附加模型信息
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello!\"}}]}\n\
data: {\"choices\":[],\"finish_reason\":\"stop\"}\n\
data: [DONE]\n",
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        // 有文本内容
        assert!(has_text_delta.load(Ordering::Relaxed));
        // 无工具调用
        assert!(!has_tool_calls.load(Ordering::Relaxed));
        // 应附加模型信息
        let text =
            String::from_utf8(output.expect("should have output").to_vec()).expect("valid utf8");
        assert_eq!(text.matches("model: gpt-test").count(), 1);
        assert!(done_state.appended_model_info);
    }

    /// 回归测试（历史 bug）：带 tools 定义但本轮响应**没有**实际 tool_calls
    /// 应该正常注入 model: xxx。
    ///
    /// 历史：commit 3f5825d 曾在请求侧检查 body 里是否含任何 tool 字段来屏蔽注入，
    /// 结果所有 agent 客户端（Cursor / Claude Code 等）都看不到模型名。
    /// 本测试锁定修复：请求侧检查已撤销，只看响应侧 `has_tool_calls`。
    #[test]
    fn should_append_model_info_ignores_request_tools_definition() {
        // 仅测纯函数行为：request_uses_structured_output 与 request body 是否含 tools 无关
        let body_with_tools = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "你好"}],
            "tools": [{
                "type": "function",
                "function": {"name": "read_file", "parameters": {}}
            }],
            "tool_choice": "auto"
        });
        // 核心断言：带 tools 的请求不应触发 structured_output 分支
        assert!(!request_uses_structured_output(&body_with_tools));
        // 次要断言：contains_tool_calling_field 仍识别 tools 定义（以备未来精细化判断）
        assert!(contains_tool_calling_field(&body_with_tools));
    }

    #[test]
    fn should_append_model_info_allows_responses_caller_when_setting_enabled() {
        let state = test_proxy_state(true);
        let body = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "你好"}],
        });

        assert!(should_append_model_info(
            &state,
            &body,
            &CallerKind::Responses,
        ));
        assert!(should_append_model_info(
            &state,
            &body,
            &CallerKind::OpenAiChat,
        ));
    }

    #[test]
    fn append_and_parse_sse_appends_model_info_when_text_only_response_despite_tools_available() {
        // Scenario: agent clients (Cursor / Claude Code) always send `tools` definition,
        // but the actual response contains text only - no tool_calls.
        // Historic 3f5825d bug: tools-in-request blocked injection entirely -> users
        //   on agent clients never saw `model: xxx`.
        // Fix: request-side tool-definition check removed; only response-side
        //   has_tool_calls blocks injection. A text-only response must inject once.
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello!\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\" How can I help?\"}}]}\n\
data: {\"choices\":[],\"finish_reason\":\"stop\"}\n\
data: [DONE]\n",
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-4o"),
            &mut done_state,
        );

        assert!(has_text_delta.load(Ordering::Relaxed));
        assert!(
            !has_tool_calls.load(Ordering::Relaxed),
            "response has no tool_calls; has_tool_calls must remain false"
        );
        let text = String::from_utf8(
            output
                .expect("model: gpt-4o should be injected (regression: tools-in-request no longer blocks)")
                .to_vec())
        .expect("valid utf8");
        assert_eq!(text.matches("model: gpt-4o").count(), 1);
        assert!(done_state.appended_model_info);
    }

    #[test]
    fn transform_sse_chunk_no_model_info_when_tool_calls_present() {
        // 场景：Claude 适配器路径，响应包含工具调用
        // 预期：不附加模型信息
        let adapter = get_adapter("claude");
        let chunk = Bytes::from_static(
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\"}}\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\
data: {\"type\":\"message_stop\"}\n\
data: [DONE]\n"
        );
        let mut buffer = String::new();
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = transform_sse_chunk(
            &chunk,
            &mut buffer,
            &mut remainder,
            &adapter,
            &prompt_tokens,
            &completion_tokens,
            &Arc::new(AtomicBool::new(false)),
            &has_text_delta,
            &has_tool_calls,
            Some("claude-3"),
            &mut done_state,
        )
        .expect("transformed output");

        // 工具调用被转换为 OpenAI 格式的 tool_calls
        assert!(has_tool_calls.load(Ordering::Relaxed));
        // 不应附加模型信息
        let output = String::from_utf8(output.to_vec()).expect("valid utf8");
        assert_eq!(output.matches("model: claude-3").count(), 0);
        assert!(!done_state.appended_model_info);
    }

    #[test]
    fn append_and_parse_sse_no_model_info_when_finish_reason_tool_calls() {
        // 场景：finish_reason 为 "tool_calls" 但 delta 中没有 tool_calls 数组
        // 这是 Claude 适配器在 message_delta 事件中的输出格式
        // 预期：不附加模型信息
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\
data: [DONE]\n",
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        // 有文本内容
        assert!(has_text_delta.load(Ordering::Relaxed));
        // finish_reason: "tool_calls" 应被检测为工具调用
        assert!(has_tool_calls.load(Ordering::Relaxed));
        // 不应附加模型信息
        assert!(output.is_none());
        assert!(!done_state.appended_model_info);
    }

    #[test]
    fn append_and_parse_sse_no_model_info_when_finish_reason_function_call() {
        // 场景：finish_reason 为 "function_call"（旧格式）
        // 预期：不附加模型信息
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"function_call\"}]}\n\
data: [DONE]\n",
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        // 有文本内容
        assert!(has_text_delta.load(Ordering::Relaxed));
        // finish_reason: "function_call" 应被检测为工具调用
        assert!(has_tool_calls.load(Ordering::Relaxed));
        // 不应附加模型信息
        assert!(output.is_none());
        assert!(!done_state.appended_model_info);
    }

    #[test]
    fn append_and_parse_sse_no_model_info_when_delta_function_call() {
        let mut buffer = String::new();
        let chunk = Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"function_call\":{\"name\":\"get_weather\",\"arguments\":\"{}\"}}}]}\n\
data: [DONE]\n"
        );
        let prompt_tokens = Arc::new(AtomicI64::new(0));
        let completion_tokens = Arc::new(AtomicI64::new(0));
        let has_sse_error = Arc::new(AtomicBool::new(false));
        let has_text_delta = Arc::new(AtomicBool::new(false));
        let has_tool_calls = Arc::new(AtomicBool::new(false));
        let mut done_state = SseDoneState::default();

        let mut remainder = Vec::new();
        let output = append_and_parse_sse(
            &mut buffer,
            &mut remainder,
            &chunk,
            &prompt_tokens,
            &completion_tokens,
            &has_sse_error,
            &has_text_delta,
            &has_tool_calls,
            Some("gpt-test"),
            &mut done_state,
        );

        assert!(has_text_delta.load(Ordering::Relaxed));
        assert!(has_tool_calls.load(Ordering::Relaxed));
        assert!(output.is_none());
        assert!(!done_state.appended_model_info);
    }

    #[test]
    fn stream_chunk_detects_message_level_tool_and_function_calls() {
        let message_tool_calls = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{}"}
                    }]
                }
            }]
        });
        assert!(stream_chunk_has_tool_calls(&message_tool_calls));

        let message_function_call = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "function_call": {"name": "get_weather", "arguments": "{}"}
                }
            }]
        });
        assert!(stream_chunk_has_tool_calls(&message_function_call));
    }

    #[test]
    fn disable_reasoning_rewrites_four_model_requests() {
        let models = [
            "deepseek-v4-falsh",
            "mimo-v2.5-pro",
            "qwen/qwen3.5-122b-a10b",
            "openai/gpt-oss-120b",
        ];

        for model in models {
            let mut body = serde_json::json!({
                "model": model,
                "thinking": true,
                "reasoning": { "effort": "high" },
                "reasoning_content": "顶层思维链",
                "reasoning_text": "顶层兼容思维链",
                "reasoning_details": "顶层详情思维链",
                "reasoning_effort": "high",
                "messages": [
                    {
                        "role": "user",
                        "content": "你好"
                    },
                    {
                        "role": "assistant",
                        "content": "历史回答",
                        "thinking": true,
                        "reasoning": { "effort": "medium" },
                        "reasoning_content": "历史思维链",
                        "reasoning_text": "历史兼容思维链",
                        "reasoning_details": "历史详情思维链",
                        "reasoning_effort": "medium"
                    }
                ]
            });

            apply_disable_reasoning(&mut body);

            let obj = body.as_object().expect("请求体必须是对象");
            assert!(!obj.contains_key("thinking"));
            assert!(!obj.contains_key("reasoning"));
            assert!(!obj.contains_key("reasoning_content"));
            assert!(!obj.contains_key("reasoning_text"));
            assert!(!obj.contains_key("reasoning_details"));
            assert!(!obj.contains_key("reasoning_effort"));

            let messages = body
                .get("messages")
                .and_then(Value::as_array)
                .expect("必须保留消息数组");
            let assistant = messages
                .get(1)
                .and_then(Value::as_object)
                .expect("必须保留助手消息");
            assert!(!assistant.contains_key("thinking"));
            assert!(!assistant.contains_key("reasoning"));
            assert!(!assistant.contains_key("reasoning_content"));
            assert!(!assistant.contains_key("reasoning_text"));
            assert!(!assistant.contains_key("reasoning_details"));
            assert!(!assistant.contains_key("reasoning_effort"));
            assert_eq!(
                assistant.get("content"),
                Some(&Value::String("历史回答".to_string()))
            );
        }
    }
}
