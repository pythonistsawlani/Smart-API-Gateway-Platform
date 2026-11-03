use crate::admin::error::AdminError;
use crate::admin::state::{AdminState, SessionInfo};
use crate::database::AppSettings;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const SESSION_TTL_MINUTES: i64 = 30;

pub async fn version() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION")
    }))
}

pub async fn state_version() -> axum::Json<crate::state_version::StatVersionsResponse> {
    let v = crate::state_version::all();
    axum::Json(crate::state_version::StatVersionsResponse {
        log: v["log"],
        pool: v["pool"],
        channel: v["channel"],
        token: v["token"],
    })
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    token: String,
    expires_at: i64,
}

#[derive(Serialize)]
pub struct AdminStatus {
    running: bool,
    port: i32,
    runtime_mode: Option<String>,
}

#[derive(Serialize)]
pub struct SettingsResponse {
    data: AppSettings,
    _version: i64,
}

#[derive(Serialize)]
pub struct RestartResponse {
    ok: bool,
    _version: i64,
    restart: Option<RestartInfo>,
}

#[derive(Serialize)]
pub struct RestartInfo {
    pub admin_relocated: bool,
    pub new_admin_base_url: Option<String>,
    pub proxy_restart_required: bool,
    pub proxy_restarted: bool,
}

#[derive(Deserialize)]
pub struct UpdateSettingsRequest {
    data: AppSettings,
    _version: i64,
}

fn invalidate_sessions_for_username(
    sessions: &mut std::collections::HashMap<String, SessionInfo>,
    username: &str,
) -> usize {
    let before = sessions.len();
    sessions.retain(|_, session| session.username != username);
    before.saturating_sub(sessions.len())
}

fn settings_credentials_changed(current: &AppSettings, next: &AppSettings) -> bool {
    next.web_admin_username != current.web_admin_username
        || next.web_admin_password != current.web_admin_password
}

pub async fn login(
    State(state): State<AdminState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AdminError> {
    let now = chrono::Utc::now();
    let key = payload.username.clone();
    {
        let mut failures = state.login_failures.lock().await;
        let entry = failures.entry(key.clone()).or_default();
        if let Some(locked_until) = entry.locked_until {
            if locked_until > now {
                let retry_after_seconds = (locked_until - now).num_seconds().max(1);
                return Err(AdminError::RateLimited {
                    retry_after_seconds,
                    remaining_attempts: 0,
                    locked_until: locked_until.timestamp(),
                });
            }
            // Lockout expired: reset failure count but keep lockout_count for backoff
            entry.locked_until = None;
            entry.count = 0;
        }
    }

    let settings = state.settings.read().await.clone();
    if payload.username != settings.web_admin_username
        || payload.password != settings.web_admin_password
    {
        let mut failures = state.login_failures.lock().await;
        let entry = failures.entry(key).or_default();
        entry.count += 1;
        let remaining_attempts = 5_u32.saturating_sub(entry.count) as i64;
        let _ = state
            .db
            .add_audit_log("admin_login_failed", "invalid credentials");

        if entry.count >= 6 {
            // Exponential backoff: 5min * 2^lockout_count, capped at 8 hours
            let lockout_minutes = (5_u64).saturating_mul(2_u64.saturating_pow(entry.lockout_count)).min(480);
            let lock_expiry = now + chrono::Duration::minutes(lockout_minutes as i64);
            entry.locked_until = Some(lock_expiry);
            entry.lockout_count = entry.lockout_count.saturating_add(1);
            entry.count = 0;
            return Err(AdminError::RateLimited {
                retry_after_seconds: (lock_expiry - now).num_seconds().max(1),
                remaining_attempts: 0,
                locked_until: lock_expiry.timestamp(),
            });
        }

        return Err(AdminError::InvalidCredentials {
            remaining_attempts,
            locked_until: entry.locked_until.map(|value| value.timestamp()),
        });
    }

    state.login_failures.lock().await.remove(&payload.username);
    state
        .login_failures
        .lock()
        .await
        .remove(&settings.web_admin_username);

    // Clear any brute-force state for this username on successful login
    state.login_failures.lock().await.remove(&payload.username);

    let token = Uuid::new_v4().to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(SESSION_TTL_MINUTES);
    state.login_sessions.write().await.insert(
        token.clone(),
        SessionInfo {
            username: settings.web_admin_username.clone(),
            expires_at,
        },
    );
    let _ = state
        .db
        .add_audit_log("admin_login_success", &payload.username);

    Ok(Json(LoginResponse {
        token,
        expires_at: expires_at.timestamp(),
    }))
}

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

pub async fn logout(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AdminError> {
    if let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        state.login_sessions.write().await.remove(token);
        let _ = state.db.add_audit_log("admin_logout", "token_redacted");
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn status(State(state): State<AdminState>) -> Json<AdminStatus> {
    let settings = state.settings.read().await.clone();
    let runtime_mode = state
        .runtime
        .as_ref()
        .map(|runtime| match runtime.runtime_mode {
            crate::runtime_mode::RuntimeMode::Combined => "combined".to_string(),
            crate::runtime_mode::RuntimeMode::Standalone => "standalone".to_string(),
        });
    Json(AdminStatus {
        running: true,
        port: settings.web_admin_port,
        runtime_mode,
    })
}

pub async fn audit_logs(
    State(state): State<AdminState>,
) -> Result<Json<Vec<crate::database::AuditLogItem>>, AdminError> {
    Ok(Json(state.db.list_audit_logs(100)?))
}

pub async fn get_settings(State(state): State<AdminState>) -> Json<SettingsResponse> {
    let mut settings = state.settings.read().await.clone();
    let version = settings.updated_at;
    settings.web_admin_password.clear();
    Json(SettingsResponse {
        _version: version,
        data: settings,
    })
}

pub async fn patch_settings(
    State(state): State<AdminState>,
    Json(patch): Json<serde_json::Value>,
) -> Result<Json<SettingsResponse>, AdminError> {
    let current = state.settings.read().await.clone();
    let mut merged = crate::commands::config::merge_settings_patch(&current, &patch)?;
    if merged.web_admin_password.is_empty() {
        merged.web_admin_password = current.web_admin_password;
    }

    state.server_api()?.update_settings(merged).await?;

    let mut response_settings = state.settings.read().await.clone();
    let version = response_settings.updated_at;
    response_settings.web_admin_password.clear();
    Ok(Json(SettingsResponse {
        _version: version,
        data: response_settings,
    }))
}

pub async fn update_settings(
    State(state): State<AdminState>,
    Json(mut payload): Json<UpdateSettingsRequest>,
) -> Result<Json<RestartResponse>, AdminError> {
    let current = state.settings.read().await.clone();
    if payload._version != current.updated_at {
        return Err(AdminError::VersionMismatch {
            expected: payload._version,
            current: current.updated_at,
        });
    }

    let current_password = current.web_admin_password.clone();
    let current_username = current.web_admin_username.clone();

    if payload.data.web_admin_password.is_empty() {
        payload.data.web_admin_password = current_password.clone();
    }

    let credentials_changed = settings_credentials_changed(&current, &payload.data);
    let mut invalidated_session_count = 0usize;
    if credentials_changed {
        let mut sessions = state.login_sessions.write().await;
        invalidated_session_count +=
            invalidate_sessions_for_username(&mut sessions, &current_username);
    }
    let session_invalidated = credentials_changed;

    let restart_info = state
        .server_api()?
        .update_settings_with_restart(payload.data.clone(), true)
        .await?;
    let refreshed = state.settings.read().await.clone();
    let _ = state.db.add_audit_log(
        "admin_settings_updated",
        &format!(
            "port={}, enabled={}, version={}, session_invalidated={}, invalidated_session_count={}, username_changed={}, password_changed={}",
            refreshed.web_admin_port,
            refreshed.web_admin_enabled,
            refreshed.updated_at,
            session_invalidated,
            invalidated_session_count,
            payload.data.web_admin_username != current_username,
            payload.data.web_admin_password != current_password
        ),
    );
    Ok(Json(RestartResponse {
        ok: true,
        _version: refreshed.updated_at,
        restart: restart_info,
    }))
}
