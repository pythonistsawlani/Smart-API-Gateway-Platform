//! Import/export facade: import and export migration packages for channels and model pools.

use crate::error::AppError;
use crate::services::import_export_service::{
    build_import_preview, build_transfer_json, validate_transfer_payload, ImportPreview,
    ImportResult,
};

use super::ServerApi;

impl ServerApi {
    /// Export channel/model transfer JSON.
    pub fn export_channel_model_transfer(&self) -> Result<String, AppError> {
        let channels = self.state().db.list_channels()?;
        let entries = self.state().db.list_entries()?;
        build_transfer_json(&channels, &entries)
    }

    /// Preview channel/model transfer import impact.
    pub fn preview_channel_model_transfer(&self, payload: &str) -> Result<ImportPreview, AppError> {
        let transfer = validate_transfer_payload(payload)?;
        let current_channels = self.state().db.list_channels()?.len();
        let current_models = self.state().db.list_entries()?.len();
        Ok(build_import_preview(
            &transfer,
            current_channels,
            current_models,
        ))
    }

    /// Import channel/model transfer, consistently maintaining versions and shell events.
    pub fn import_channel_model_transfer(&self, payload: &str) -> Result<ImportResult, AppError> {
        let transfer = validate_transfer_payload(payload)?;
        let (channel_count, model_count) = self
            .state()
            .db
            .replace_channels_and_models_from_transfer(&transfer)?;

        crate::state_version::bump("channel");
        crate::state_version::bump("pool");
        self.emit_event("channels-changed");
        self.emit_event("entries-changed");

        Ok(ImportResult {
            success: true,
            message: format!("Import successful; reconstructed {channel_count} channels and {model_count} models."),
            channel_count,
            model_count,
        })
    }
}
