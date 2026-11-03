//! Config facade: settings read / full update / partial patch.
//!
//! Invokes reusable helpers in `commands::config`, using `AppEventHandle` underneath,
//! and is therefore available across all build modes.

use crate::commands::config;
use crate::database::AppSettings;
use crate::error::AppError;

use super::ServerApi;

impl ServerApi {
    /// Get current settings from in-memory cache.
    pub async fn get_settings(&self) -> Result<AppSettings, AppError> {
        config::get_settings_from_state(self.state()).await
    }

    /// Completely update settings, triggering side effects like proxy/admin reload.
    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, AppError> {
        config::update_settings_from_state(self.app.clone(), self.state(), settings).await
    }

    /// Completely update settings (async reload), returning restart info.
    ///
    /// Used in scenarios requiring `RestartInfo` such as admin HTTP handlers.
    pub async fn update_settings_with_restart(
        &self,
        settings: AppSettings,
        restart_async: bool,
    ) -> Result<Option<crate::admin::RestartInfo>, AppError> {
        config::apply_settings_update_with_restart(self.app.clone(),
            self.state(),
            settings,
            restart_async,
        )
        .await
    }

    /// Partially patch settings and return updated settings.
    ///
    /// Note: `web_admin_password` protection logic (keeping original value on empty password) is handled by the caller.
    pub async fn patch_settings(&self, patch: serde_json::Value) -> Result<AppSettings, AppError> {
        config::patch_settings_from_state(self.app.clone(), self.state(), patch).await
    }
}