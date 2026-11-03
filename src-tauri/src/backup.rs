use crate::data_dir;
use crate::AppError;
use chrono::Local;
use std::fs;
use std::path::Path;

/// Run the backup and cleanup process
pub fn run_backup() {
    let db_path = match data_dir::database_path() {
        Ok(path) => path,
        Err(e) => {
            log::error!("Backup failed: Failed to resolve database path: {e}");
            return;
        }
    };
    if !db_path.exists() {
        return;
    }

    if let Err(e) = backup_database(&db_path) {
        log::error!("Database backup failed: {e}");
    }

    let Some(data_dir) = db_path.parent() else {
        log::error!("Backup cleanup failed: Failed to get database parent directory");
        return;
    };

    let backups_dir = data_dir.join("backups");
    if let Err(e) = cleanup_old_backups(&backups_dir, 7) {
        log::error!("Backup cleanup failed: {e}");
    }
}

/// Backup the database and its sidecar files
fn backup_database(db_path: &Path) -> Result<(), AppError> {
    let today = Local::now().format("%Y-%m-%d").to_string();
    let exe_dir = db_path
        .parent()
        .ok_or_else(|| AppError::Database("Failed to get database parent directory".to_string()))?;

    let backup_root = exe_dir.join("backups");
    let today_dir = backup_root.join(&today);

    if today_dir.exists() {
        // Backup for today already exists, skip
        return Ok(());
    }

    fs::create_dir_all(&today_dir)
        .map_err(|e| AppError::Database(format!("Failed to create backup directory: {e}")))?;

    let db_file_name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("smart-api-gateway.db");
    let target_db_path = today_dir.join(db_file_name);

    // Copy main database file
    fs::copy(db_path, &target_db_path)
        .map_err(|e| AppError::Database(format!("Failed to copy database: {e}")))?;

    // Copy sidecar files if they exist
    for suffix in ["-wal", "-shm"] {
        let sidecar_path = db_path.with_file_name(format!("{}{}", db_file_name, suffix));
        if sidecar_path.exists() {
            let target_sidecar_path = today_dir.join(format!("{}{}", db_file_name, suffix));
            let _ = fs::copy(sidecar_path, target_sidecar_path);
        }
    }

    log::info!("Database backed up to {}", today_dir.display());
    Ok(())
}

/// Cleanup backups older than max_days
fn cleanup_old_backups(backups_dir: &Path, max_days: u64) -> Result<(), AppError> {
    if !backups_dir.exists() {
        return Ok(());
    }

    let now = Local::now().naive_local().date();
    let entries = fs::read_dir(backups_dir)
        .map_err(|e| AppError::Database(format!("Failed to read backups directory: {e}")))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
            // Attempt to parse folder name as YYYY-MM-DD
            if let Ok(date) = chrono::NaiveDate::parse_from_str(dir_name, "%Y-%m-%d") {
                let age = now.signed_duration_since(date);
                if age.num_days() > max_days as i64 {
                    log::info!("Deleting old backup: {}", path.display());
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
    }

    Ok(())
}
