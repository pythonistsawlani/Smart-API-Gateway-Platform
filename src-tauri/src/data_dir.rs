use crate::error::AppError;
use std::path::{Path, PathBuf};

#[cfg(all(feature = "gui", mobile))]
use tauri::Manager;

const APP_BINARY_NAMES: &[&str] = &["smart-api-gateway", "smart-api-gateway.exe"];

pub(crate) fn resolve_data_dir() -> Result<PathBuf, AppError> {
    resolve_data_dir_from(
        std::env::args_os().map(PathBuf::from),
        std::env::current_exe,
    )
}

pub(crate) fn database_path() -> Result<PathBuf, AppError> {
    database_path_in(resolve_data_dir()?)
}

#[cfg(feature = "gui")]
pub(crate) fn database_path_from_app(app: &tauri::AppHandle) -> Result<PathBuf, AppError> {
    #[cfg(mobile)]
    {
        return database_path_in(app.path().app_data_dir().map_err(|e| {
            AppError::Database(format!("Failed to resolve mobile app data dir: {e}"))
        })?);
    }

    #[cfg(not(mobile))]
    {
        let _ = app;
        database_path()
    }
}

fn database_path_in(dir: PathBuf) -> Result<PathBuf, AppError> {
    ensure_data_dir(&dir)?;
    Ok(dir.join("smart-api-gateway.db"))
}

fn ensure_data_dir(dir: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(dir).map_err(|e| {
        AppError::Database(format!(
            "Failed to create data directory {}: {e}",
            dir.display()
        ))
    })?;

    let probe = dir.join(".smart-api-gateway-write-test");
    std::fs::write(&probe, b"ok").map_err(|e| {
        AppError::Database(format!(
            "Data directory is not writable {}: {e}",
            dir.display()
        ))
    })?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

fn resolve_data_dir_from<I, F>(args: I, current_exe: F) -> Result<PathBuf, AppError>
where
    I: IntoIterator<Item = PathBuf>,
    F: FnOnce() -> std::io::Result<PathBuf>,
{
    if let Some(dir) = args
        .into_iter()
        .find_map(|arg| app_binary_parent_from_arg(&arg))
    {
        return Ok(dir);
    }

    let exe =
        current_exe().map_err(|e| AppError::Database(format!("Failed to get exe path: {e}")))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::Database("Failed to get executable parent directory".to_string()))
}

fn app_binary_parent_from_arg(arg: &Path) -> Option<PathBuf> {
    let file_name = arg.file_name()?.to_str()?;
    if !APP_BINARY_NAMES.contains(&file_name) {
        return None;
    }

    let parent = arg.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }

    Some(parent.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_invocation_uses_smart_api_gateway_argument_parent() {
        let dir = resolve_data_dir_from(
            [
                PathBuf::from(
                    "/data/data/com.termux/files/home/as-libs/combined/ld-linux-aarch64.so.1",
                ),
                PathBuf::from("--library-path"),
                PathBuf::from("/data/data/com.termux/files/home/as-libs/combined"),
                PathBuf::from("/data/data/com.termux/files/home/work/as/smart-api-gateway"),
                PathBuf::from("--nodisktop"),
            ],
            || {
                Ok(PathBuf::from(
                    "/data/data/com.termux/files/home/as-libs/combined/ld-linux-aarch64.so.1",
                ))
            },
        )
        .unwrap();

        assert_eq!(
            dir,
            PathBuf::from("/data/data/com.termux/files/home/work/as")
        );
    }

    #[test]
    fn direct_invocation_falls_back_to_current_exe_parent() {
        let dir = resolve_data_dir_from(
            [PathBuf::from("smart-api-gateway"), PathBuf::from("--nodisktop")],
            || Ok(PathBuf::from("/opt/smart-api-gateway/smart-api-gateway")),
        )
        .unwrap();

        assert_eq!(dir, PathBuf::from("/opt/smart-api-gateway"));
    }

    #[test]
    fn database_path_uses_requested_directory() {
        let dir =
            std::env::temp_dir().join(format!("smart-api-gateway-data-dir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let path = database_path_in(dir.clone()).unwrap();

        assert_eq!(path, dir.join("smart-api-gateway.db"));
        assert!(dir.exists());
        assert!(!dir.join(".smart-api-gateway-write-test").exists());

        let _ = std::fs::remove_dir_all(dir);
    }
}
