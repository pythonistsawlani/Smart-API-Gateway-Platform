use crate::admin::auth::require_auth;
use crate::admin::channel_handlers;
use crate::admin::chat_handlers;
use crate::admin::connection_apps_handlers;
use crate::admin::handlers;
use crate::admin::import_export_handlers;
use crate::admin::pool_handlers;
use crate::admin::proxy_handlers;
use crate::admin::state::AdminState;
use crate::admin::token_handlers;
use crate::admin::translation_handlers;
use crate::admin::usage_handlers;
use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;

pub fn build_admin_router(state: AdminState) -> Router {
    let protected = Router::new()
        .route("/admin/logout", post(handlers::logout))
        .route("/admin/status", get(handlers::status))
        .route("/admin/audit-logs", get(handlers::audit_logs))
        .route(
            "/admin/settings",
            get(handlers::get_settings)
                .put(handlers::update_settings)
                .patch(handlers::patch_settings),
        )
        // Channel API routes - all require auth
        .route(
            "/admin/channels",
            get(channel_handlers::list).post(channel_handlers::create),
        )
        .route(
            "/admin/channels/paginated",
            get(channel_handlers::list_paginated),
        )
        .route(
            "/admin/channels/save-with-models",
            post(channel_handlers::save_with_models),
        )
        .route(
            "/admin/channels/:id",
            get(channel_handlers::list)
                .put(channel_handlers::update)
                .delete(channel_handlers::delete),
        )
        .route(
            "/admin/channels/:id/fetch-models",
            post(channel_handlers::fetch_models),
        )
        .route(
            "/admin/channels/fetch-models-direct",
            post(channel_handlers::fetch_models_direct),
        )
        .route(
            "/admin/channels/probe-url",
            post(channel_handlers::probe_url),
        )
        .route(
            "/admin/channels/:id/select-models",
            post(channel_handlers::select_models),
        )
        .route(
            "/admin/channels/:id/response-ms",
            put(channel_handlers::update_response_ms),
        )
        .route(
            "/admin/channels/:id/test",
            post(channel_handlers::test_channel),
        )
        .route(
            "/admin/import-export/channel-model/export",
            get(import_export_handlers::export_channel_model_transfer),
        )
        .route(
            "/admin/import-export/channel-model/preview",
            post(import_export_handlers::preview_channel_model_transfer),
        )
        .route(
            "/admin/import-export/channel-model/import",
            post(import_export_handlers::import_channel_model_transfer),
        )
        .route(
            "/admin/channels/test-direct",
            post(channel_handlers::test_channel_direct),
        )
        // Pool API routes - all require auth
        .route(
            "/admin/pool",
            get(pool_handlers::list).post(pool_handlers::create),
        )
        .route("/admin/pool/paginated", get(pool_handlers::list_paginated))
        .route("/admin/pool/:id/toggle", put(pool_handlers::toggle))
        .route("/admin/pool/:id", delete(pool_handlers::delete))
        .route("/admin/pool/reorder", post(pool_handlers::reorder))
        .route(
            "/admin/pool/:id/test-latency",
            post(pool_handlers::test_latency),
        )
        .route(
            "/admin/pool/backfill-catalog-meta",
            post(pool_handlers::backfill_catalog_meta),
        )
        .route("/admin/pool/groups", get(pool_handlers::get_groups))
        .route(
            "/admin/pool/:id/display-name",
            put(pool_handlers::update_display_name),
        )
        .route("/admin/pool/:id/group", put(pool_handlers::update_group))
        // Token API routes - all require auth
        .route(
            "/admin/tokens",
            get(token_handlers::list_tokens).post(token_handlers::create_token),
        )
        .route(
            "/admin/tokens/paginated",
            get(token_handlers::list_tokens_paginated),
        )
        .route("/admin/tokens/:id", delete(token_handlers::delete_token))
        .route(
            "/admin/tokens/:id/toggle",
            put(token_handlers::toggle_token),
        )
        // Usage/Dashboard API routes - all require auth
        .route("/admin/logs", get(usage_handlers::get_logs))
        .route(
            "/admin/dashboard/stats",
            get(usage_handlers::get_dashboard_stats),
        )
        .route(
            "/admin/dashboard/model-consumption",
            get(usage_handlers::get_model_consumption),
        )
        .route(
            "/admin/dashboard/call-trend",
            get(usage_handlers::get_call_trend),
        )
        .route(
            "/admin/dashboard/model-distribution",
            get(usage_handlers::get_model_distribution),
        )
        .route(
            "/admin/dashboard/model-ranking",
            get(usage_handlers::get_model_ranking),
        )
        .route(
            "/admin/dashboard/user-ranking",
            get(usage_handlers::get_user_ranking),
        )
        .route(
            "/admin/dashboard/user-trend",
            get(usage_handlers::get_user_trend),
        )
        .route("/admin/proxy/status", get(proxy_handlers::get_status))
        .route("/admin/proxy/start", post(proxy_handlers::start))
        .route("/admin/proxy/stop", post(proxy_handlers::stop))
        .route("/admin/test-chat", post(chat_handlers::test_chat))
        .route(
            "/admin/connection-apps",
            get(connection_apps_handlers::list),
        )
        .route(
            "/admin/connection-apps/:id/execute",
            post(connection_apps_handlers::execute),
        )
        .route(
            "/admin/translation-relay",
            get(translation_handlers::get_translation_relay),
        )
        .route("/admin/state-version", get(handlers::state_version))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        // Root-level serving for eventual 9090/ migration (compatible with /admin/*)
        .route("/", get(crate::admin::static_files::admin_index))
        .route(
            "/assets/*path",
            get(crate::admin::static_files::admin_asset_root),
        )
        .route(
            "/star.jpg",
            get(crate::admin::static_files::admin_asset_root),
        )
        .route(
            "/favicon.ico",
            get(crate::admin::static_files::admin_asset_root),
        )
        .route(
            "/logo/*path",
            get(crate::admin::static_files::admin_asset_root),
        )
        // Admin API routes (login, health, version)
        .route("/admin/login", post(handlers::login))
        .route("/admin/health", get(handlers::health))
        .route("/admin/version", get(handlers::version))
        .merge(protected)
        .with_state(state)
}
