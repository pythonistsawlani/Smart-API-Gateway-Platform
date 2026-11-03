use super::circuit_breaker::CircuitBreaker;
use super::handlers;
use super::middleware::request_id_middleware;
use super::rate_limiter::RateLimiter;
use super::responses_handler;
use crate::database::{AppSettings, Database};
use axum::extract::Query;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use http::header::HeaderValue;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, RwLock};
use tower_http::cors::{Any, CorsLayer};

/// Build a `CorsLayer` from the `cors_allowed_origins` setting.
///
/// An empty whitelist (the default) behaves as "no cross-origin": browsers
/// cannot send cross-site requests, while same-origin and non-browser clients
/// (curl, Tauri IPC, desktop apps) are unaffected.
fn build_cors_layer(settings: &AppSettings) -> CorsLayer {
    let origins: Vec<HeaderValue> = settings
        .cors_allowed_origins
        .split(',')
        .map(|origin| origin.trim())
        .filter(|origin| !origin.is_empty())
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    if origins.is_empty() {
        CorsLayer::new()
    } else {
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub address: String,
    pub port: i32,
}

/// Shared proxy state
#[derive(Clone)]
pub struct ProxyState {
    pub db: Arc<Database>,
    pub settings: Arc<RwLock<AppSettings>>,
    pub circuit_breakers: Arc<RwLock<HashMap<String, CircuitBreaker>>>,
    pub failure_counts: Arc<RwLock<HashMap<String, u32>>>, // Entry ID -> consecutive failure count
    pub app_handle: Option<crate::AppEventHandle>,
    pub http_client: reqwest::Client,
    pub response_store: Arc<RwLock<HashMap<String, serde_json::Value>>>,
    pub rate_limiter: Arc<RateLimiter>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AzureDeploymentsQuery {
    #[serde(rename = "api-version")]
    _api_version: Option<String>,
}

/// HTTP proxy server
pub struct ProxyServer {
    port: i32,
    connect_timeout_secs: u64,
    bind_external: bool,
    state: ProxyState,
    shutdown_tx: Arc<RwLock<Option<oneshot::Sender<()>>>>,
}

impl ProxyServer {
    pub fn new(
        port: i32,
        db: Arc<Database>,
        settings: Arc<RwLock<AppSettings>>,
        app_handle: Option<crate::AppEventHandle>,
        failure_counts: Arc<RwLock<HashMap<String, u32>>>,
    ) -> Self {
        let (connect_timeout_secs, bind_external) = settings
            .try_read()
            .map(|settings| {
                (
                    settings.proxy_connect_timeout_secs.clamp(1, 300),
                    settings.bind_external,
                )
            })
            .unwrap_or((30, false));
        let state = ProxyState {
            db,
            settings,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            failure_counts,
            app_handle,
            http_client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(connect_timeout_secs))
                .gzip(true)
                .build()
                .expect("failed to build proxy HTTP client"),
            response_store: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(RateLimiter::new()),
        };

        Self {
            port,
            connect_timeout_secs,
            bind_external,
            state,
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn start(&self) -> Result<(), String> {
        self.start_with_admin(None).await
    }

    pub async fn start_with_admin(&self, admin_router: Option<Router>) -> Result<(), String> {
        if self.shutdown_tx.read().await.is_some() {
            return Err("Proxy already running".to_string());
        }

        // Safe-by-default: bind to loopback unless bind_external is enabled.
        let bind_host = if self.bind_external {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        };
        let addr: SocketAddr = format!("{bind_host}:{}", self.port)
            .parse()
            .map_err(|e| format!("Invalid address: {e}"))?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let app = self.build_router(admin_router);

        let settings = self
            .state
            .settings
            .try_read()
            .map(|settings| settings.clone())
            .unwrap_or_default();

        let tls_enabled = settings.tls_enabled;
        let tls_cert_path = settings.tls_cert_path.clone();
        let tls_key_path = settings.tls_key_path.clone();

        if tls_enabled {
            let cert = PathBuf::from(&tls_cert_path);
            let key = PathBuf::from(&tls_key_path);
            if !cert.is_file() || !key.is_file() {
                return Err(format!(
                    "TLS is enabled but certificate/key files are missing: {} / {}",
                    tls_cert_path, tls_key_path
                ));
            }
            let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .map_err(|e| format!("Failed to load TLS certificate/key: {e}"))?;
            *self.shutdown_tx.write().await = Some(shutdown_tx);
            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();
            tokio::spawn(async move {
                tokio::spawn(async move {
                    let _ = shutdown_rx.await;
                    shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
                });
                axum_server::bind_rustls(addr, tls_config)
                    .handle(handle)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
                    .unwrap_or_else(|e| log::error!("Proxy HTTPS server error: {e}"));
                log::info!("Proxy server stopped");
            });
        } else {
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .map_err(|e| format!("Failed to bind: {e}"))?;
            *self.shutdown_tx.write().await = Some(shutdown_tx);
            tokio::spawn(async move {
                axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap_or_else(|e| {
                        log::error!("Proxy server error: {e}");
                    });
                log::info!("Proxy server stopped");
            });
        }

        if self.bind_external {
            log::warn!(
                "Proxy server is bound to 0.0.0.0:{port} (bind_external=true) and is reachable \
                 from the network. This is unsafe unless rate limiting and access keys are enabled.",
                port = self.port
            );
        } else {
            log::info!(
                "Proxy server bound to 127.0.0.1:{port}. Set bind_external=true in Settings \
                 to expose it on the network.",
                port = self.port
            );
        }

        let access_key_required = self
            .state
            .settings
            .try_read()
            .map(|settings| settings.access_key_required)
            .unwrap_or(true);
        if !access_key_required {
            log::warn!(
                "access_key_required is DISABLED: the proxy accepts requests without an access \
                 key. Enable access keys in Settings unless this is a trusted local-only setup."
            );
        }

        log::info!(
            "Proxy server started on {addr}{} connect_timeout={}s",
            if tls_enabled { " (TLS)" } else { "" },
            self.connect_timeout_secs
        );

        Ok(())
    }

    /// Build the axum router (proxy routes + optional admin merge) with the
    /// CORS and request-ID layers. Extracted from `start_with_admin` so tests
    /// can exercise the full request pipeline without binding a socket.
    fn build_router(&self, admin_router: Option<Router>) -> Router {
        let cors = self
            .state
            .settings
            .try_read()
            .map(|settings| build_cors_layer(&settings))
            .unwrap_or_else(|_| CorsLayer::new());

        let mut app = Router::new()
            .route("/health", get(handlers::health_check))
            .route(
                "/v1/chat/completions",
                post(handlers::handle_chat_completions),
            )
            .route("/v1/messages", post(handlers::handle_messages))
            .route("/v1/models", get(handlers::handle_list_models))
            .route(
                "/anthropic/v1/models",
                get(handlers::handle_list_models_claude),
            )
            .route("/v1beta/models", get(handlers::handle_list_models_gemini))
            .route("/v1beta/openai/models", get(handlers::handle_list_models))
            .route(
                "/openai/deployments",
                get(|state, query: Query<AzureDeploymentsQuery>| async move {
                    handlers::handle_list_models_azure(state, query).await
                }),
            )
            // Gemini native endpoint (generateContent + streamGenerateContent)
            .route("/v1beta/models/*rest", post(handlers::handle_gemini_native))
            // Gemini single model detail
            .route(
                "/v1beta/models/{model}",
                get(handlers::handle_gemini_model_detail),
            )
            // Azure native endpoint
            .route(
                "/openai/deployments/*rest",
                post(handlers::handle_azure_chat),
            )
            // OpenAI Responses API (Chat Completions format under the hood)
            .route("/v1/responses", post(responses_handler::handle_responses))
            .route(
                "/v1/responses/:response_id",
                get(responses_handler::get_response).delete(responses_handler::delete_response),
            )
            .route(
                "/v1/responses/:response_id/cancel",
                post(responses_handler::cancel_response),
            )
            .layer(cors)
            .layer(middleware::from_fn(request_id_middleware))
            .with_state(self.state.clone());

        if let Some(admin_router) = admin_router {
            app = app.merge(admin_router);
        }
        app
    }

    pub async fn stop(&self) -> Result<(), String> {
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
            Ok(())
        } else {
            Err("Proxy not running".to_string())
        }
    }

    pub fn get_status(&self) -> ProxyStatus {
        let running = self
            .shutdown_tx
            .try_read()
            .map(|guard| guard.is_some())
            .unwrap_or(true);

        ProxyStatus {
            running,
            address: "127.0.0.1".to_string(),
            port: self.port,
        }
    }
}
