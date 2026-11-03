use crate::database::dao::PaginatedResult;
use crate::database::{Channel, ModelInfo};
use crate::error::AppError;
use crate::services::channel_service::{self, FetchModelsResult, ProbeResult, TestChannelResult};
use crate::AppState;
use serde::Deserialize;
use tauri::State;

#[derive(Deserialize)]
pub struct ModelCatalogMetaInput {
    pub model: String,
    #[serde(default)]
    pub display_name: String,
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
#[allow(dead_code)]
enum ModelsEndpointError {
    Network(String),
    Timeout(String),
    Auth(u16),
    Http(u16),
    Parse(String),
    Empty,
}

#[allow(dead_code)]
impl ModelsEndpointError {
    fn is_blocking(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Timeout(_) | Self::Auth(_))
    }

    fn message(&self) -> String {
        match self {
            Self::Network(msg) => format!("Network error: {msg}"),
            Self::Timeout(msg) => format!("Network timeout: {msg}"),
            Self::Auth(status) => format!("Authentication failed: HTTP {status}"),
            Self::Http(status) => format!("HTTP {status}"),
            Self::Parse(msg) => format!("Invalid model list response: {msg}"),
            Self::Empty => "Empty model list".to_string(),
        }
    }
}

#[derive(Clone)]
#[allow(dead_code)]
struct EndpointGuess {
    detected_type: String,
    corrected_base_url: String,
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
pub(crate) struct UpdateResponseMsParams {
    #[serde(rename = "channelId")]
    channel_id: String,
    #[serde(rename = "responseMs")]
    response_ms: String,
}

#[tauri::command]
pub fn update_channel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    params: crate::services::channel_service::UpdateChannelParams,
) -> Result<Channel, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.update_channel(params)
}

#[tauri::command]
pub fn update_channel_response_ms(
    app: crate::AppEventHandle,
    state: State<'_, AppState>,
    params: UpdateResponseMsParams,
) -> Result<(), AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.update_channel_response_ms(&params.channel_id, &params.response_ms)
}

#[tauri::command]
pub fn list_channels(
    app: crate::AppEventHandle,
    state: State<'_, AppState>,
) -> Result<Vec<Channel>, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.list_channels()
}

#[tauri::command]
pub fn list_channels_paginated(
    app: crate::AppEventHandle,
    state: State<'_, AppState>,
    page: i32,
    page_size: i32,
) -> Result<PaginatedResult<Channel>, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.list_channels_paginated(page, page_size)
}

#[tauri::command]
pub async fn create_channel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    params: CreateChannelParams,
) -> Result<Channel, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.create_channel(channel_service::CreateChannelParams {
        name: params.name,
        api_type: params.api_type,
        base_url: params.base_url,
        api_key: params.api_key,
        notes: params.notes,
        upstream_headers: params.upstream_headers,
    })
}

#[tauri::command]
pub async fn delete_channel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.delete_channel(id)
}

#[tauri::command]
pub async fn probe_url(
    url: String,
    api_type: Option<String>,
    api_key: Option<String>,
) -> Result<ProbeResult, AppError> {
    channel_service::probe_url(url, api_type, api_key).await
}

#[tauri::command]
pub async fn test_channel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<TestChannelResult, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.test_channel(&channel_id).await
}

#[tauri::command]
pub async fn test_channel_direct(
    params: channel_service::TestChannelDirectParams,
) -> Result<TestChannelResult, AppError> {
    Ok(channel_service::test_channel_direct(params).await)
}

#[tauri::command]
pub async fn fetch_models_direct(
    _state: State<'_, AppState>,
    api_type: String,
    base_url: String,
    api_key: String,
    verified: Option<bool>,
    upstream_headers: Option<String>,
) -> Result<FetchModelsResult, AppError> {
    channel_service::fetch_models_direct(api_type, base_url, api_key, verified, upstream_headers).await
}

#[tauri::command]
pub async fn fetch_models(
    app: crate::AppEventHandle,
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<FetchModelsResult, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.fetch_channel_models(channel_id).await
}

#[tauri::command]
pub async fn select_models(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    model_names: Vec<String>,
    available_models: Vec<ModelInfo>,
    catalog_meta: Vec<ModelCatalogMetaInput>,
) -> Result<(), AppError> {
    let catalog_meta: Vec<crate::database::ModelCatalogMetaInput> = catalog_meta
        .into_iter()
        .map(|item| crate::database::ModelCatalogMetaInput {
            model: item.model,
            display_name: item.display_name,
            provider_logo: item.provider_logo,
            release_date: item.release_date,
            model_meta_zh: item.model_meta_zh,
            model_meta_en: item.model_meta_en,
        })
        .collect();
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.select_channel_models(&channel_id, &model_names, &available_models, &catalog_meta)
}

/// Generate model list URL candidates: adapter standard + common variants
#[allow(dead_code)]
fn build_models_url_variants(
    adapter: &(dyn crate::proxy::protocol::ProtocolAdapter + Send + Sync),
    base_url: &str,
    api_key: &str,
) -> Vec<String> {
    let mut urls = vec![adapter.build_models_url(base_url, api_key)];
    let base = base_url.trim_end_matches('/');
    for v in &[
        "/models",
        "/v1/models",
        "/api/models",
        "/api/v1/models",
        "/v2/models",
    ] {
        let u = format!("{base}{v}");
        if !urls.contains(&u) {
            urls.push(u);
        }
    }
    urls
}

/// Try a single models endpoint, return parsed models or a structured endpoint error.
#[allow(dead_code)]
async fn try_models_endpoint(
    client: &reqwest::Client,
    adapter: &(dyn crate::proxy::protocol::ProtocolAdapter + Send + Sync),
    url: &str,
    api_key: &str,
) -> Result<Vec<ModelInfo>, ModelsEndpointError> {
    let resp = adapter
        .apply_auth(client.get(url), api_key)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                ModelsEndpointError::Timeout(e.to_string())
            } else if e.is_connect() || e.is_request() {
                ModelsEndpointError::Network(e.to_string())
            } else {
                ModelsEndpointError::Network(e.to_string())
            }
        })?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ModelsEndpointError::Auth(status.as_u16()));
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

/// Try to extract model list from a JSON body (even error responses)
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

/// Chat probe: send a tiny request to verify the API works
#[allow(dead_code)]
async fn try_chat_probe(
    client: &reqwest::Client,
    adapter: &(dyn crate::proxy::protocol::ProtocolAdapter + Send + Sync),
    base_url: &str,
    api_key: &str,
    api_type: &str,
) -> Option<ProbeSuccess> {
    let test_model = match api_type {
        "claude" => "claude-3-5-sonnet-20241022",
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
                    crate::services::channel_service::canonical_base_url_for_success(
                        api_type, base_url, &chat_url,
                    );
                // Server responded -> API works, return known models
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

/// Commonly known models per API type
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
        "claude" => &[
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

#[allow(dead_code)]
fn dedup_models(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|m| !m.id.eq_ignore_ascii_case("auto") && !m.name.eq_ignore_ascii_case("auto"))
        .filter(|m| seen.insert(m.name.clone()))
        .collect()
}

#[tauri::command]
pub async fn save_channel_with_models(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    params: channel_service::SaveChannelWithModelsParams,
) -> Result<channel_service::SaveChannelWithModelsResult, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.save_channel_with_models(params)
}
