use crate::state_version;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct TakeDirtyParams {
    pub module: String,
}

/// Retrieve version number of the specified module. Frontend compares with previous value; a change indicates an update
#[tauri::command]
pub async fn take_dirty(params: TakeDirtyParams) -> Result<u64, crate::AppError> {
    match params.module.as_str() {
        "log" => Ok(state_version::current("log")),
        "pool" => Ok(state_version::current("pool")),
        "channel" => Ok(state_version::current("channel")),
        "token" => Ok(state_version::current("token")),
        _ => Err(crate::AppError::Validation(format!(
            "Unknown module: {}",
            params.module
        ))),
    }
}
