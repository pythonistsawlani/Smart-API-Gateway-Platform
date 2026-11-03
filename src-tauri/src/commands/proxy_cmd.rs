use crate::commands::config::refresh_settings_l1;
use crate::error::AppError;
use crate::proxy::ProxyStatus;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn refresh_tray_menu(app: tauri::AppHandle) -> Result<(), AppError> {
    crate::refresh_tray_if_enabled(&app);
    Ok(())
}

// ---------------------------------------------------------------------------
// Reusable helper: does not depend on tauri::State, for ServerApi and admin handler invocations
// ---------------------------------------------------------------------------

/// Start proxy server and bind admin router.
pub async fn start_proxy_from_state(
    app: tauri::AppHandle,
    state: &AppState,
) -> Result<ProxyStatus, AppError> {
    let settings = state.settings.read().await.clone();
    let port = settings.listen_port;

    let mut proxy_guard = state.proxy.write().await;
    if proxy_guard.is_some() {
        return Err(AppError::Proxy("Proxy already running".to_string()));
    }

    let server = crate::proxy::ProxyServer::new(
        port,
        state.db.clone(),
        state.settings.clone(),
        Some(app.clone()),
        state.failure_counts.clone(),
    );
    let admin_router = crate::admin::build_combined_router(
        &settings,
        crate::admin::AdminState::new_runtime(state.clone(), Some(app.clone())),
    );
    server
        .start_with_admin(admin_router)
        .await
        .map_err(|e| AppError::Proxy(e.to_string()))?;

    let status = ProxyStatus {
        running: true,
        address: "127.0.0.1".to_string(),
        port,
    };

    *proxy_guard = Some(server);

    // Update config flag, then rebuild L1 settings cache from DB
    state.db.set_config_value("proxy_enabled", "1")?;
    refresh_settings_l1(state).await?;

    Ok(status)
}

/// Stop the running proxy server.
pub async fn stop_proxy_from_state(state: &AppState) -> Result<(), AppError> {
    let mut proxy_guard = state.proxy.write().await;
    if let Some(server) = proxy_guard.take() {
        server
            .stop()
            .await
            .map_err(|e| AppError::Proxy(e.to_string()))?;
    }
    state.db.set_config_value("proxy_enabled", "0")?;
    refresh_settings_l1(state).await?;
    Ok(())
}

/// Query current proxy running status (does not alter any state).
pub async fn proxy_status_from_state(state: &AppState) -> Result<ProxyStatus, AppError> {
    let proxy_guard = state.proxy.read().await;
    let settings = state.settings.read().await.clone();

    Ok(match proxy_guard.as_ref() {
        Some(server) => server.get_status(),
        None => ProxyStatus {
            running: false,
            address: "127.0.0.1".to_string(),
            port: settings.listen_port,
        },
    })
}

// ---------------------------------------------------------------------------
// Tauri command thin wrapper: invoked via ServerApi
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn start_proxy(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ProxyStatus, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.start_proxy().await
}

#[tauri::command]
pub async fn stop_proxy(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.stop_proxy().await
}

#[tauri::command]
pub async fn get_proxy_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ProxyStatus, AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.proxy_status().await
}
