//! SERVER API facade: platform-agnostic entry points for core state mutations.
//!
//! `ServerApi` holds `AppState` and `AppEventHandle`, encapsulating proxy / config / channel /
//! pool / token facades so non-Tauri callers (e.g. admin HTTP handler,
//! future mobile bridge, headless CLI) can operate on core business logic with a unified interface.

pub mod channel;
pub mod config;
pub mod import_export;
pub mod pool;
pub mod proxy;
pub mod token;

use crate::AppState;

/// Platform-agnostic business operations facade.
///
/// In GUI mode `AppEventHandle = tauri::AppHandle`,
/// in headless mode `AppEventHandle = ()` (zero-sized struct, event emissions are no-ops).
///
/// Callers trigger core mutations via `ServerApi`
/// rather than directly depending on `tauri::State` or `#[tauri::command]`.
#[derive(Clone)]
pub struct ServerApi {
    state: AppState,
    app: Option<crate::AppEventHandle>,
}

impl ServerApi {
    /// Create a new `ServerApi` instance.
    ///
    /// - `state`: shared application state
    /// - `app`: event handle (`tauri::AppHandle` in GUI mode, None in headless mode)
    pub fn new(state: AppState, app: Option<crate::AppEventHandle>) -> Self {
        Self { state, app }
    }

    /// Get reference to internal `AppState`.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Safely emit frontend event; silently skipped when no handle is present (headless compatible).
    pub(crate) fn emit_event(&self, event: &str) {
        if let Some(handle) = &self.app {
            crate::event::emit(handle, event);
        }
    }
}
