use crate::database::dao::PaginatedResult;
use crate::database::{ApiEntry, Database, EntryCatalogMetaInput};
use crate::error::AppError;
use crate::proxy::protocol::get_adapter;
use crate::services::api_key_utils::primary_api_key;
use crate::services::log_service::{
    extract_usage_tokens, insert_test_usage_log, TestUsageLogInput,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

fn parse_latency_ms(value: &str) -> Option<f64> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || value == "x" {
        return None;
    }
    if let Some(ms) = value.strip_suffix("ms") {
        return ms.parse::<f64>().ok();
    }
    if let Some(seconds) = value.strip_suffix('s') {
        return seconds.parse::<f64>().ok().map(|s| s * 1000.0);
    }
    value.parse::<f64>().ok()
}

fn latency_score(value: Option<f64>) -> f64 {
    let Some(ms) = value else {
        return 0.0;
    };
    if ms <= 0.0 {
        return 0.0;
    }
    (100.0 * (1.0 - (ms / 1000.0) / 20.0)).clamp(0.0, 100.0)
}

fn calculate_entry_score(
    model_score: f64,
    channel_response_ms: &str,
    entry_response_ms: &str,
) -> f64 {
    let model_score = model_score.clamp(0.0, 100.0);
    let channel_score = latency_score(parse_latency_ms(channel_response_ms));
    let entry_score = latency_score(parse_latency_ms(entry_response_ms));
    (model_score * 0.6 + channel_score * 0.2 + entry_score * 0.2).clamp(0.0, 100.0)
}

#[derive(Deserialize)]
pub struct CreateEntryParams {
    pub channel_id: String,
    pub model: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub provider_logo: String,
    #[serde(default)]
    pub release_date: String,
    #[serde(default)]
    pub model_meta_zh: String,
    #[serde(default)]
    pub model_meta_en: String,
    #[serde(default)]
    pub group_name: Option<String>,
}

#[derive(Deserialize)]
pub struct CatalogMetaUpdate {
    pub id: String,
    pub display_name: String,
    pub provider_logo: String,
    pub release_date: String,
    pub model_meta_zh: String,
    pub model_meta_en: String,
}

#[derive(Serialize)]
pub struct TestLatencyResult {
    pub status: String,
    pub response_ms: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
}

/// List all API entries from the database.
pub fn list_entries(db: &Database) -> Result<Vec<ApiEntry>, AppError> {
    db.list_entries()
}

pub fn list_entries_paginated(
    db: &Database,
    page: i32,
    page_size: i32,
    group_name: Option<&str>,
    search: Option<&str>,
    channel_id: Option<&str>,
) -> Result<PaginatedResult<ApiEntry>, AppError> {
    db.list_entries_paginated(page, page_size, group_name, search, channel_id)
}

/// Toggle an entry's enabled state.
/// When enabling, clears cooldown and failure count for a fresh start.
pub fn toggle_entry(
    db: &Database,
    failure_counts: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
    id: &str,
    enabled: bool,
    pin_to_top: bool,
) -> Result<(), AppError> {
    toggle_entry_inner(db, failure_counts, id, enabled)?;
    if pin_to_top {
        db.set_entry_priority(id, 0)?;
    }
    crate::state_version::bump("pool");
    Ok(())
}

/// Batch toggle entries — single IPC call to avoid N concurrent IPC calls.
pub fn batch_toggle_entries(
    db: &Database,
    failure_counts: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
    ids: &[String],
    enabled: bool,
) -> Result<(), AppError> {
    for id in ids {
        toggle_entry_inner(db, failure_counts, id, enabled)?;
    }
    crate::state_version::bump("pool");
    Ok(())
}

fn toggle_entry_inner(
    db: &Database,
    failure_counts: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
    id: &str,
    enabled: bool,
) -> Result<(), AppError> {
    db.toggle_entry(id, enabled)?;
    if enabled {
        let _ = db.set_entry_cooldown(id, None);
        if let Ok(mut counts) = failure_counts.try_write() {
            counts.remove(id);
        }
    }
    Ok(())
}

/// Reorder entries by the given ordered IDs.
pub fn reorder_entries(db: &Database, ordered_ids: &[String]) -> Result<(), AppError> {
    db.reorder_entries(ordered_ids)?;
    crate::state_version::bump("pool");
    Ok(())
}

/// Delete an entry by ID.
pub fn delete_entry(db: &Database, id: &str) -> Result<(), AppError> {
    db.delete_entry(id)?;
    crate::state_version::bump("pool");
    Ok(())
}

/// Create a new API entry. Also syncs channel model list.
pub fn create_entry(db: &Database, params: CreateEntryParams) -> Result<ApiEntry, AppError> {
    let display_name = params.display_name.as_deref().unwrap_or(&params.model);
    let group_name = params.group_name.as_deref().unwrap_or("auto");
    let entry = db.create_entry_auto(
        &params.channel_id,
        &params.model,
        display_name,
        &params.provider_logo,
        &params.release_date,
        &params.model_meta_zh,
        &params.model_meta_en,
        group_name,
    )?;
    let _ = db.add_channel_model_if_missing(
        &params.channel_id,
        &params.model,
        entry.owned_by.as_deref(),
    );
    crate::state_version::bump("pool");
    Ok(entry)
}

/// Convert command-layer catalog meta updates to database-layer inputs.
fn to_catalog_meta_inputs(items: Vec<CatalogMetaUpdate>) -> Vec<EntryCatalogMetaInput> {
    items
        .into_iter()
        .map(|item| EntryCatalogMetaInput {
            id: item.id,
            display_name: item.display_name,
            provider_logo: item.provider_logo,
            release_date: item.release_date,
            model_meta_zh: item.model_meta_zh,
            model_meta_en: item.model_meta_en,
        })
        .collect()
}

/// Backfill catalog metadata for multiple entries.
pub fn backfill_entry_catalog_meta(
    db: &Database,
    items: Vec<CatalogMetaUpdate>,
) -> Result<(), AppError> {
    let inputs = to_catalog_meta_inputs(items);
    db.backfill_entry_catalog_meta(&inputs)?;
    crate::state_version::bump("pool");
    Ok(())
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect::<String>()
}

/// Test latency for a specific entry.
pub async fn test_entry_latency(
    db: &Database,
    entry_id: &str,
    model_score: f64,
) -> Result<TestLatencyResult, AppError> {
    let entries = db.get_entries_for_routing_all()?;
    let entry = entries
        .iter()
        .find(|e| e.id == entry_id)
        .ok_or_else(|| AppError::NotFound(format!("Entry {entry_id} not found")))?
        .clone();

    let channel = db.get_channel(&entry.channel_id)?;
    let update_score = |response_ms: &str| -> f64 {
        let score = calculate_entry_score(model_score, &channel.response_ms, response_ms);
        let _ = db.update_entry_score(entry_id, score);
        let _ = db.update_entry_sort_index(entry_id, (100.0 - score.round()) as i32);
        score
    };

    let adapter = get_adapter(&channel.api_type);
    let url = adapter.build_chat_url(&channel.base_url, &entry.model);

    let mut upstream_body = json!({
        "model": entry.model,
        "messages": [{"role": "user", "content": "Say OK"}],
        "stream": false,
    });
    adapter.transform_request(&mut upstream_body, &entry.model);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            let message = format!("HTTP client: {e}");
            let _ = db.update_entry_response_ms(entry_id, "X");
            let new_score = calculate_entry_score(model_score, &channel.response_ms, "X");
            let _ = db.update_entry_score(entry_id, new_score);
            let _ = db.update_entry_sort_index(entry_id, (100.0 - new_score.round()) as i32);
            let _ = db.toggle_entry(entry_id, false);
            insert_test_usage_log(
                db,
                None,
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "latency_test",
                    log_group: "latency_test",
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    latency_ms: 0,
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
            crate::state_version::bump("pool");
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

    let start = Instant::now();
    let response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            let message = format!("network_error: {e}");
            let _ = db.update_entry_response_ms(entry_id, "X");
            let _ = db.toggle_entry(entry_id, false);
            insert_test_usage_log(
                db,
                None,
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "latency_test",
                    log_group: "latency_test",
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
            crate::state_version::bump("pool");
            return Ok(TestLatencyResult {
                status: "failed:network_error".to_string(),
                response_ms: "X".to_string(),
                score: update_score("X"),
                error_detail: Some(message),
            });
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;
    let status = response.status();

    if !status.is_success() {
        let status_code = status.as_u16() as i32;
        let error_preview = response.text().await.unwrap_or_default();
        let error_preview = truncate_for_log(&error_preview, 1000);
        let error_detail = if error_preview.is_empty() {
            format!("http_{}", status.as_u16())
        } else {
            format!("http_{}: {}", status.as_u16(), error_preview)
        };
        let _ = db.update_entry_response_ms(entry_id, "X");
        let _ = db.toggle_entry(entry_id, false);
        insert_test_usage_log(
            db,
            None,
            TestUsageLogInput {
                entry: &entry,
                channel: &channel,
                operation: "latency_test",
                log_group: "latency_test",
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms: latency_ms as i64,
                status_code,
                success: false,
                error_message: Some(&error_detail),
                request_payload: Some(&upstream_body),
                response_payload: None,
                error_kind: Some("http_error"),
                response_ms: Some("X"),
                error_preview: Some(&error_preview),
            },
        );
        crate::state_version::bump("pool");
        return Ok(TestLatencyResult {
            status: "failed:http_error".to_string(),
            response_ms: "X".to_string(),
            score: update_score("X"),
            error_detail: Some(error_detail),
        });
    }

    let body = match response.text().await {
        Ok(body) => body,
        Err(e) => {
            let message = format!("response_read_error: {e}");
            let _ = db.update_entry_response_ms(entry_id, "X");
            let _ = db.toggle_entry(entry_id, false);
            insert_test_usage_log(
                db,
                None,
                TestUsageLogInput {
                    entry: &entry,
                    channel: &channel,
                    operation: "latency_test",
                    log_group: "latency_test",
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
            crate::state_version::bump("pool");
            return Ok(TestLatencyResult {
                status: "failed:response_error".to_string(),
                response_ms: "X".to_string(),
                score: update_score("X"),
                error_detail: Some(message),
            });
        }
    };

    if body.trim().is_empty() {
        let _ = db.update_entry_response_ms(entry_id, "X");
        let _ = db.toggle_entry(entry_id, false);
        insert_test_usage_log(
            db,
            None,
            TestUsageLogInput {
                entry: &entry,
                channel: &channel,
                operation: "latency_test",
                log_group: "latency_test",
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms: latency_ms as i64,
                status_code: 200,
                success: false,
                error_message: Some("empty_response"),
                request_payload: Some(&upstream_body),
                response_payload: None,
                error_kind: Some("empty_response"),
                response_ms: Some("X"),
                error_preview: None,
            },
        );
        crate::state_version::bump("pool");

        return Ok(TestLatencyResult {
            status: "failed:empty_response".to_string(),
            response_ms: "X".to_string(),
            score: update_score("X"),
            error_detail: Some("empty_response".to_string()),
        });
    }

    let parsed_body = serde_json::from_str::<Value>(&body).ok();
    let (prompt_tokens, completion_tokens) = parsed_body
        .as_ref()
        .map(extract_usage_tokens)
        .unwrap_or((0, 0));
    let response_ms = latency_ms.to_string();
    db.update_entry_response_ms(entry_id, &response_ms)?;
    let score = update_score(&response_ms);
    // Enable entry and clear cooldown, ensuring subsequent automatic routing can hit
    db.toggle_entry(entry_id, true)?;
    let _ = db.set_entry_cooldown(entry_id, None);
    insert_test_usage_log(
        db,
        None,
        TestUsageLogInput {
            entry: &entry,
            channel: &channel,
            operation: "latency_test",
            log_group: "latency_test",
            prompt_tokens,
            completion_tokens,
            latency_ms: latency_ms as i64,
            status_code: 200,
            success: true,
            error_message: None,
            request_payload: Some(&upstream_body),
            response_payload: None,
            error_kind: None,
            response_ms: Some(&response_ms),
            error_preview: None,
        },
    );
    crate::state_version::bump("pool");

    Ok(TestLatencyResult {
        status: "ok".to_string(),
        response_ms,
        score,
        error_detail: None,
    })
}

/// Update response time for an entry.
pub fn update_entry_response_ms(
    db: &Database,
    entry_id: &str,
    response_ms: &str,
) -> Result<(), AppError> {
    db.update_entry_response_ms(entry_id, response_ms)
}

/// Get all distinct group names from the database.
pub fn get_all_groups(db: &Database) -> Result<Vec<String>, AppError> {
    db.get_all_group_names()
}

/// Update the group_name for a specific entry.
pub fn update_entry_display_name(
    db: &Database,
    id: &str,
    display_name: &str,
) -> Result<(), AppError> {
    db.update_entry_display_name(id, display_name)?;
    crate::state_version::bump("pool");
    Ok(())
}

pub fn update_entry_group(db: &Database, id: &str, group_name: &str) -> Result<(), AppError> {
    db.update_entry_group(id, group_name)?;
    crate::state_version::bump("pool");
    Ok(())
}
