//! Token facade: access key CRUD and status toggling.
//!
//! Invokes functions in `services::token_service`, using `&Database` underneath,
//! available across all build modes.

use crate::database::dao::PaginatedResult;
use crate::database::AccessKey;
use crate::error::AppError;

use super::ServerApi;

impl ServerApi {
    /// Get all access keys.
    pub fn list_access_keys(&self) -> Result<Vec<AccessKey>, AppError> {
        crate::services::token_service::list_access_keys(&self.state().db)
    }

    /// Paginated retrieval of access keys.
    pub fn list_access_keys_paginated(
        &self,
        page: i32,
        page_size: i32,
    ) -> Result<PaginatedResult<AccessKey>, AppError> {
        crate::services::token_service::list_access_keys_paginated(
            &self.state().db,
            page,
            page_size,
        )
    }

    /// Create new access key.
    pub fn create_access_key(&self, name: &str) -> Result<AccessKey, AppError> {
        crate::services::token_service::create_access_key(&self.state().db, name)
    }

    /// Delete specified access key.
    pub fn delete_access_key(&self, id: &str) -> Result<(), AppError> {
        crate::services::token_service::delete_access_key(&self.state().db, id, self.app.as_ref())
    }

    /// Toggle enabled status of access key.
    pub fn toggle_access_key(&self, id: &str, enabled: bool) -> Result<(), AppError> {
        crate::services::token_service::toggle_access_key(
            &self.state().db,
            id,
            enabled,
            self.app.as_ref(),
        )
    }
}
