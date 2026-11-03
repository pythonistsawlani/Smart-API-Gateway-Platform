use crate::admin::RestartInfo;
use crate::database::AppSettings;
use crate::error::AppError;
use crate::AppState;
#[cfg(feature = "gui")]
use serde::Deserialize;
#[cfg(feature = "gui")]
use tauri::State;

async fn restart_proxy_if_running(
    app: Option<crate::AppEventHandle>,
    state: &AppState,
    previous_settings: &AppSettings,
) -> Result<(), AppError> {
    let mut proxy_guard = state.proxy.write().await;
    let Some(server) = proxy_guard.take() else {
        return Ok(());
    };

    let _ = server.stop().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let settings = state.settings.read().await.clone();
    if !settings.proxy_enabled {
        return Ok(());
    }

    let admin_router = crate::admin::build_combined_router(
        &settings,
        crate::admin::AdminState::new_runtime(state.clone(), app.clone()),
    );
    let new_server = crate::proxy::ProxyServer::new(
        settings.listen_port,
        state.db.clone(),
        state.settings.clone(),
        app.clone(),
        state.failure_counts.clone(),
    );
    if let Err(error) = new_server.start_with_admin(admin_router).await {
        // Rollback: restore previous settings and restart proxy with old config
        state.db.update_settings(previous_settings)?;
        let restored_settings = refresh_settings_l1(state).await?;
        sync_autostart(&restored_settings);

        let rollback_server = crate::proxy::ProxyServer::new(
            previous_settings.listen_port,
            state.db.clone(),
            state.settings.clone(),
            app.clone(),
            state.failure_counts.clone(),
        );
        let rollback_admin_router = crate::admin::build_combined_router(
            &restored_settings,
            crate::admin::AdminState::new_runtime(state.clone(), app.clone()),
        );
        rollback_server
            .start_with_admin(rollback_admin_router)
            .await
            .map_err(|restore_error| {
                AppError::Proxy(format!("{error}; rollback failed: {restore_error}"))
            })?;
        *proxy_guard = Some(rollback_server);
        // Rollback succeeded - log the original error but don't propagate it
        log::error!("Proxy restart failed, rolled back to previous config: {error}");
    }

    state.db.set_config_value("proxy_enabled", "1")?;
    *proxy_guard = Some(new_server);
    Ok(())
}

#[cfg(feature = "gui")]
const GITHUB_REPO: &str = "pythonistsawlani/Smart-API-Gateway-Platform";

#[cfg(feature = "gui")]
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[allow(dead_code)]
    body: Option<String>,
}

#[cfg(feature = "gui")]
#[tauri::command]
pub async fn check_update() -> Result<Option<serde_json::Value>, AppError> {
    let current = env!("CARGO_PKG_VERSION");

    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Network(e.to_string()))?;

    let resp = client
        .get(&url)
        .header("User-Agent", "smart-api-gateway")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| AppError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Ok(None);
    }

    let release: GithubRelease = resp
        .json()
        .await
        .map_err(|e| AppError::Network(e.to_string()))?;
    let latest = release.tag_name.trim_start_matches('v').to_string();

    if latest == current {
        return Ok(None);
    }

    Ok(Some(serde_json::json!({
        "current": current,
        "latest": latest,
        "url": release.html_url,
    })))
}

#[cfg(feature = "desktop")]
fn sync_autostart(settings: &AppSettings) {
    let app_name = "API Switch";
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            log::error!("Failed to get exe path: {e}");
            return;
        }
    };
    let exe_path = exe.to_string_lossy().to_string();

    let auto = match auto_launch::AutoLaunchBuilder::new()
        .set_app_name(app_name)
        .set_app_path(&exe_path)
        .build()
    {
        Ok(a) => a,
        Err(e) => {
            log::error!("Failed to create AutoLaunch: {e}");
            return;
        }
    };

    let result = if settings.autostart {
        auto.enable()
    } else {
        auto.disable()
    };

    if let Err(e) = result {
        log::error!("Failed to sync autostart: {e}");
    }
}

pub async fn refresh_settings_l1(state: &AppState) -> Result<AppSettings, AppError> {
    // Settings writes are rare and settings are small.
    // Keep DB as the source of truth: after every settings write,
    // rebuild the L1 settings cache from DB instead of patching fields manually.
    let settings = state.db.get_settings()?;
    *state.settings.write().await = settings.clone();
    Ok(settings)
}

#[cfg(not(feature = "desktop"))]
fn sync_autostart(_settings: &AppSettings) {}

pub(crate) fn merge_settings_patch(
    current: &AppSettings,
    patch: &serde_json::Value,
) -> Result<AppSettings, AppError> {
    let patch_obj = patch
        .as_object()
        .ok_or_else(|| AppError::Validation("settings patch must be a JSON object".to_string()))?;
    let mut settings_value = serde_json::to_value(current)
        .map_err(|e| AppError::Internal(format!("serialize settings failed: {e}")))?;

    let Some(settings_obj) = settings_value.as_object_mut() else {
        return Err(AppError::Internal(
            "settings did not serialize to a JSON object".to_string(),
        ));
    };

    for (key, value) in patch_obj {
        if key != "_version" {
            settings_obj.insert(key.clone(), value.clone());
        }
    }

    serde_json::from_value(settings_value)
        .map_err(|e| AppError::Validation(format!("invalid settings patch: {e}")))
}

pub async fn apply_settings_update(
    app: Option<crate::AppEventHandle>,
    state: &AppState,
    settings: AppSettings,
    restart_async: bool,
) -> Result<(), AppError> {
    let _ = apply_settings_update_with_restart(app, state, settings, restart_async).await?;
    Ok(())
}

pub async fn apply_settings_update_with_restart(
    app: Option<crate::AppEventHandle>,
    state: &AppState,
    settings: AppSettings,
    restart_async: bool,
) -> Result<Option<RestartInfo>, AppError> {
    let previous_settings = state.settings.read().await.clone();
    let requires_proxy_restart = previous_settings.listen_port != settings.listen_port
        || previous_settings.web_admin_enabled != settings.web_admin_enabled
        || previous_settings.web_admin_username != settings.web_admin_username
        || previous_settings.web_admin_password != settings.web_admin_password
        || previous_settings.web_admin_port != settings.web_admin_port;

    state.db.update_settings(&settings)?;
    let settings = refresh_settings_l1(state).await?;
    sync_autostart(&settings);

    let admin_relocated = settings.web_admin_port != previous_settings.web_admin_port;
    let proxy_was_running = state.proxy.read().await.is_some();
    let proxy_restart_required = proxy_was_running && requires_proxy_restart;

    let mut restart_info = RestartInfo {
        admin_relocated,
        new_admin_base_url: if admin_relocated {
            Some(format!(
                "http://127.0.0.1:{}/admin",
                settings.web_admin_port
            ))
        } else {
            None
        },
        proxy_restart_required,
        proxy_restarted: false,
    };

    if requires_proxy_restart {
        let state_for_restart = state.clone();
        let app_for_restart = app.clone();
        let previous_settings_for_restart = previous_settings.clone();
        let restart_work = async move {
            restart_proxy_if_running(
                app_for_restart.clone(),
                &state_for_restart,
                &previous_settings_for_restart,
            )
            .await?;

            if let Err(e) = crate::admin::restart_admin(
                state_for_restart.clone(),
                app_for_restart.clone(),
                state_for_restart.admin.clone(),
            )
            .await
            {
                log::error!("Failed to restart admin server after settings update: {e}");
            }
            Ok::<(), AppError>(())
        };

        if restart_async {
            // For async mode, mark that restart was triggered
            restart_info.proxy_restarted = true;
            tokio::spawn(async move {
                match restart_work.await {
                    Ok(_) => log::debug!("Settings side effects applied successfully"),
                    Err(e) => log::error!("Failed to apply settings runtime side effects: {e}"),
                }
            });
        } else {
            restart_work.await?;
            restart_info.proxy_restarted = true;
        }
    }

    Ok(Some(restart_info))
}

// ---------------------------------------------------------------------------
// Reusable helper: does not depend on tauri::State, for ServerApi and admin handler invocations
// ---------------------------------------------------------------------------

/// Get current settings from in-memory cache.
pub async fn get_settings_from_state(state: &AppState) -> Result<AppSettings, AppError> {
    Ok(state.settings.read().await.clone())
}

/// Completely update settings, trigger side effects like proxy/admin reload, and return updated settings.
pub async fn update_settings_from_state(
    app: Option<crate::AppEventHandle>,
    state: &AppState,
    settings: AppSettings,
) -> Result<AppSettings, AppError> {
    apply_settings_update(app, state, settings, false).await?;
    Ok(state.settings.read().await.clone())
}

/// Partially patch settings and return updated settings.
/// Note: web_admin_password protection logic is handled by the caller (e.g. tauri command).
pub async fn patch_settings_from_state(
    app: Option<crate::AppEventHandle>,
    state: &AppState,
    patch: serde_json::Value,
) -> Result<AppSettings, AppError> {
    let current = state.settings.read().await.clone();
    let merged = merge_settings_patch(&current, &patch)?;
    update_settings_from_state(app, state, merged).await
}

// ---------------------------------------------------------------------------
// Tauri command thin wrapper
// ---------------------------------------------------------------------------

#[cfg(feature = "gui")]
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, AppError> {
    get_settings_from_state(state.inner()).await
}

#[cfg(feature = "gui")]
#[tauri::command]
pub async fn update_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<(), AppError> {
    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.update_settings(settings).await?;
    Ok(())
}

#[cfg(feature = "gui")]
#[tauri::command]
pub async fn patch_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    patch: serde_json::Value,
) -> Result<AppSettings, AppError> {
    // Preserve original web_admin_password protection logic: keep original value when password is empty
    let current = state.settings.read().await.clone();
    let mut merged = merge_settings_patch(&current, &patch)?;
    if merged.web_admin_password.is_empty() {
        merged.web_admin_password = current.web_admin_password;
    }

    let api = crate::server_api::ServerApi::new(state.inner().clone(), Some(app));
    api.update_settings(merged).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_patch_changes_only_requested_field() {
        let current = AppSettings {
            access_key_required: false,
            listen_port: 8123,
            web_admin_enabled: true,
            web_admin_port: 9456,
            ..AppSettings::default()
        };

        let patched = merge_settings_patch(
            &current,
            &serde_json::json!({ "access_key_required": true }),
        )
        .unwrap();

        assert!(patched.access_key_required);
        assert_eq!(patched.listen_port, 8123);
        assert!(patched.web_admin_enabled);
        assert_eq!(patched.web_admin_port, 9456);
    }

    #[test]
    fn settings_patch_can_enable_raw_protocol_recording() {
        let current = AppSettings::default();

        let patched = merge_settings_patch(
            &current,
            &serde_json::json!({ "record_raw_protocol_data": true }),
        )
        .unwrap();

        assert!(patched.record_raw_protocol_data);
        assert!(patched.disable_reasoning);
    }
}
