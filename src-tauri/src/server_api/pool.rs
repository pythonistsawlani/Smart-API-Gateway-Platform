//! Pool facade: CRUD, batch operations, latency testing, etc. for API entries.
//!
//! Invokes functions in `services::pool_service`, using `&Database` underneath,
//! available across all build modes.

use crate::database::dao::PaginatedResult;
use crate::database::ApiEntry;
use crate::error::AppError;
use crate::services::pool_service::{
    self, CatalogMetaUpdate, CreateEntryParams, TestLatencyResult,
};

use super::ServerApi;

impl ServerApi {
    /// Get all API entries.
    pub fn list_entries(&self) -> Result<Vec<ApiEntry>, AppError> {
        pool_service::list_entries(&self.state().db)
    }

    /// Paginated retrieval of API entries.
    pub fn list_entries_paginated(
        &self,
        page: i32,
        page_size: i32,
        group_name: Option<&str>,
        search: Option<&str>,
        channel_id: Option<&str>,
    ) -> Result<PaginatedResult<ApiEntry>, AppError> {
        pool_service::list_entries_paginated(
            &self.state().db,
            page,
            page_size,
            group_name,
            search,
            channel_id,
        )
    }

    /// Toggle enabled status of a single entry.
    pub fn toggle_entry(&self, id: &str, enabled: bool, pin_to_top: bool) -> Result<(), AppError> {
        let result = pool_service::toggle_entry(
            &self.state().db,
            &self.state().failure_counts,
            id,
            enabled,
            pin_to_top,
        );
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Batch toggle enabled status of entries (single IPC call).
    pub fn batch_toggle_entries(&self, ids: &[String], enabled: bool) -> Result<(), AppError> {
        let result = pool_service::batch_toggle_entries(
            &self.state().db,
            &self.state().failure_counts,
            ids,
            enabled,
        );
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Reorder entries in the given sequence.
    pub fn reorder_entries(&self, ordered_ids: &[String]) -> Result<(), AppError> {
        let result = pool_service::reorder_entries(&self.state().db, ordered_ids);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Delete specified entry.
    pub fn delete_entry(&self, id: &str) -> Result<(), AppError> {
        let result = pool_service::delete_entry(&self.state().db, id);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Create new API entry.
    pub fn create_entry(&self, params: CreateEntryParams) -> Result<ApiEntry, AppError> {
        let entry = pool_service::create_entry(&self.state().db, params)?;
        self.emit_event("entries-changed");
        Ok(entry)
    }

    /// Batch backfill catalog metadata.
    pub fn backfill_entry_catalog_meta(
        &self,
        items: Vec<CatalogMetaUpdate>,
    ) -> Result<(), AppError> {
        let result = pool_service::backfill_entry_catalog_meta(&self.state().db, items);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Test latency of specified entry.
    pub async fn test_entry_latency(
        &self,
        entry_id: &str,
        model_score: f64,
    ) -> Result<TestLatencyResult, AppError> {
        let result =
            pool_service::test_entry_latency(&self.state().db, entry_id, model_score).await;
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Update response time of specified entry.
    pub fn update_entry_response_ms(
        &self,
        entry_id: &str,
        response_ms: &str,
    ) -> Result<(), AppError> {
        let result =
            pool_service::update_entry_response_ms(&self.state().db, entry_id, response_ms);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Get all group names.
    pub fn get_all_groups(&self) -> Result<Vec<String>, AppError> {
        pool_service::get_all_groups(&self.state().db)
    }

    /// Update display name of specified entry.
    pub fn update_entry_display_name(&self, id: &str, display_name: &str) -> Result<(), AppError> {
        let result = pool_service::update_entry_display_name(&self.state().db, id, display_name);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }

    /// Update group of specified entry.
    pub fn update_entry_group(&self, id: &str, group_name: &str) -> Result<(), AppError> {
        let result = pool_service::update_entry_group(&self.state().db, id, group_name);
        if result.is_ok() {
            self.emit_event("entries-changed");
        }
        result
    }
}
