pub mod dao;
mod schema;

use crate::data_dir;
use crate::embedded_pool;
use crate::error::AppError;
use chrono::Utc;
use rusqlite::Connection;
use std::{path::Path, sync::Mutex};

/// Macro to safely lock the database connection
macro_rules! lock_conn {
    ($mutex:expr) => {
        $mutex
            .lock()
            .map_err(|e| AppError::Database(format!("Mutex lock failed: {}", e)))?
    };
}

pub(crate) use lock_conn;

pub use dao::*;

/// Database connection wrapper
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
}

impl Database {
    /// Open database next to the real smart-api-gateway executable (portable mode).
    pub fn open() -> Result<Self, AppError> {
        Self::open_at(data_dir::database_path()?)
    }

    /// Open database at an already resolved platform data path.
    pub fn open_at(db_path: impl AsRef<Path>) -> Result<Self, AppError> {
        let conn = open_or_recover(db_path.as_ref())?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| AppError::Database(format!("Failed to set pragmas: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create all tables and initialize required default data.
    pub fn create_tables(&self) -> Result<(), AppError> {
        {
            let conn = lock_conn!(self.conn);
            schema::create_tables(&conn)?;
        }
        self.ensure_default_channel_seed()
    }

    fn ensure_default_channel_seed(&self) -> Result<(), AppError> {
        if !self.list_channels()?.is_empty() {
            return Ok(());
        }

        let xor_key: u8 = 0xA5;
        let decrypted: Vec<u8> = embedded_pool::POOL
            .iter()
            .map(|&byte| byte ^ xor_key)
            .collect();
        let Ok(text) = String::from_utf8(decrypted) else {
            return Ok(());
        };
        let keys: Vec<&str> = text
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();
        if keys.is_empty() {
            return Ok(());
        }

        let pick = (chrono::Utc::now().timestamp_subsec_micros() as usize) % keys.len();
        let channel = self.create_channel(
            "test api",
            "custom",
            "https://open.bigmodel.cn/api/paas/v4",
            keys[pick],
            None,
            None,
        )?;

        self.create_entry(
            &channel.id,
            "glm-4-flash",
            "glm-4-flash",
            0,
            "",
            "",
            "",
            "",
            "auto",
        )?;

        Ok(())
    }
}

fn open_or_recover(db_path: &Path) -> Result<Connection, AppError> {
    let conn = Connection::open(db_path)
        .map_err(|e| AppError::Database(format!("Failed to open db: {e}")))?;

    if !is_database_healthy(&conn) {
        drop(conn);
        backup_corrupt_database(db_path)?;
        return Connection::open(db_path)
            .map_err(|e| AppError::Database(format!("Failed to recreate db: {e}")));
    }

    Ok(conn)
}

fn is_database_healthy(conn: &Connection) -> bool {
    conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map(|result| result.eq_ignore_ascii_case("ok"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_database() -> Database {
        Database {
            conn: Mutex::new(Connection::open_in_memory().unwrap()),
        }
    }

    #[test]
    fn create_tables_populates_default_channel_seed() {
        let db = in_memory_database();

        db.create_tables().unwrap();

        let channels = db.list_channels().unwrap();
        let entries = db.list_entries().unwrap();

        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "test api");
        assert_eq!(channels[0].api_type, "openai");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "glm-4-flash");
        assert_eq!(entries[0].group_name.as_deref(), Some("auto"));
    }
}

fn backup_corrupt_database(db_path: &Path) -> Result<(), AppError> {
    if !db_path.exists() {
        return Ok(());
    }

    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    let backup_path = db_path.with_file_name(format!("smart-api-gateway.corrupt.{timestamp}.db"));

    std::fs::rename(db_path, &backup_path).map_err(|e| {
        AppError::Database(format!(
            "Database is corrupted and failed to move it to {}: {e}",
            backup_path.display()
        ))
    })?;

    log::warn!(
        "Database was corrupted and has been moved to {}. A new database will be created.",
        backup_path.display()
    );

    for suffix in ["-wal", "-shm"] {
        let file_name = db_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("smart-api-gateway.db");
        let sidecar_path = db_path.with_file_name(format!("{file_name}{suffix}"));
        if sidecar_path.exists() {
            let backup_file_name = backup_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("smart-api-gateway.corrupt.db");
            let sidecar_backup_path =
                backup_path.with_file_name(format!("{backup_file_name}{suffix}"));
            let _ = std::fs::rename(sidecar_path, sidecar_backup_path);
        }
    }

    Ok(())
}
