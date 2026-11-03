mod auth;
mod channel_handlers;
mod chat_handlers;
mod connection_apps_handlers;
mod error;
mod handlers;
mod import_export_handlers;
mod pool_handlers;
mod proxy_handlers;
mod router;
mod state;
mod static_files;
mod token_handlers;
mod translation_handlers;
mod usage_handlers;

use crate::database::AppSettings;
use crate::AppState;
use axum::Router;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};

pub enum AdminMode {
    Disabled,
    Standalone,
    Combined,
}

pub use error::{
    ERROR_CODE_EMPTY_MODEL_LIST, ERROR_CODE_ENDPOINT_CORRECTION_FAILED,
    ERROR_CODE_ENDPOINT_UNREACHABLE, ERROR_CODE_ENDPOINT_VALIDATION_FAILED,
    ERROR_CODE_FETCH_MODELS_FAILED, ERROR_CODE_HTTP_CLIENT_ERROR, ERROR_CODE_INVALID_CREDENTIALS,
    ERROR_CODE_INVALID_URL, ERROR_CODE_RATE_LIMITED, ERROR_CODE_TIMEOUT,
    ERROR_CODE_UNSUPPORTED_PROVIDER,
};
pub use handlers::RestartInfo;
pub use router::build_admin_router;
pub use state::AdminState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStatus {
    pub running: bool,
    pub address: String,
    pub port: i32,
}

pub struct AdminServer {
    port: i32,
    state: AdminState,
    shutdown_tx: Arc<RwLock<Option<oneshot::Sender<()>>>>,
}

impl AdminServer {
    pub fn new(port: i32, runtime: AppState, app_handle: Option<crate::AppEventHandle>) -> Self {
        Self {
            port,
            state: AdminState::new_runtime(runtime, app_handle),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    pub fn router(&self) -> Router {
        build_admin_router(self.state.clone())
    }

    pub async fn start(&self) -> Result<(), String> {
        if self.shutdown_tx.read().await.is_some() {
            return Err("Admin server already running".to_string());
        }

        let addr: SocketAddr = format!("127.0.0.1:{}", self.port)
            .parse()
            .map_err(|e| format!("Invalid admin address: {e}"))?;
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind admin server: {e}"))?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let app = self.router();

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap_or_else(|e| log::error!("Admin server error: {e}"));
            log::info!("Admin server stopped");
        });

        log::info!("Admin server started on {addr}");
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
            Ok(())
        } else {
            Err("Admin server not running".to_string())
        }
    }

    pub fn get_status(&self) -> AdminStatus {
        let running = self
            .shutdown_tx
            .try_read()
            .map(|guard| guard.is_some())
            .unwrap_or(true);

        AdminStatus {
            running,
            address: "127.0.0.1".to_string(),
            port: self.port,
        }
    }
}

pub fn should_start_admin(settings: &AppSettings) -> bool {
    if !settings.web_admin_enabled {
        return false;
    }

    // Safe-by-default: refuse to boot the admin server while credentials are
    // missing or still set to the legacy hardcoded admin/admin default.
    let username = settings.web_admin_username.trim();
    let password = settings.web_admin_password.trim();
    if username.is_empty() || password.is_empty() {
        log::warn!(
            "Web Admin disabled: admin credentials are not configured. \
             Set a username and password in Settings, or use \
             API_SWITCH_ADMIN_USER / API_SWITCH_ADMIN_PASS."
        );
        return false;
    }
    if username.eq_ignore_ascii_case("admin") && password == "admin" {
        log::warn!(
            "Web Admin disabled: default credentials (admin/admin) are still active. \
             Set a new password in Settings first."
        );
        return false;
    }
    true
}

pub fn admin_mode(settings: &AppSettings) -> AdminMode {
    if !should_start_admin(settings) {
        AdminMode::Disabled
    } else if settings.web_admin_port == settings.listen_port {
        AdminMode::Combined
    } else {
        AdminMode::Standalone
    }
}

pub fn build_combined_router(settings: &AppSettings, state: AdminState) -> Option<Router> {
    match admin_mode(settings) {
        AdminMode::Combined => Some(build_admin_router(state)),
        _ => None,
    }
}

pub async fn start_admin_if_enabled(
    runtime: AppState,
    app_handle: Option<crate::AppEventHandle>,
    admin_slot: Arc<RwLock<Option<AdminServer>>>,
) -> Result<(), String> {
    let snapshot = runtime.settings.read().await.clone();
    if !matches!(admin_mode(&snapshot), AdminMode::Standalone) {
        return Ok(());
    }

    let mut guard = admin_slot.write().await;
    if guard.is_some() {
        return Ok(());
    }

    let server = AdminServer::new(snapshot.web_admin_port, runtime, app_handle);
    server.start().await?;
    *guard = Some(server);
    Ok(())
}

pub async fn restart_admin(
    runtime: AppState,
    app_handle: Option<crate::AppEventHandle>,
    admin_slot: Arc<RwLock<Option<AdminServer>>>,
) -> Result<(), String> {
    if let Some(server) = admin_slot.write().await.take() {
        let _ = server.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    start_admin_if_enabled(runtime, app_handle.clone(), admin_slot).await
}

pub fn apply_admin_env(settings: &mut AppSettings) {
    let user = std::env::var("API_SWITCH_ADMIN_USER").unwrap_or_default();
    let pass = std::env::var("API_SWITCH_ADMIN_PASS").unwrap_or_default();
    if !user.trim().is_empty() && !pass.is_empty() {
        settings.web_admin_enabled = true;
        settings.web_admin_username = user;
        settings.web_admin_password = pass;
    }
}
