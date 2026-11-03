use crate::database::{ApiEntry, Channel, ModelInfo};
use crate::error::AppError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const FORMAT_ID: &str = "smart-api-gateway-channel-model-transfer";
pub const EXPORTER: &str = "smart-api-gateway";
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelModelTransfer {
    pub format: String,
    pub exporter: String,
    pub schema_version: u32,
    pub app_version: String,
    pub exported_at: String,
    pub summary: TransferSummary,
    pub data: TransferData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferSummary {
    pub channel_count: usize,
    pub model_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferData {
    pub channels: Vec<TransferChannel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferChannel {
    pub name: String,
    pub api_type: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub upstream_headers: Option<String>,
    #[serde(default)]
    pub available_models: Vec<TransferModelInfo>,
    #[serde(default)]
    pub selected_models: Vec<String>,
    pub models: Vec<TransferModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferModelInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferModel {
    pub model: String,
    pub display_name: String,
    pub group_name: String,
    pub enabled: bool,
    pub sort_index: i32,
    #[serde(default)]
    pub provider_logo: String,
    #[serde(default)]
    pub release_date: String,
    #[serde(default)]
    pub model_meta_zh: String,
    #[serde(default)]
    pub model_meta_en: String,
    #[serde(default)]
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub incoming_channels: usize,
    pub incoming_models: usize,
    pub current_channels: usize,
    pub current_models: usize,
    pub contains_api_keys: bool,
    pub has_empty_model_channel: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub success: bool,
    pub message: String,
    pub channel_count: usize,
    pub model_count: usize,
}

pub fn validate_transfer_payload(payload: &str) -> Result<ChannelModelTransfer, AppError> {
    const MAX_PAYLOAD_BYTES: usize = 1_000_000;

    if payload.trim().is_empty() {
        return Err(AppError::Validation("Import text cannot be empty".to_string()));
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(AppError::Validation(
            "Import text is too large; please check if the correct export text was copied".to_string(),
        ));
    }

    let transfer: ChannelModelTransfer = serde_json::from_str(payload)
        .map_err(|e| AppError::Validation(format!("Import text is not valid JSON: {e}")))?;

    validate_transfer(&transfer)?;
    Ok(transfer)
}

pub fn validate_transfer(transfer: &ChannelModelTransfer) -> Result<(), AppError> {
    if transfer.format != FORMAT_ID {
        return Err(AppError::Validation(
            "Import file format is invalid; must be channel and model data exported by Smart API Gateway".to_string(),
        ));
    }
    if transfer.exporter != EXPORTER {
        return Err(AppError::Validation(
            "Import data source is invalid; must be data exported by Smart API Gateway".to_string(),
        ));
    }
    if transfer.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(AppError::Validation(format!(
            "Unsupported import data version: {}, currently only version {} is supported",
            transfer.schema_version, CURRENT_SCHEMA_VERSION
        )));
    }
    if transfer.data.channels.is_empty() {
        return Err(AppError::Validation(
            "Import data contains no channels; at least 1 channel is required".to_string(),
        ));
    }

    for (channel_index, channel) in transfer.data.channels.iter().enumerate() {
        let channel_no = channel_index + 1;
        if channel.name.trim().is_empty() {
            return Err(AppError::Validation(format!(
                "Channel {channel_no} name cannot be empty"
            )));
        }
        if !is_valid_api_type(&channel.api_type) {
            return Err(AppError::Validation(format!(
                "Channel {channel_no} API type is invalid: {}",
                channel.api_type
            )));
        }
        if channel.base_url.trim().is_empty() {
            return Err(AppError::Validation(format!(
                "Channel {channel_no} Base URL cannot be empty"
            )));
        }
        let lower_base_url = channel.base_url.trim().to_ascii_lowercase();
        if !lower_base_url.starts_with("http://") && !lower_base_url.starts_with("https://") {
            return Err(AppError::Validation(format!(
                "Channel {channel_no} Base URL format is invalid"
            )));
        }
        let mut seen_models = HashSet::new();
        for (model_index, model) in channel.models.iter().enumerate() {
            let model_no = model_index + 1;
            let normalized_model = model.model.trim().to_ascii_lowercase();
            if normalized_model.is_empty() {
                return Err(AppError::Validation(format!(
                    "Model {model_no} name in channel {channel_no} cannot be empty"
                )));
            }
            if !seen_models.insert(normalized_model) {
                return Err(AppError::Validation(format!(
                    "Duplicate model in channel {channel_no}: {}",
                    model.model
                )));
            }
            if model.group_name.trim().is_empty() {
                return Err(AppError::Validation(format!(
                    "Group name cannot be empty for model {} in channel {channel_no}",
                    model.model
                )));
            }
        }
    }

    Ok(())
}

fn is_valid_api_type(api_type: &str) -> bool {
    matches!(
        api_type,
        "openai" | "anthropic" | "gemini" | "azure" | "custom" | "responses" | "claude"
    )
}

pub fn build_import_preview(
    transfer: &ChannelModelTransfer,
    current_channels: usize,
    current_models: usize,
) -> ImportPreview {
    let incoming_channels = transfer.data.channels.len();
    let incoming_models = transfer
        .data
        .channels
        .iter()
        .map(|channel| channel.models.len())
        .sum();
    let contains_api_keys = transfer
        .data
        .channels
        .iter()
        .any(|channel| !channel.api_key.trim().is_empty());
    let empty_model_channels: Vec<String> = transfer
        .data
        .channels
        .iter()
        .filter(|channel| channel.models.is_empty())
        .map(|channel| channel.name.clone())
        .collect();

    let mut warnings = Vec::new();
    if contains_api_keys {
        warnings.push("Imported text contains API Key, please keep it safe.".to_string());
    }
    for channel_name in &empty_model_channels {
        warnings.push(format!("Channel {channel_name} has no associated models"));
    }

    ImportPreview {
        incoming_channels,
        incoming_models,
        current_channels,
        current_models,
        contains_api_keys,
        has_empty_model_channel: !empty_model_channels.is_empty(),
        warnings,
    }
}

pub fn build_transfer_json(channels: &[Channel], entries: &[ApiEntry]) -> Result<String, AppError> {
    let transfer = build_transfer(channels, entries);
    serde_json::to_string_pretty(&transfer)
        .map_err(|e| AppError::Internal(format!("Export JSON generation failed: {e}")))
}

pub fn build_transfer(channels: &[Channel], entries: &[ApiEntry]) -> ChannelModelTransfer {
    let transfer_channels: Vec<TransferChannel> = channels
        .iter()
        .map(|channel| {
            let models: Vec<TransferModel> = entries
                .iter()
                .filter(|entry| entry.channel_id == channel.id)
                .map(|entry| TransferModel {
                    model: entry.model.clone(),
                    display_name: entry.display_name.clone(),
                    group_name: entry
                        .group_name
                        .clone()
                        .unwrap_or_else(|| "auto".to_string()),
                    enabled: entry.enabled,
                    sort_index: entry.sort_index,
                    provider_logo: entry.provider_logo.clone().unwrap_or_default(),
                    release_date: entry.release_date.clone().unwrap_or_default(),
                    model_meta_zh: entry.model_meta_zh.clone().unwrap_or_default(),
                    model_meta_en: entry.model_meta_en.clone().unwrap_or_default(),
                    score: entry.score,
                })
                .collect();

            TransferChannel {
                name: channel.name.clone(),
                api_type: channel.api_type.clone(),
                base_url: channel.base_url.clone(),
                api_key: channel.api_key.clone(),
                enabled: channel.enabled,
                notes: channel.notes.clone(),
                upstream_headers: channel.upstream_headers.clone(),
                available_models: channel
                    .available_models
                    .iter()
                    .map(model_info_to_transfer)
                    .collect(),
                selected_models: channel.selected_models.clone(),
                models,
            }
        })
        .collect();

    let model_count = transfer_channels
        .iter()
        .map(|channel| channel.models.len())
        .sum();

    ChannelModelTransfer {
        format: FORMAT_ID.to_string(),
        exporter: EXPORTER.to_string(),
        schema_version: CURRENT_SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        exported_at: Utc::now().to_rfc3339(),
        summary: TransferSummary {
            channel_count: transfer_channels.len(),
            model_count,
        },
        data: TransferData {
            channels: transfer_channels,
        },
    }
}

pub fn model_info_to_transfer(model: &ModelInfo) -> TransferModelInfo {
    TransferModelInfo {
        id: model.id.clone(),
        name: model.name.clone(),
        owned_by: model.owned_by.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_payload() -> String {
        r#"{
          "format": "smart-api-gateway-channel-model-transfer",
          "exporter": "smart-api-gateway",
          "schema_version": 1,
          "app_version": "0.7.27",
          "exported_at": "2026-06-01T12:00:00+08:00",
          "summary": { "channel_count": 1, "model_count": 1 },
          "data": {
            "channels": [{
              "name": "OpenAI",
              "api_type": "openai",
              "base_url": "https://api.openai.com",
              "api_key": "sk-test",
              "enabled": true,
              "notes": "Main Channel",
              "available_models": [{ "id": "gpt-4.1", "name": "gpt-4.1", "owned_by": "openai" }],
              "selected_models": ["gpt-4.1"],
              "models": [{
                "model": "gpt-4.1",
                "display_name": "GPT 4.1",
                "group_name": "auto",
                "enabled": true,
                "sort_index": 7,
                "provider_logo": "openai",
                "release_date": "2025-04-14",
                "model_meta_zh": "Chinese description",
                "model_meta_en": "English description",
                "score": 12.5
              }]
            }]
          }
        }"#
        .to_string()
    }

    #[test]
    fn validate_accepts_api_switch_export_payload() {
        let transfer = validate_transfer_payload(&valid_payload()).expect("Valid export text should pass validation");

        assert_eq!(transfer.format, FORMAT_ID);
        assert_eq!(transfer.exporter, EXPORTER);
        assert_eq!(transfer.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(transfer.data.channels.len(), 1);
        assert_eq!(transfer.data.channels[0].models[0].sort_index, 7);
    }

    #[test]
    fn validate_accepts_empty_api_key_channel() {
        let payload = valid_payload().replace("\"api_key\": \"sk-test\"", "\"api_key\": \"\"");

        let transfer = validate_transfer_payload(&payload).expect("Empty API Key channel should also be allowed to import");

        assert_eq!(transfer.data.channels[0].api_key, "");
        let preview = build_import_preview(&transfer, 0, 0);
        assert!(!preview.contains_api_keys);
    }

    #[test]
    fn validate_rejects_non_api_switch_payload() {
        let payload =
            valid_payload().replace("\"exporter\": \"smart-api-gateway\"", "\"exporter\": \"other\"");

        let error = validate_transfer_payload(&payload).expect_err("Data not exported by this feature must be rejected");

        assert!(error.to_string().contains("must be data exported by Smart API Gateway"));
    }

    #[test]
    fn validate_rejects_empty_channel_list_before_delete() {
        let payload = valid_payload().replace(
            "[{\n              \"name\": \"OpenAI\",\n              \"api_type\": \"openai\",\n              \"base_url\": \"https://api.openai.com\",\n              \"api_key\": \"sk-test\",\n              \"enabled\": true,\n              \"notes\": \"Main Channel\",\n              \"available_models\": [{ \"id\": \"gpt-4.1\", \"name\": \"gpt-4.1\", \"owned_by\": \"openai\" }],\n              \"selected_models\": [\"gpt-4.1\"],\n              \"models\": [{\n                \"model\": \"gpt-4.1\",\n                \"display_name\": \"GPT 4.1\",\n                \"group_name\": \"auto\",\n                \"enabled\": true,\n                \"sort_index\": 7,\n                \"provider_logo\": \"openai\",\n                \"release_date\": \"2025-04-14\",\n                \"model_meta_zh\": \"Chinese description\",\n                \"model_meta_en\": \"English description\",\n                \"score\": 12.5\n              }]\n            }]",
            "[]",
        );

        let error = validate_transfer_payload(&payload).expect_err("Empty import must be rejected");

        assert!(error.to_string().contains("at least 1 channel is required"));
    }

    #[test]
    fn validate_rejects_duplicate_models_in_same_channel() {
        let payload = valid_payload().replace(
            "]\n            }]\n          }",
            ", {\n                \"model\": \"gpt-4.1\",\n                \"display_name\": \"Duplicate\",\n                \"group_name\": \"auto\",\n                \"enabled\": true,\n                \"sort_index\": 8\n              }]\n            }]\n          }",
        );

        let error = validate_transfer_payload(&payload).expect_err("Duplicate models within the same channel must be rejected");

        assert!(error.to_string().contains("Duplicate model"));
    }

    #[test]
    fn preview_counts_channels_models_and_api_keys() {
        let transfer = validate_transfer_payload(&valid_payload()).expect("Valid export text should pass validation");

        let preview = build_import_preview(&transfer, 3, 18);

        assert_eq!(preview.incoming_channels, 1);
        assert_eq!(preview.incoming_models, 1);
        assert_eq!(preview.current_channels, 3);
        assert_eq!(preview.current_models, 18);
        assert!(preview.contains_api_keys);
        assert!(!preview.has_empty_model_channel);
    }

    #[test]
    fn build_transfer_excludes_runtime_state_and_preserves_routing_fields() {
        let channel = Channel {
            id: "source-channel-id".to_string(),
            name: "OpenAI".to_string(),
            api_type: "openai".to_string(),
            base_url: "https://api.openai.com".to_string(),
            api_key: "sk-test".to_string(),
            available_models: vec![ModelInfo {
                id: "gpt-4.1".to_string(),
                name: "gpt-4.1".to_string(),
                owned_by: Some("openai".to_string()),
            }],
            selected_models: vec!["gpt-4.1".to_string()],
            enabled: true,
            last_fetch_at: 123456,
            notes: "Main Channel".to_string(),
            upstream_headers: None,
            response_ms: "999".to_string(),
            created_at: 1,
            updated_at: 2,
        };
        let entry = ApiEntry {
            id: "source-entry-id".to_string(),
            channel_id: "source-channel-id".to_string(),
            model: "gpt-4.1".to_string(),
            display_name: "GPT 4.1".to_string(),
            sort_index: 7,
            enabled: true,
            cooldown_until: Some(999999),
            circuit_state: "open".to_string(),
            created_at: 3,
            updated_at: 4,
            channel_name: Some("OpenAI".to_string()),
            channel_api_type: Some("openai".to_string()),
            owned_by: Some("openai".to_string()),
            response_ms: Some("888".to_string()),
            provider_logo: Some("openai".to_string()),
            release_date: Some("2025-04-14".to_string()),
            model_meta_zh: Some("Chinese description".to_string()),
            model_meta_en: Some("English description".to_string()),
            group_name: Some("coding".to_string()),
            score: 12.5,
        };

        let json = build_transfer_json(&[channel], &[entry]).expect("Export JSON should generate successfully");

        assert!(json.contains("smart-api-gateway-channel-model-transfer"));
        assert!(json.contains("sk-test"));
        assert!(json.contains("coding"));
        assert!(json.contains("\"sort_index\": 7"));
        assert!(!json.contains("source-channel-id"));
        assert!(!json.contains("source-entry-id"));
        assert!(!json.contains("cooldown_until"));
        assert!(!json.contains("response_ms"));
        assert!(!json.contains("last_fetch_at"));
    }

    #[test]
    fn import_with_missing_upstream_headers_does_not_fail() {
        let payload = valid_payload().replace(
            "\"upstream_headers\": null,",
            "",
        );
        let transfer = validate_transfer_payload(&payload)
            .expect("Legacy file missing upstream_headers field should be importable");
        assert!(transfer.data.channels[0].upstream_headers.is_none());
    }
}
