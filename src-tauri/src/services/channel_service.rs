use crate::admin::{
    ERROR_CODE_EMPTY_MODEL_LIST, ERROR_CODE_ENDPOINT_CORRECTION_FAILED,
    ERROR_CODE_ENDPOINT_UNREACHABLE, ERROR_CODE_FETCH_MODELS_FAILED, ERROR_CODE_HTTP_CLIENT_ERROR,
    ERROR_CODE_INVALID_CREDENTIALS, ERROR_CODE_INVALID_URL, ERROR_CODE_RATE_LIMITED,
    ERROR_CODE_TIMEOUT, ERROR_CODE_UNSUPPORTED_PROVIDER,
};
use crate::database::dao::PaginatedResult;
use crate::database::{Channel, Database, ModelInfo};
use crate::error::AppError;
use crate::proxy::protocol::get_adapter;
use crate::services::api_key_utils::primary_api_key;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct ModelCatalogMetaInput {
    pub model: String,
    pub provider_logo: String,
    pub release_date: String,
    pub model_meta_zh: String,
    pub model_meta_en: String,
}

#[derive(Clone)]
#[allow(dead_code)]
struct ProbeSuccess {
    models: Vec<ModelInfo>,
    corrected_base_url: String,
}

#[derive(Debug, Clone)]
enum ModelsEndpointError {
    Network(String),
    Timeout(String),
    Auth(u16),
    RateLimited(u16),
    Http(u16),
    Parse(String),
    Empty,
}

impl ModelsEndpointError {
    fn is_blocking(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Timeout(_))
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Network(_) => ERROR_CODE_ENDPOINT_UNREACHABLE,
            Self::Timeout(_) => ERROR_CODE_TIMEOUT,
            Self::Auth(_) => ERROR_CODE_INVALID_CREDENTIALS,
            Self::RateLimited(_) => ERROR_CODE_RATE_LIMITED,
            Self::Http(_) => ERROR_CODE_UNSUPPORTED_PROVIDER,
            Self::Parse(_) => ERROR_CODE_UNSUPPORTED_PROVIDER,
            Self::Empty => ERROR_CODE_EMPTY_MODEL_LIST,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Network(msg) => format!("Endpoint unreachable: {msg}"),
            Self::Timeout(msg) => format!("Endpoint timeout: {msg}"),
            Self::Auth(status) => format!("Invalid credentials: HTTP {status}"),
            Self::RateLimited(status) => format!("Rate limited: HTTP {status}"),
            Self::Http(status) => format!("Provider returned unsupported response: HTTP {status}"),
            Self::Parse(msg) => format!("Provider returned unsupported response: {msg}"),
            Self::Empty => "Provider returned an empty model list".to_string(),
        }
    }

    fn to_operation_error(&self) -> ChannelOperationError {
        let details = match self {
            Self::Auth(status) | Self::Http(status) | Self::RateLimited(status) => {
                Some(serde_json::json!({ "status_code": status }))
            }
            Self::Network(raw_message) | Self::Timeout(raw_message) | Self::Parse(raw_message) => {
                Some(serde_json::json!({ "raw_message": raw_message }))
            }
            Self::Empty => None,
        };

        let mut error = ChannelOperationError::new(self.code(), self.message());
        error.details = details;
        error
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EndpointGuess {
    detected_type: String,
    corrected_base_url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointCandidateScope {
    UserUrl,
    BaseSite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointCandidateStatus {
    Usable,
    Reachable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EndpointCandidate {
    guess: EndpointGuess,
    scope: EndpointCandidateScope,
    status: EndpointCandidateStatus,
}

struct ScopedBaseUrlCandidates {
    user_urls: Vec<String>,
    base_sites: Vec<String>,
}

#[derive(Deserialize)]
pub struct CreateChannelParams {
    pub name: String,
    pub api_type: String,
    pub base_url: String,
    pub api_key: String,
    pub notes: Option<String>,
    pub upstream_headers: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateChannelParams {
    pub id: String,
    pub name: Option<String>,
    pub api_type: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub enabled: Option<bool>,
    pub notes: Option<String>,
    pub upstream_headers: Option<Option<String>>,
}

#[derive(Deserialize)]
pub(crate) struct UpdateResponseMsParams {
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "responseMs")]
    pub response_ms: String,
}

#[derive(Serialize)]
pub struct ChannelOperationError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ChannelOperationError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[derive(Serialize)]
pub struct ProbeResult {
    pub reachable: bool,
    pub status_code: Option<u16>,
    pub latency_ms: u64,
    pub detected_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrected_base_url: Option<String>,
    #[serde(default)]
    pub available_types: Vec<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ChannelOperationError>,
}

#[derive(Serialize)]
pub struct FetchModelsResult {
    pub detected_type: String,
    pub corrected_base_url: String,
    pub models: Vec<ModelInfo>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ChannelOperationError>,
    #[serde(default)]
    pub endpoint_corrected: bool,
    #[serde(default)]
    pub auto_saved: bool,
}

// Service functions - thin wrappers around existing logic

#[derive(Serialize)]
pub struct TestChannelResult {
    pub success: bool,
    pub latency_ms: u64,
    pub status_code: Option<u16>,
    pub message: String,
}

#[derive(Deserialize)]
pub struct TestChannelDirectParams {
    pub api_type: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub upstream_headers: Option<String>,
}

#[derive(Deserialize)]
pub struct SaveChannelWithModelsParams {
    pub id: Option<String>,
    pub name: String,
    pub api_type: String,
    pub base_url: String,
    pub api_key: String,
    pub notes: Option<String>,
    pub upstream_headers: Option<String>,
    pub enabled: Option<bool>,
    pub selected_models: Vec<String>,
    pub available_models: Vec<ModelInfo>,
    pub catalog_meta: Option<Vec<crate::database::ModelCatalogMetaInput>>,
    pub response_ms: Option<String>,
}

#[derive(Serialize)]
pub struct SaveChannelWithModelsResult {
    pub channel: Channel,
    pub models_synced: bool,
    pub response_ms_updated: bool,
    pub entries_changed: bool,
    pub warnings: Vec<String>,
}
pub fn update_channel_response_ms(
    db: &Database,
    params: UpdateResponseMsParams,
) -> Result<(), AppError> {
    db.update_channel_response_ms(&params.channel_id, &params.response_ms)?;
    crate::state_version::bump("channel");
    Ok(())
}

pub fn list_channels(db: &Database) -> Result<Vec<Channel>, AppError> {
    db.list_channels()
}

pub fn list_channels_paginated(
    db: &Database,
    page: i32,
    page_size: i32,
) -> Result<PaginatedResult<Channel>, AppError> {
    db.list_channels_paginated(page, page_size)
}

pub fn create_channel(db: &Database, params: CreateChannelParams) -> Result<Channel, AppError> {
    let channel = db.create_channel(
        &params.name,
        &params.api_type,
        &params.base_url,
        &params.api_key,
        params.notes.as_deref(),
        params.upstream_headers.as_deref(),
    )?;
    crate::state_version::bump("channel");
    Ok(channel)
}

pub fn update_channel(
    db: &Database,
    app: Option<&crate::AppEventHandle>,
    params: UpdateChannelParams,
) -> Result<Channel, AppError> {
    if let Some(false) = params.enabled {
        db.disable_entries_for_channel(&params.id)?;
    }
    db.update_channel(
        &params.id,
        params.name.as_deref(),
        params.api_type.as_deref(),
        params.base_url.as_deref(),
        params.api_key.as_deref(),
        params.enabled,
        params.notes.as_deref(),
        params.upstream_headers.as_ref().map(|value| value.as_deref()),
    )?;
    if let Some(app) = app {
        crate::event::emit(app, "channels-changed");
    }
    crate::state_version::bump("channel");
    db.get_channel(&params.id)
}

pub fn delete_channel(
    db: &Database,
    app: Option<&crate::AppEventHandle>,
    id: String,
) -> Result<(), AppError> {
    db.delete_channel(&id)?;
    if let Some(app) = app {
        crate::event::emit(app, "channels-changed");
    }
    crate::state_version::bump("channel");
    crate::state_version::bump("pool");
    Ok(())
}

pub async fn probe_url(
    url: String,
    api_type: Option<String>,
    api_key: Option<String>,
) -> Result<ProbeResult, AppError> {
    let url = url.trim_end_matches('/').trim();
    if url.is_empty() {
        return Ok(ProbeResult {
            reachable: false,
            status_code: None,
            latency_ms: 0,
            detected_type: None,
            corrected_base_url: None,
            available_types: Vec::new(),
            message: "Empty URL".into(),
            error: Some(ChannelOperationError::new(
                ERROR_CODE_INVALID_URL,
                "Empty URL",
            )),
        });
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| AppError::Network(format!("HTTP client: {e}")))?;

    let start = std::time::Instant::now();
    if let (Some(api_type), Some(api_key)) = (api_type.as_deref(), api_key.as_deref()) {
        if !api_key.trim().is_empty() {
            let api_key = primary_api_key(api_key);
            let (primary_guess, all_available) =
                detect_endpoint_and_collect(api_type, url, api_key).await;
            if let Some(guess) = primary_guess {
                return Ok(ProbeResult {
                    reachable: true,
                    status_code: Some(200),
                    latency_ms: start.elapsed().as_millis() as u64,
                    detected_type: Some(guess.detected_type),
                    message: format!("API calibrated ({})", guess.corrected_base_url),
                    corrected_base_url: Some(guess.corrected_base_url),
                    available_types: all_available,
                    error: None,
                });
            }
            if !all_available.is_empty() {
                return Ok(ProbeResult {
                    reachable: true,
                    status_code: Some(200),
                    latency_ms: start.elapsed().as_millis() as u64,
                    detected_type: Some(all_available[0].clone()),
                    corrected_base_url: None,
                    available_types: all_available,
                    error: None,
                    message: "Endpoint reachable via alternative protocol".into(),
                });
            }
        }
    }

    match client.head(url).send().await {
        Ok(r) => {
            let s = r.status().as_u16();
            let ms = start.elapsed().as_millis() as u64;
            Ok(ProbeResult {
                reachable: s < 500,
                status_code: Some(s),
                latency_ms: ms,
                detected_type: None,
                corrected_base_url: None,
                available_types: Vec::new(),
                message: format!("{s} ({ms}ms)"),
                error: None,
            })
        }
        Err(_) => {
            let _start2 = std::time::Instant::now();
            match client.get(url).send().await {
                Ok(r) => {
                    let s = r.status().as_u16();
                    let ms = start.elapsed().as_millis() as u64;
                    Ok(ProbeResult {
                        reachable: s < 500,
                        status_code: Some(s),
                        latency_ms: ms,
                        detected_type: None,
                        corrected_base_url: None,
                        available_types: Vec::new(),
                        message: format!("{s} ({ms}ms)"),
                        error: None,
                    })
                }
                Err(e) => {
                    let ms = start.elapsed().as_millis() as u64;
                    let error = if e.is_timeout() {
                        ChannelOperationError::new(ERROR_CODE_TIMEOUT, e.to_string())
                    } else {
                        ChannelOperationError::new(ERROR_CODE_ENDPOINT_UNREACHABLE, e.to_string())
                    }
                    .with_details(serde_json::json!({ "url": url }));
                    Ok(ProbeResult {
                        reachable: false,
                        status_code: None,
                        latency_ms: ms,
                        detected_type: None,
                        corrected_base_url: None,
                        available_types: Vec::new(),
                        message: e.to_string(),
                        error: Some(error),
                    })
                }
            }
        }
    }
}

pub async fn fetch_models_direct(
    api_type: String,
    base_url: String,
    api_key: String,
    verified: Option<bool>,
    upstream_headers: Option<String>,
) -> Result<FetchModelsResult, AppError> {
    let base_url = normalize_base_url(&base_url);
    if base_url.is_empty() {
        return Ok(FetchModelsResult {
            detected_type: api_type,
            corrected_base_url: base_url,
            models: Vec::new(),
            message: "Empty URL".into(),
            warning: None,
            error: Some(ChannelOperationError::new(
                ERROR_CODE_INVALID_URL,
                "Empty URL",
            )),
            endpoint_corrected: false,
            auto_saved: false,
        });
    }
    smart_fetch_models(
        &api_type,
        &base_url,
        primary_api_key(&api_key),
        verified.unwrap_or(false),
        upstream_headers.as_deref(),
    )
    .await
    .map_err(|e| AppError::Network(e.message))
}

pub async fn fetch_models(
    db: &Database,
    channel_id: String,
) -> Result<FetchModelsResult, AppError> {
    let channel = db.get_channel(&channel_id)?;
    let original_base_url = normalize_base_url(&channel.base_url);
    let endpoint_guess = detect_endpoint_guess(
        &channel.api_type,
        &channel.base_url,
        primary_api_key(&channel.api_key),
    )
    .await;
    let Some(guess) = endpoint_guess else {
        return Ok(FetchModelsResult {
            detected_type: channel.api_type,
            corrected_base_url: original_base_url,
            models: Vec::new(),
            message: "Could not validate endpoint. Check network, URL, API type, and API key."
                .into(),
            warning: None,
            error: Some(ChannelOperationError::new(
                ERROR_CODE_ENDPOINT_CORRECTION_FAILED,
                "Could not validate endpoint. Check network, URL, API type, and API key.",
            )),
            endpoint_corrected: false,
            auto_saved: false,
        });
    };

    let endpoint_corrected =
        channel.api_type != guess.detected_type || original_base_url != guess.corrected_base_url;

    let result = match fetch_models_result_with_fallback(
        &guess.detected_type,
        &guess.corrected_base_url,
        primary_api_key(&channel.api_key),
        channel.upstream_headers.as_deref(),
    )
    .await
    {
        Ok((models, _actual_type, _actual_base_url)) => {
            let count = models.len();
            let message = if endpoint_corrected {
                format!(
                    "Detected: {} ({count} models). Endpoint correction is ready to save.",
                    guess.detected_type
                )
            } else {
                format!("Detected: {} ({count} models)", guess.detected_type)
            };
            FetchModelsResult {
                detected_type: guess.detected_type.clone(),
                corrected_base_url: guess.corrected_base_url.clone(),
                models,
                message,
                warning: endpoint_corrected.then(|| {
                    format!(
                        "Endpoint correction suggested: {} {} -> {} {}. Review and save manually.",
                        channel.api_type,
                        original_base_url,
                        guess.detected_type,
                        guess.corrected_base_url
                    )
                }),
                error: None,
                endpoint_corrected,
                auto_saved: false,
            }
        }
        Err(error) => {
            let warning = endpoint_corrected.then(|| {
                format!(
                    "Endpoint correction suggested: {} {} -> {} {}. Models were not saved automatically because fetch failed.",
                    channel.api_type,
                    original_base_url,
                    guess.detected_type,
                    guess.corrected_base_url
                )
            });
            return Ok(FetchModelsResult {
                detected_type: guess.detected_type.clone(),
                corrected_base_url: guess.corrected_base_url.clone(),
                models: Vec::new(),
                message: error.message.clone(),
                warning,
                error: Some(error),
                endpoint_corrected,
                auto_saved: false,
            });
        }
    };

    db.update_channel_models(&channel_id, &result.models, &[])?;
    Ok(result)
}

// The remaining helper functions are required for model fetch flows.

async fn smart_fetch_models(
    api_type: &str,
    base_url: &str,
    api_key: &str,
    _verified: bool,
    upstream_headers: Option<&str>,
) -> Result<FetchModelsResult, ChannelOperationError> {
    let base_url = normalize_base_url(base_url);
    let normalized_type = normalize_api_type(api_type);
    let (models, actual_type, actual_base_url) =
        fetch_models_result_with_fallback(normalized_type, &base_url, api_key, upstream_headers).await?;
    let count = models.len();

    Ok(FetchModelsResult {
        message: format!("Fetched: {actual_type} ({count} models)"),
        detected_type: actual_type.to_string(),
        corrected_base_url: actual_base_url,
        models,
        warning: None,
        error: None,
        endpoint_corrected: false,
        auto_saved: false,
    })
}

async fn detect_endpoint_guess(
    api_type: &str,
    base_url: &str,
    api_key: &str,
) -> Option<EndpointGuess> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .danger_accept_invalid_certs(true)
        .build()
        .ok()?;

    let candidates = detect_endpoint_candidates(&client, api_type, base_url, api_key).await;
    select_preferred_endpoint_candidate(&candidates).map(|candidate| candidate.guess)
}

async fn detect_endpoint_and_collect(
    api_type: &str,
    base_url: &str,
    api_key: &str,
) -> (Option<EndpointGuess>, Vec<String>) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, Vec::new()),
    };

    let candidates = detect_endpoint_candidates(&client, api_type, base_url, api_key).await;
    let primary_guess =
        select_preferred_endpoint_candidate(&candidates).map(|candidate| candidate.guess);
    let all_found_types = collect_reachable_endpoint_types(&candidates);

    (primary_guess, all_found_types)
}

async fn fetch_models_result_with_fallback(
    preferred_type: &str,
    preferred_base_url: &str,
    api_key: &str,
    upstream_headers: Option<&str>,
) -> Result<(Vec<ModelInfo>, &'static str, String), ChannelOperationError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| {
            ChannelOperationError::new(ERROR_CODE_HTTP_CLIENT_ERROR, format!("HTTP client: {e}"))
        })?;

    let current_type = normalize_api_type(preferred_type);
    let adapter = get_adapter(current_type);
    let mut last_error: Option<ChannelOperationError> = None;
    let mut merged_models = Vec::new();

    for candidate_base_url in build_base_url_candidates_for_type(current_type, preferred_base_url) {
        for url in build_models_url_variants_for_type(
            current_type,
            adapter.as_ref(),
            &candidate_base_url,
            api_key,
        ) {
            match try_models_endpoint(
                &client,
                adapter.as_ref(),
                &url,
                api_key,
                current_type,
                upstream_headers,
            )
            .await
            {
                Ok(models) if !models.is_empty() => {
                    merged_models.extend(models);
                }
                Ok(_) => {}
                Err(err) if err.is_blocking() => {
                    last_error = Some(err.to_operation_error().with_details(serde_json::json!({
                        "api_type": current_type,
                        "base_url": candidate_base_url,
                        "models_url": url,
                    })));
                }
                Err(err) => {
                    last_error = Some(err.to_operation_error().with_details(serde_json::json!({
                        "api_type": current_type,
                        "base_url": candidate_base_url,
                        "models_url": url,
                    })));
                }
            }
        }
    }

    let models = dedup_models(merged_models);
    if !models.is_empty() {
        log::info!(
            "[fetch_models] OK type={current_type}, base_url={} ({} models)",
            preferred_base_url,
            models.len()
        );
        return Ok((models, current_type, normalize_base_url(preferred_base_url)));
    }

    Err(last_error.unwrap_or_else(|| {
        ChannelOperationError::new(
            ERROR_CODE_FETCH_MODELS_FAILED,
            "Could not fetch models. Check URL and API Key.",
        )
    }))
}

enum DetectionResult {
    Found(EndpointCandidate),
    NotFound,
}

async fn detect_endpoint_candidates(
    client: &reqwest::Client,
    api_type: &str,
    base_url: &str,
    api_key: &str,
) -> Vec<EndpointCandidate> {
    let scoped_candidates = build_scoped_base_url_candidates(base_url);
    let api_types = endpoint_probe_api_types(api_type);
    let selected_api_type = normalize_api_type(api_type);
    let mut tasks = Vec::new();

    for scope in [
        EndpointCandidateScope::UserUrl,
        EndpointCandidateScope::BaseSite,
    ] {
        let base_urls = match scope {
            EndpointCandidateScope::UserUrl => &scoped_candidates.user_urls,
            EndpointCandidateScope::BaseSite => &scoped_candidates.base_sites,
        };

        for base_url in base_urls {
            for current_type in &api_types {
                let current_type = *current_type;
                let candidate_base_url = base_url.clone();
                let respect_selected_type = current_type == selected_api_type;
                tasks.push(async move {
                    detect_type_with_base_url(
                        client,
                        current_type,
                        &candidate_base_url,
                        api_key,
                        respect_selected_type,
                        scope,
                    )
                    .await
                });
            }
        }
    }

    futures::future::join_all(tasks)
        .await
        .into_iter()
        .filter_map(|result| match result {
            DetectionResult::Found(candidate) => Some(candidate),
            DetectionResult::NotFound => None,
        })
        .collect()
}

fn endpoint_probe_api_types(selected_api_type: &str) -> Vec<&'static str> {
    let selected = normalize_api_type(selected_api_type);
    let mut types = vec![selected];
    for api_type in ["openai", "responses", "anthropic", "gemini", "azure"] {
        if !types.contains(&api_type) {
            types.push(api_type);
        }
    }
    types
}

async fn detect_type_with_base_url(
    client: &reqwest::Client,
    api_type: &str,
    base_url: &str,
    api_key: &str,
    respect_selected_type: bool,
    scope: EndpointCandidateScope,
) -> DetectionResult {
    let adapter = get_adapter(api_type);
    let urls = build_models_url_variants_for_type(api_type, adapter.as_ref(), base_url, api_key);
    let mut reachable_candidate: Option<EndpointCandidate> = None;

    for url in &urls {
        match try_models_endpoint(client, adapter.as_ref(), url, api_key, api_type, None).await {
            Ok(models) => {
                if !models.is_empty() {
                    if !is_authoritative_detection_success(api_type, url) {
                        continue;
                    }
                    let corrected_base_url =
                        canonical_base_url_for_success(api_type, base_url, url);
                    let detected_type = if respect_selected_type {
                        api_type.to_string()
                    } else {
                        resolve_detected_type(api_type, &corrected_base_url)
                    };
                    log::info!(
                        "[detect_endpoint] OK via {}, type={detected_type}, base_url={corrected_base_url}",
                        sanitize_url_for_log(url)
                    );
                    return DetectionResult::Found(EndpointCandidate {
                        guess: EndpointGuess {
                            detected_type,
                            corrected_base_url,
                        },
                        scope,
                        status: EndpointCandidateStatus::Usable,
                    });
                }
            }
            Err(ModelsEndpointError::Network(_)) | Err(ModelsEndpointError::Timeout(_)) => {}
            Err(err) => {
                if reachable_candidate.is_none() {
                    let corrected_base_url =
                        canonical_base_url_for_success(api_type, base_url, url);
                    let detected_type = if respect_selected_type {
                        api_type.to_string()
                    } else {
                        resolve_detected_type(api_type, &corrected_base_url)
                    };
                    log::info!(
                        "[detect_endpoint] Endpoint reachable via {}, type={detected_type} ({})",
                        sanitize_url_for_log(url),
                        err.message()
                    );
                    reachable_candidate = Some(EndpointCandidate {
                        guess: EndpointGuess {
                            detected_type,
                            corrected_base_url,
                        },
                        scope,
                        status: EndpointCandidateStatus::Reachable,
                    });
                }
            }
        }
    }

    reachable_candidate
        .map(DetectionResult::Found)
        .unwrap_or(DetectionResult::NotFound)
}

fn normalize_api_type(api_type: &str) -> &'static str {
    match api_type {
        "custom" | "openai" => "openai",
        "responses" => "responses",
        "claude" | "anthropic" => "anthropic",
        "gemini" => "gemini",
        "azure" => "azure",
        _ => "openai",
    }
}

fn select_preferred_endpoint_candidate(
    candidates: &[EndpointCandidate],
) -> Option<EndpointCandidate> {
    [
        (
            EndpointCandidateScope::UserUrl,
            EndpointCandidateStatus::Usable,
        ),
        (
            EndpointCandidateScope::UserUrl,
            EndpointCandidateStatus::Reachable,
        ),
        (
            EndpointCandidateScope::BaseSite,
            EndpointCandidateStatus::Usable,
        ),
        (
            EndpointCandidateScope::BaseSite,
            EndpointCandidateStatus::Reachable,
        ),
    ]
    .into_iter()
    .find_map(|(scope, status)| {
        candidates
            .iter()
            .find(|candidate| candidate.scope == scope && candidate.status == status)
            .cloned()
    })
}

fn collect_reachable_endpoint_types(candidates: &[EndpointCandidate]) -> Vec<String> {
    let mut types = Vec::new();
    for candidate in candidates {
        let detected_type = candidate.guess.detected_type.clone();
        if !types.contains(&detected_type) {
            types.push(detected_type);
        }
    }
    types
}

fn build_scoped_base_url_candidates(base_url: &str) -> ScopedBaseUrlCandidates {
    let normalized = normalize_base_url(base_url);
    let base_site = extract_base_site(&normalized).unwrap_or_else(|| normalized.clone());

    ScopedBaseUrlCandidates {
        user_urls: build_user_url_candidates(&normalized),
        base_sites: build_site_url_candidates(&base_site),
    }
}

fn build_user_url_candidates(base_url: &str) -> Vec<String> {
    let normalized = normalize_base_url(base_url);
    let mut candidates = Vec::new();
    push_unique_url_candidate(&mut candidates, normalized.clone());

    if normalized.to_ascii_lowercase().ends_with("/v1") {
        let without_v1 = normalized[..normalized.len() - 3]
            .trim_end_matches('/')
            .to_string();
        push_unique_url_candidate(&mut candidates, without_v1);
    } else if !normalized.is_empty() {
        push_unique_url_candidate(
            &mut candidates,
            format!("{}/v1", normalized.trim_end_matches('/')),
        );
    }

    candidates
}

fn build_site_url_candidates(base_site: &str) -> Vec<String> {
    let normalized = normalize_base_url(base_site);
    let mut candidates = Vec::new();
    push_unique_url_candidate(&mut candidates, normalized.clone());

    if normalized.to_ascii_lowercase().ends_with("/v1") {
        let without_v1 = normalized[..normalized.len() - 3]
            .trim_end_matches('/')
            .to_string();
        push_unique_url_candidate(&mut candidates, without_v1);
    } else if !normalized.is_empty() {
        push_unique_url_candidate(
            &mut candidates,
            format!("{}/v1", normalized.trim_end_matches('/')),
        );
    }

    candidates
}

fn push_unique_url_candidate(candidates: &mut Vec<String>, candidate: String) {
    if !candidate.is_empty() && !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

fn sanitize_url_for_log(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

fn is_authoritative_detection_success(api_type: &str, success_url: &str) -> bool {
    match api_type {
        "gemini" => {
            success_url.contains("/v1beta/openai/") || success_url.contains("/v1beta/models")
        }
        "azure" => success_url.contains("/openai/deployments"),
        _ => true,
    }
}

fn normalize_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.is_empty() {
        String::new()
    } else {
        format!("https://{trimmed}")
    }
}

fn build_base_url_candidates(base_url: &str) -> Vec<String> {
    let normalized = normalize_base_url(base_url);
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for candidate in [normalized.clone(), trim_known_api_suffix(&normalized)] {
        if !candidate.is_empty() && seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }

    if let Some(scheme_end) = normalized.find("://") {
        let after_scheme = &normalized[scheme_end + 3..];
        if let Some(slash) = after_scheme.find('/') {
            let base_site = format!("{}://{}", &normalized[..scheme_end], &after_scheme[..slash]);
            if seen.insert(base_site.clone()) {
                candidates.push(base_site);
            }
        }
    }

    candidates
}

fn build_base_url_candidates_for_type(api_type: &str, base_url: &str) -> Vec<String> {
    if api_type == "openai" {
        return build_base_url_candidates(base_url);
    }

    let normalized = normalize_base_url(base_url);
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let base_site = extract_base_site(&normalized).unwrap_or_else(|| normalized.clone());
    for candidate in [normalized, base_site] {
        if !candidate.is_empty() && seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn extract_base_site(base_url: &str) -> Option<String> {
    let normalized = normalize_base_url(base_url);
    let scheme_end = normalized.find("://")?;
    let after_scheme = &normalized[scheme_end + 3..];
    if let Some(slash) = after_scheme.find('/') {
        Some(format!(
            "{}://{}",
            &normalized[..scheme_end],
            &after_scheme[..slash]
        ))
    } else {
        Some(normalized)
    }
}

pub(crate) fn canonical_base_url_for_success(
    api_type: &str,
    fallback_base_url: &str,
    success_url: &str,
) -> String {
    let success = success_url.trim();
    let success_lower = success.to_ascii_lowercase();

    if api_type == "gemini" {
        if let Some(idx) = success_lower.find("/v1beta/openai/") {
            let base = &success[..idx];
            return base.trim_end_matches('/').to_string();
        }
    }

    if api_type == "claude" || api_type == "anthropic" {
        if let Some(idx) = success_lower.find("/v1/") {
            let base = &success[..idx];
            return base.trim_end_matches('/').to_string();
        }
    }

    if api_type == "azure" {
        if let Some(idx) = success_lower.find("/openai/deployments") {
            let base = &success[..idx];
            return base.trim_end_matches('/').to_string();
        }
    }

    if api_type == "openai" {
        if let Some(idx) = success_lower.find("/v1/") {
            let base = &success[..idx + 3]; // include "/v1"
            return base.trim_end_matches('/').to_string();
        }
    }

    if api_type == "custom" {
        for suffix in ["/models", "/chat/completions"] {
            if success_lower.ends_with(suffix) {
                let stripped = &success[..success.len() - suffix.len()];
                return stripped.trim_end_matches('/').to_string();
            }
        }
    }

    normalize_base_url(fallback_base_url)
}

fn trim_known_api_suffix(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let lower = base.to_ascii_lowercase();
    let suffixes = [
        "/v1/chat/completions",
        "/chat/completions",
        "/v1/messages",
        "/v1/models",
        "/models",
        "/v1beta/openai/chat/completions",
        "/v1beta/openai/models",
        "/openai/deployments",
    ];
    for suffix in suffixes {
        if lower.ends_with(suffix) {
            let stripped = &base[..base.len() - suffix.len()];
            return stripped.trim_end_matches('/').to_string();
        }
    }
    base.to_string()
}

fn resolve_detected_type(detected: &str, base_url: &str) -> String {
    let _ = base_url;
    detected.into()
}

fn build_models_url_variants_for_type(
    api_type: &str,
    adapter: &(dyn crate::proxy::protocol::ProtocolAdapter + Send + Sync),
    base_url: &str,
    api_key: &str,
) -> Vec<String> {
    let mut urls = Vec::new();
    let base = base_url.trim_end_matches('/');
    let mut push = |url: String| {
        if !urls.contains(&url) {
            urls.push(url);
        }
    };

    push(adapter.build_models_url(base_url, api_key));
    if api_type == "openai" {
        for suffix in [
            "/models",
            "/v1/models",
            "/api/models",
            "/api/v1/models",
            "/v2/models",
        ] {
            push(format!("{base}{suffix}"));
        }
    }

    if api_type == "gemini" {
        push(format!("{base}/v1beta/models"));
    }

    urls
}

async fn try_models_endpoint(
    client: &reqwest::Client,
    adapter: &(dyn crate::proxy::protocol::ProtocolAdapter + Send + Sync),
    url: &str,
    api_key: &str,
    _api_type: &str,
    upstream_headers: Option<&str>,
) -> Result<Vec<ModelInfo>, ModelsEndpointError> {
    let req = crate::services::upstream_headers::apply_upstream_headers(
        adapter.apply_auth(client.get(url), api_key),
        upstream_headers,
    )
    .map_err(|e| ModelsEndpointError::Parse(e.to_string()))?;
    let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                ModelsEndpointError::Timeout(e.to_string())
            } else {
                ModelsEndpointError::Network(e.to_string())
            }
        })?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ModelsEndpointError::Auth(status.as_u16()));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ModelsEndpointError::RateLimited(status.as_u16()));
    }
    if status != reqwest::StatusCode::OK {
        return Err(ModelsEndpointError::Http(status.as_u16()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ModelsEndpointError::Parse(e.to_string()))?;
    let models: Vec<ModelInfo> = adapter
        .parse_models_response(&body)
        .into_iter()
        .map(|(id, owned_by)| ModelInfo {
            name: id.clone(),
            id,
            owned_by,
        })
        .collect();
    if models.is_empty() {
        Err(ModelsEndpointError::Empty)
    } else {
        Ok(models)
    }
}

#[allow(dead_code)]
fn extract_models_from_json(body: &str) -> Option<Vec<ModelInfo>> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let arr = json.get("data")?.as_array()?;
    let models: Vec<ModelInfo> = arr
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            if id.eq_ignore_ascii_case("auto") {
                return None;
            }
            let owned = m
                .get("owned_by")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            Some(ModelInfo {
                name: id.clone(),
                id,
                owned_by: Some(owned),
            })
        })
        .collect();
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

#[allow(dead_code)]
async fn try_chat_probe(
    client: &reqwest::Client,
    adapter: &(dyn crate::proxy::protocol::ProtocolAdapter + Send + Sync),
    base_url: &str,
    api_key: &str,
    api_type: &str,
) -> Option<ProbeSuccess> {
    let test_model = match api_type {
        "claude" | "anthropic" => "claude-3-5-sonnet-20241022",
        "gemini" => "gemini-2.0-flash",
        _ => "gpt-4o-mini",
    };
    let chat_url = adapter.build_chat_url(base_url, test_model);
    let body = serde_json::json!({"model": test_model, "messages": [{"role":"user","content":"hi"}], "max_tokens": 1});
    let req = adapter.apply_auth(
        client
            .post(&chat_url)
            .header("Content-Type", "application/json"),
        api_key,
    );
    match req.json(&body).send().await {
        Ok(resp) => {
            let s = resp.status().as_u16();
            if s < 500 {
                let corrected_base_url =
                    canonical_base_url_for_success(api_type, base_url, &chat_url);
                if let Ok(text) = resp.text().await {
                    if let Some(m) = extract_models_from_json(&text) {
                        return Some(ProbeSuccess {
                            models: m,
                            corrected_base_url,
                        });
                    }
                }
                return Some(ProbeSuccess {
                    models: known_models_for_type(api_type),
                    corrected_base_url,
                });
            }
            None
        }
        Err(_) => None,
    }
}

#[allow(dead_code)]
fn known_models_for_type(api_type: &str) -> Vec<ModelInfo> {
    let list: &[(&str, &str)] = match api_type {
        "openai" => &[
            ("gpt-4o", "openai"),
            ("gpt-4o-mini", "openai"),
            ("gpt-4-turbo", "openai"),
            ("gpt-3.5-turbo", "openai"),
            ("o1", "openai"),
            ("o1-mini", "openai"),
            ("o1-preview", "openai"),
            ("o3-mini", "openai"),
            ("o4-mini", "openai"),
        ],
        "claude" | "anthropic" => &[
            ("claude-sonnet-4-20250514", "anthropic"),
            ("claude-3-5-sonnet-20241022", "anthropic"),
            ("claude-3-5-haiku-20241022", "anthropic"),
            ("claude-3-opus-20240229", "anthropic"),
        ],
        "gemini" => &[
            ("gemini-2.5-pro-preview-05-06", "google"),
            ("gemini-2.0-flash", "google"),
            ("gemini-1.5-pro", "google"),
            ("gemini-1.5-flash", "google"),
        ],
        "azure" => &[
            ("gpt-4o", "azure"),
            ("gpt-4o-mini", "azure"),
            ("gpt-4-turbo", "azure"),
        ],
        _ => &[
            ("gpt-4o", "openai"),
            ("gpt-4o-mini", "openai"),
            ("gpt-3.5-turbo", "openai"),
            ("claude-3-5-sonnet-20241022", "anthropic"),
            ("claude-3-5-haiku-20241022", "anthropic"),
            ("gemini-2.0-flash", "google"),
            ("deepseek-chat", "deepseek"),
            ("deepseek-reasoner", "deepseek"),
            ("qwen-turbo", "alibaba"),
            ("glm-4-flash", "zhipu"),
        ],
    };
    list.iter()
        .map(|&(name, owner)| ModelInfo {
            name: name.into(),
            id: name.into(),
            owned_by: Some(owner.into()),
        })
        .collect()
}

fn dedup_models(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|m| !m.id.eq_ignore_ascii_case("auto") && !m.name.eq_ignore_ascii_case("auto"))
        .filter(|m| seen.insert(m.name.clone()))
        .collect()
}

/// Test a channel by actually chatting with the model.
/// Sends "just say OK"; treats an HTTP 200 as success.
/// Records latency and returns success/failure with HTTP status code.
pub async fn test_channel_chat(
    base_url: &str,
    api_key: &str,
    api_type: &str,
    model: &str,
    upstream_headers: Option<&str>,
) -> TestChannelResult {
    let start = std::time::Instant::now();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return TestChannelResult {
                success: false,
                latency_ms: 0,
                status_code: None,
                message: format!("Failed to create HTTP client: {}", e),
            };
        }
    };
    let adapter = get_adapter(api_type);
    let chat_url = adapter.build_chat_url(base_url, model);

    // Build an OpenAI-shaped probe, then let the adapter translate it into the
    // upstream's native format. Native-only upstreams (e.g. Anthropic
    // /v1/messages) hang on an OpenAI-shaped body, so this translation is what
    // makes Claude channels testable. Kept non-streaming: some upstreams stall
    // streaming probes.
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "just say OK"}
        ],
        "max_tokens": 10
    });
    adapter.transform_request(&mut body, model);

    let req = adapter.apply_auth(
        client
            .post(&chat_url)
            .header("Content-Type", "application/json"),
        api_key,
    );
    let req = match crate::services::upstream_headers::apply_upstream_headers(req, upstream_headers) {
        Ok(req) => req,
        Err(e) => {
            let latency = start.elapsed().as_millis() as u64;
            return TestChannelResult {
                success: false,
                latency_ms: latency,
                status_code: None,
                message: format!("Header configuration error: {e}"),
            };
        }
    };

    match req.json(&body).send().await {
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let latency = start.elapsed().as_millis() as u64;

            if status_code != 200 {
                return TestChannelResult {
                    success: false,
                    latency_ms: latency,
                    status_code: Some(status_code),
                    message: format!("HTTP {}", status_code),
                };
            }

            TestChannelResult {
                success: true,
                latency_ms: latency,
                status_code: Some(status_code),
                message: "OK".to_string(),
            }
        }
        Err(e) => {
            let latency = start.elapsed().as_millis() as u64;
            TestChannelResult {
                success: false,
                latency_ms: latency,
                status_code: None,
                message: format!("Request failed: {}", e),
            }
        }
    }
}

pub async fn test_channel_direct(params: TestChannelDirectParams) -> TestChannelResult {
    test_channel_chat(
        &params.base_url,
        primary_api_key(&params.api_key),
        normalize_api_type(&params.api_type),
        &params.model,
        params.upstream_headers.as_deref(),
    )
    .await
}

pub fn save_channel_with_models(
    db: &Database,
    app: Option<&crate::AppEventHandle>,
    params: SaveChannelWithModelsParams,
) -> Result<SaveChannelWithModelsResult, AppError> {
    let mut warnings = Vec::new();

    // 1. Create or Update channel
    let channel = if let Some(id) = &params.id {
        db.update_channel(
            id,
            Some(&params.name),
            Some(&params.api_type),
            Some(&params.base_url),
            Some(&params.api_key),
            params.enabled,
            params.notes.as_deref(),
            Some(params.upstream_headers.as_deref()),
        )?;
        db.get_channel(id)?
    } else {
        db.create_channel(
            &params.name,
            &params.api_type,
            &params.base_url,
            &params.api_key,
            params.notes.as_deref(),
            params.upstream_headers.as_deref(),
        )?
    };

    // 2. Update response_ms if provided
    let mut response_ms_updated = false;
    if let Some(ms) = params.response_ms {
        if let Err(e) = db.update_channel_response_ms(&channel.id, &ms) {
            warnings.push(format!("Failed to update response time: {e}"));
        } else {
            response_ms_updated = true;
        }
    }

    // 3. Sync models
    let mut models_synced = false;
    let catalog_meta = params.catalog_meta.unwrap_or_default();
    if let Err(e) = db.update_channel_models(
        &channel.id,
        &params.available_models,
        &params.selected_models,
    ) {
        warnings.push(format!("Failed to update channel model snapshot: {e}"));
    } else {
        // Sync to api_entries
        if let Err(e) = db.sync_entries_for_channel_with_meta(
            &channel.id,
            &params.selected_models,
            &catalog_meta,
        ) {
            warnings.push(format!("Failed to sync API pool entries: {e}"));
        } else {
            models_synced = true;
        }
    }

    // 4. Notifications & Dirty flags
    if let Some(app) = app {
        crate::event::emit(app, "channels-changed");
        crate::event::emit(app, "entries-changed");
    }
    crate::state_version::bump("channel");
    crate::state_version::bump("pool");

    Ok(SaveChannelWithModelsResult {
        channel,
        models_synced,
        response_ms_updated,
        entries_changed: true,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        scope: EndpointCandidateScope,
        status: EndpointCandidateStatus,
        api_type: &str,
        base_url: &str,
    ) -> EndpointCandidate {
        EndpointCandidate {
            guess: EndpointGuess {
                detected_type: api_type.to_string(),
                corrected_base_url: base_url.to_string(),
            },
            scope,
            status,
        }
    }

    #[test]
    fn select_prefers_user_url_reachable_over_base_site_usable() {
        let candidates = vec![
            candidate(
                EndpointCandidateScope::BaseSite,
                EndpointCandidateStatus::Usable,
                "openai",
                "https://a.com/v1",
            ),
            candidate(
                EndpointCandidateScope::UserUrl,
                EndpointCandidateStatus::Reachable,
                "openai",
                "https://a.com/xxx/yyy/v1",
            ),
        ];

        let selected = select_preferred_endpoint_candidate(&candidates).expect("Should select reachable candidate");

        assert_eq!(selected.scope, EndpointCandidateScope::UserUrl);
        assert_eq!(selected.status, EndpointCandidateStatus::Reachable);
        assert_eq!(
            selected.guess.corrected_base_url,
            "https://a.com/xxx/yyy/v1"
        );
    }

    #[test]
    fn build_user_url_candidates_preserves_extended_path_and_v1_variant() {
        let groups = build_scoped_base_url_candidates("https://a.com/xxx/yyy");

        assert_eq!(
            groups.user_urls,
            vec![
                "https://a.com/xxx/yyy".to_string(),
                "https://a.com/xxx/yyy/v1".to_string(),
            ]
        );
        assert_eq!(
            groups.base_sites,
            vec!["https://a.com".to_string(), "https://a.com/v1".to_string()]
        );
    }

    #[test]
    fn build_user_url_candidates_for_v1_input_also_tries_without_v1() {
        let groups = build_scoped_base_url_candidates("https://a.com/xxx/yyy/v1");

        assert_eq!(
            groups.user_urls,
            vec![
                "https://a.com/xxx/yyy/v1".to_string(),
                "https://a.com/xxx/yyy".to_string(),
            ]
        );
    }

    #[test]
    fn collect_reachable_types_marks_reachable_and_usable_protocols() {
        let candidates = vec![
            candidate(
                EndpointCandidateScope::UserUrl,
                EndpointCandidateStatus::Reachable,
                "openai",
                "https://a.com/xxx/yyy/v1",
            ),
            candidate(
                EndpointCandidateScope::BaseSite,
                EndpointCandidateStatus::Usable,
                "anthropic",
                "https://a.com",
            ),
            candidate(
                EndpointCandidateScope::BaseSite,
                EndpointCandidateStatus::Usable,
                "openai",
                "https://a.com/v1",
            ),
        ];

        let types = collect_reachable_endpoint_types(&candidates);

        assert_eq!(types, vec!["openai".to_string(), "anthropic".to_string()]);
    }
}
