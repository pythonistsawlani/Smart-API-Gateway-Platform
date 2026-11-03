//! Channel facade: channel CRUD, model fetching, connection testing, etc.
//!
//! Invokes functions in `services::channel_service`, using `&Database` and
//! `Option<&AppEventHandle>` underneath, available across all build modes.

use crate::database::dao::PaginatedResult;
use crate::database::{Channel, ModelCatalogMetaInput, ModelInfo};
use crate::error::AppError;
use crate::services::channel_service::{
    self, CreateChannelParams, FetchModelsResult, SaveChannelWithModelsParams,
    SaveChannelWithModelsResult, TestChannelResult, UpdateChannelParams, UpdateResponseMsParams,
};

use super::ServerApi;

impl ServerApi {
    /// Get all channels list.
    pub fn list_channels(&self) -> Result<Vec<Channel>, AppError> {
        channel_service::list_channels(&self.state().db)
    }

    /// Get model list for the specified channel.
    pub async fn fetch_channel_models(
        &self,
        channel_id: String,
    ) -> Result<FetchModelsResult, AppError> {
        channel_service::fetch_models(&self.state().db, channel_id).await
    }

    /// Paginated retrieval of channel list.
    pub fn list_channels_paginated(
        &self,
        page: i32,
        page_size: i32,
    ) -> Result<PaginatedResult<Channel>, AppError> {
        channel_service::list_channels_paginated(&self.state().db, page, page_size)
    }

    /// Create new channel.
    pub fn create_channel(&self, params: CreateChannelParams) -> Result<Channel, AppError> {
        let channel = channel_service::create_channel(&self.state().db, params)?;
        self.emit_event("channels-changed");
        Ok(channel)
    }

    /// Update channel, also triggering frontend events.
    pub fn update_channel(&self, params: UpdateChannelParams) -> Result<Channel, AppError> {
        channel_service::update_channel(&self.state().db, self.app.as_ref(), params)
    }

    /// Delete channel, also triggering frontend events.
    pub fn delete_channel(&self, id: String) -> Result<(), AppError> {
        let result = channel_service::delete_channel(&self.state().db, self.app.as_ref(), id);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Update channel response latency in ms.
    pub fn update_channel_response_ms(
        &self,
        channel_id: &str,
        response_ms: &str,
    ) -> Result<(), AppError> {
        let result = channel_service::update_channel_response_ms(
            &self.state().db,
            UpdateResponseMsParams {
                channel_id: channel_id.to_string(),
                response_ms: response_ms.to_string(),
            },
        );
        if result.is_ok() {
            self.emit_event("channels-changed");
        }
        result
    }

    /// Sync selected models for channel and maintain model-derived pool entries.
    pub fn select_channel_models(
        &self,
        channel_id: &str,
        model_names: &[String],
        available_models: &[ModelInfo],
        catalog_meta: &[ModelCatalogMetaInput],
    ) -> Result<(), AppError> {
        self.state()
            .db
            .update_channel_models(channel_id, available_models, model_names)?;
        self.state().db.sync_entries_for_channel_with_meta(
            channel_id,
            model_names,
            catalog_meta,
        )?;
        crate::state_version::bump("channel");
        crate::state_version::bump("pool");
        self.emit_event("channels-changed");
        self.emit_event("entries-changed");
        Ok(())
    }

    /// Test specified channel and update enabled status/response latency according to test result.
    pub async fn test_channel(&self, channel_id: &str) -> Result<TestChannelResult, AppError> {
        let channel = self.state().db.get_channel(channel_id)?;

        let model = channel
            .selected_models
            .first()
            .or_else(|| channel.available_models.first().map(|m| &m.name))
            .cloned()
            .unwrap_or_else(|| "gpt-3.5-turbo".to_string());

        let result = channel_service::test_channel_chat(
            &channel.base_url,
            &channel.api_key,
            &channel.api_type,
            &model,
            channel.upstream_headers.as_deref(),
        )
        .await;

        if result.success && result.status_code == Some(200) {
            let _ = self
                .state()
                .db
                .update_channel_response_ms(channel_id, &result.latency_ms.to_string());
            let _ = channel_service::update_channel(
                &self.state().db,
                self.app.as_ref(),
                UpdateChannelParams {
                    id: channel_id.to_string(),
                    name: None,
                    api_type: None,
                    base_url: None,
                    api_key: None,
                    enabled: Some(true),
                    notes: None,
                    upstream_headers: None,
                },
            );
        } else {
            let _ = self.state().db.disable_channel(channel_id);
            crate::state_version::bump("channel");
            self.emit_event("channels-changed");
        }

        Ok(result)
    }

    /// One-step save channel and sync models, while triggering frontend events.
    pub fn save_channel_with_models(
        &self,
        params: SaveChannelWithModelsParams,
    ) -> Result<SaveChannelWithModelsResult, AppError> {
        channel_service::save_channel_with_models(&self.state().db, self.app.as_ref(), params)
    }
}
