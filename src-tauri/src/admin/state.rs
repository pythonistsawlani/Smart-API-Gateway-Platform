use crate::admin::error::AdminError;
use crate::database::{AppSettings, Database};
use crate::server_api::ServerApi;
use crate::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Default)]
pub struct LoginFailureState {
    pub count: u32,
    pub locked_until: Option<chrono::DateTime<chrono::Utc>>,
    pub lockout_count: u32,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub username: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct AdminState {
    pub db: Arc<Database>,
    pub settings: Arc<RwLock<AppSettings>>,
    pub login_sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
    pub login_failures: Arc<Mutex<HashMap<String, LoginFailureState>>>,
    pub runtime: Option<AppState>,
    pub app_handle: Option<crate::AppEventHandle>,
}

impl AdminState {
    pub fn new_runtime(runtime: AppState, app_handle: Option<crate::AppEventHandle>) -> Self {
        Self {
            db: runtime.db.clone(),
            settings: runtime.settings.clone(),
            login_sessions: Arc::new(RwLock::new(HashMap::new())),
            login_failures: Arc::new(Mutex::new(HashMap::new())),
            runtime: Some(runtime),
            app_handle,
        }
    }

    pub fn mark_channel_dirty(&self) {
        if let Some(handle) = &self.app_handle {
            crate::event::emit(handle, "channels-changed");
        }
    }

    pub fn mark_pool_dirty(&self) {
        if let Some(handle) = &self.app_handle {
            crate::event::emit(handle, "entries-changed");
        }
    }

    /// Build the shared SERVER API facade for Web API handlers.
    pub fn server_api(&self) -> Result<ServerApi, AdminError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| AdminError::Internal("AdminState missing runtime".to_string()))?;
        Ok(ServerApi::new(runtime.clone(), self.app_handle.clone()))
    }

    pub fn mark_token_dirty(&self) {
        // Token bumps are managed by token_service
    }

    pub fn mark_log_dirty(&self) {
        // Log bumps are managed by log_service
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_admin_state(
        runtime: Option<AppState>,
        app_handle: Option<crate::AppEventHandle>,
    ) -> AdminState {
        let db_path = std::env::temp_dir().join(format!(
            "smart-api-gateway-admin-state-test-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let db = Database::open_at(&db_path).expect("test database should open");
        db.create_tables().expect("test schema should initialize");
        let _ = std::fs::remove_file(PathBuf::from(format!("{}-wal", db_path.display())));
        let _ = std::fs::remove_file(PathBuf::from(format!("{}-shm", db_path.display())));
        let _ = std::fs::remove_file(&db_path);

        AdminState {
            db: Arc::new(db),
            settings: Arc::new(RwLock::new(AppSettings::default())),
            login_sessions: Arc::new(RwLock::new(HashMap::new())),
            login_failures: Arc::new(Mutex::new(HashMap::new())),
            runtime,
            app_handle,
        }
    }

    #[test]
    fn server_api_requires_runtime_state() {
        let state = test_admin_state(None, None);

        let err = match state.server_api() {
            Ok(_) => panic!("missing runtime should fail"),
            Err(err) => err,
        };

        assert!(format!("{err:?}").contains("AdminState missing runtime"));
    }

    #[test]
    fn server_api_allows_missing_app_handle_for_headless_business_api() {
        let base = test_admin_state(None, None);
        let runtime = AppState {
            db: base.db.clone(),
            settings: base.settings.clone(),
            proxy: Arc::new(RwLock::new(None)),
            admin: Arc::new(RwLock::new(None)),
            translation_relay: Arc::new(RwLock::new(None)),
            failure_counts: Arc::new(RwLock::new(HashMap::new())),
            runtime_mode: crate::runtime_mode::RuntimeMode::Standalone,
        };
        let state = test_admin_state(Some(runtime), None);

        state
            .server_api()
            .expect("headless business API must not require an app shell handle");
    }
}
