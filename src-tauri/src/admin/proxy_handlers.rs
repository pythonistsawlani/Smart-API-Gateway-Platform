// Proxy administration handlers (admin side)
// Provides status, start, and stop endpoints for the HTTP proxy.

use crate::admin::{error::AdminError, state::AdminState};
use crate::server_api::ServerApi;
use axum::{extract::State, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct ProxyStatusResponse {
    pub running: bool,
    pub port: i32,
}

/// Build a ServerApi instance from AdminState.
/// Returns an error when runtime or app_handle is missing.
fn server_api_from_admin(state: &AdminState) -> Result<ServerApi, AdminError> {
    let runtime = state
        .runtime
        .as_ref()
        .ok_or_else(|| AdminError::Internal("AdminState missing runtime".to_string()))?
        .clone();
    Ok(ServerApi::new(
            runtime,
            state.app_handle.clone(),
        ))
}

/// Get the current proxy status.
///
/// Returns JSON with `running` flag and the configured listen `port`.
pub async fn get_status(
    State(state): State<AdminState>,
) -> Result<Json<ProxyStatusResponse>, AdminError> {
    let api = server_api_from_admin(&state)?;
    let status = api
        .proxy_status()
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))?;
    Ok(Json(ProxyStatusResponse {
        running: status.running,
        port: status.port,
    }))
}

/// Start the proxy server.
///
/// Fails with an error if the proxy is already running.
pub async fn start(
    State(state): State<AdminState>,
) -> Result<Json<ProxyStatusResponse>, AdminError> {
    let api = server_api_from_admin(&state)?;
    let status = api
        .start_proxy()
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))?;
    Ok(Json(ProxyStatusResponse {
        running: status.running,
        port: status.port,
    }))
}

/// Stop the proxy server.
///
/// Fails if the proxy is not currently running.
pub async fn stop(
    State(state): State<AdminState>,
) -> Result<Json<ProxyStatusResponse>, AdminError> {
    let api = server_api_from_admin(&state)?;
    let runtime = state
        .runtime
        .as_ref()
        .ok_or_else(|| AdminError::Internal("AdminState missing runtime".to_string()))?;
    let settings = runtime.settings.read().await.clone();
    api.stop_proxy()
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))?;
    Ok(Json(ProxyStatusResponse {
        running: false,
        port: settings.listen_port,
    }))
}
