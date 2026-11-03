use crate::admin::error::AdminError;
use crate::admin::state::AdminState;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::AppSettings;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{Mutex, RwLock};

    fn test_state() -> AdminState {
        AdminState {
            db: Arc::new(crate::database::Database {
                conn: std::sync::Mutex::new(
                    rusqlite::Connection::open_in_memory().unwrap(),
                ),
            }),
            settings: Arc::new(RwLock::new(AppSettings {
                web_admin_username: "admin".to_string(),
                ..AppSettings::default()
            })),
            login_sessions: Arc::new(RwLock::new(HashMap::new())),
            login_failures: Arc::new(Mutex::new(HashMap::new())),
            runtime: None,
            app_handle: None,
        }
    }

    async fn call_require_auth(
        state: AdminState,
        auth_header: Option<&str>,
    ) -> axum::http::StatusCode {
        use axum::middleware::from_fn_with_state;
        use axum::routing::get;
        use axum::Router;
        use tower::util::ServiceExt;

        let mut builder = axum::http::Request::builder().uri("/admin/status");
        if let Some(value) = auth_header {
            builder = builder.header("authorization", value);
        }
        let request = builder.body(axum::body::Body::empty()).unwrap();

        let app = Router::new()
            .route("/admin/status", get(|| async { axum::http::StatusCode::OK }))
            .layer(from_fn_with_state(state.clone(), require_auth))
            .with_state(());

        app.oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn missing_bearer_token_is_rejected_even_in_debug_builds() {
        let state = test_state();
        let status = call_require_auth(state, None).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_token_is_rejected() {
        let state = test_state();
        let status = call_require_auth(state, Some("Bearer invalid-token")).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_session_token_is_accepted() {
        let state = test_state();
        {
            let mut sessions = state.login_sessions.write().await;
            sessions.insert(
                "valid-token".to_string(),
                crate::admin::state::SessionInfo {
                    username: "admin".to_string(),
                    expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
                },
            );
        }
        let status = call_require_auth(state, Some("Bearer valid-token")).await;
        assert_eq!(status, axum::http::StatusCode::OK);
    }
}

pub async fn require_auth(
    State(state): State<AdminState>,
    request: Request,
    next: Next,
) -> Result<Response, AdminError> {
    use axum::http::header::AUTHORIZATION;
    const SESSION_TTL_MINUTES: i64 = 30;

    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string)
        .ok_or(AdminError::Unauthorized)?;

    let now = chrono::Utc::now();
    let current_username = state.settings.read().await.web_admin_username.clone();

    let mut sessions = state.login_sessions.write().await;
    sessions.retain(|_, session| session.expires_at > now);

    let session_valid = match sessions.get_mut(&token) {
        Some(session) if session.username == current_username => {
            session.expires_at = now + chrono::Duration::minutes(SESSION_TTL_MINUTES);
            true
        }
        _ => false,
    };

    if !session_valid {
        sessions.remove(&token);
        return Err(AdminError::Unauthorized);
    }

    Ok(next.run(request).await)
}
