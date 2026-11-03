use crate::error::AppError;
use rusqlite::Connection;

pub fn create_tables(conn: &Connection) -> Result<(), AppError> {
    // 1. Channels
    conn.execute(
        "CREATE TABLE IF NOT EXISTS channels (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            api_type TEXT NOT NULL DEFAULT 'openai',
            base_url TEXT NOT NULL,
            api_key TEXT NOT NULL,
            available_models TEXT DEFAULT '[]',
            selected_models TEXT DEFAULT '[]',
            enabled INTEGER DEFAULT 1,
            last_fetch_at INTEGER DEFAULT 0,
            notes TEXT,
            upstream_headers TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    // 2. API Entries
    conn.execute(
        "CREATE TABLE IF NOT EXISTS api_entries (
            id TEXT PRIMARY KEY,
            channel_id TEXT NOT NULL,
            model TEXT NOT NULL,
            display_name TEXT NOT NULL,
            sort_index INTEGER DEFAULT 0,
            enabled INTEGER DEFAULT 1,
            cooldown_until INTEGER,
            response_ms TEXT DEFAULT '',
            provider_logo TEXT DEFAULT '',
            release_date TEXT DEFAULT '',
            model_meta_zh TEXT DEFAULT '',
            model_meta_en TEXT DEFAULT '',
            score REAL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
        )",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    // 3. Access Keys
    conn.execute(
        "CREATE TABLE IF NOT EXISTS access_keys (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            key TEXT NOT NULL UNIQUE,
            enabled INTEGER DEFAULT 1,
            created_at INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    // 4. Usage Logs
    conn.execute(
        "CREATE TABLE IF NOT EXISTS usage_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            type INTEGER NOT NULL DEFAULT 2,
            content TEXT DEFAULT '',
            access_key_id TEXT,
            access_key_name TEXT DEFAULT 'auto',
            token_name TEXT DEFAULT '',
            api_entry_id TEXT NOT NULL,
            channel_id TEXT NOT NULL,
            channel_name TEXT NOT NULL,
            model TEXT NOT NULL,
            requested_model TEXT NOT NULL,
            quota INTEGER DEFAULT 0,
            is_stream INTEGER NOT NULL,
            prompt_tokens INTEGER DEFAULT 0,
            completion_tokens INTEGER DEFAULT 0,
            latency_ms INTEGER DEFAULT 0,
            first_token_ms INTEGER DEFAULT 0,
            use_time INTEGER DEFAULT 0,
            status_code INTEGER DEFAULT 0,
            success INTEGER NOT NULL,
            request_id TEXT DEFAULT '',
            log_group TEXT DEFAULT '',
            other TEXT DEFAULT '',
            error_message TEXT,
            ip TEXT,
            created_at INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    ensure_api_entry_columns(conn)?;
    ensure_usage_log_columns(conn)?;
    ensure_channel_columns(conn)?;

    // Migrate api_type values in channels table
    // custom -> openai, claude -> anthropic
    conn.execute(
        "UPDATE channels SET api_type = 'openai' WHERE api_type = 'custom'",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    conn.execute(
        "UPDATE channels SET api_type = 'anthropic' WHERE api_type = 'claude'",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    // Indexes
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_usage_logs_created_at ON usage_logs(created_at)",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_usage_logs_model ON usage_logs(model)",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_usage_logs_access_key ON usage_logs(access_key_id)",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_usage_logs_channel ON usage_logs(channel_id)",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            action TEXT NOT NULL,
            detail TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    // 5. Config
    conn.execute(
        "CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    // Insert default config values
    let defaults = [
        ("proxy_enabled", "1"),
        ("listen_port", "9090"),
        ("access_key_required", "1"),
        ("circuit_failure_threshold", "5"),
        ("proxy_connect_timeout_secs", "30"),
        ("circuit_recovery_secs", "300"),
        ("circuit_disable_codes", "401,403,410"),
        ("circuit_retry_codes", "100-199,300-399,401-407,409-499,500-503,505-523,525-599"),
        ("disable_keywords", "Your credit balance is too low\nThis organization has been disabled.\nYou exceeded your current quota\nPermission denied\nThe security token included in the request is invalid\nOperation not allowed\nYour account is not authorized\ninsufficient_quota\nquota_exceeded_error\ntoken plan limit exhausted\nUpstream rate limit exceeded\ninvalid api key\nUnauthorized - Invalid token"),
        ("keyword_freeze_scope", "model"),
        ("locale", "en"),
        ("theme", "light"),
        ("show_guide", "1"),
        ("autostart", "0"),
        ("start_minimized", "0"),
        ("default_sort_mode", "custom"),
        ("active_group", "auto"),
        ("web_admin_enabled", "1"),
        ("web_admin_port", "9090"),
        ("bind_external", "0"),
        ("rate_limit_enabled", "1"),
        ("rate_limit_rpm_per_ip", "120"),
        ("rate_limit_rpm_per_key", "240"),
        ("rate_limit_rpm_per_model", "600"),
        ("cors_allowed_origins", ""),
        ("tls_enabled", "0"),
        ("tls_cert_path", ""),
        ("tls_key_path", ""),
        ("show_conversation_model", "0"),
        ("disable_reasoning", "1"),
        ("record_raw_protocol_data", "0"),
        ("passthrough_header_injection", "0"),
        ("app_version", "0.6.9"),
    ];

    for (key, value) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO config (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }

    // Migrate old default circuit values to the personal-version cooldown defaults.
    // Preserve explicitly customized values by only rewriting the previous defaults.
    conn.execute(
        "UPDATE config SET value = '5' WHERE key = 'circuit_failure_threshold' AND value IN ('1', '3', '4')",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    conn.execute(
        "UPDATE config SET value = '300' WHERE key = 'circuit_recovery_secs' AND value = '60'",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    conn.execute(
        "UPDATE config SET value = '401,403,410' WHERE key = 'circuit_disable_codes' AND value = '401'",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(())
}

fn ensure_api_entry_columns(conn: &Connection) -> Result<(), AppError> {
    ensure_column(conn, "api_entries", "cooldown_until", "INTEGER")?;
    ensure_column(conn, "api_entries", "response_ms", "TEXT DEFAULT ''")?;
    ensure_column(conn, "api_entries", "provider_logo", "TEXT DEFAULT ''")?;
    ensure_column(conn, "api_entries", "release_date", "TEXT DEFAULT ''")?;
    ensure_column(conn, "api_entries", "model_meta_zh", "TEXT DEFAULT ''")?;
    ensure_column(conn, "api_entries", "model_meta_en", "TEXT DEFAULT ''")?;
    ensure_column(conn, "api_entries", "score", "REAL DEFAULT 0")?;
    // group_name grouping field
    ensure_column(
        conn,
        "api_entries",
        "group_name",
        "TEXT NOT NULL DEFAULT 'auto'",
    )?;
    conn.execute(
        "UPDATE api_entries SET group_name = 'auto' WHERE group_name IS NULL OR TRIM(group_name) = ''",
        [],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(())
}

fn ensure_channel_columns(conn: &Connection) -> Result<(), AppError> {
    ensure_column(conn, "channels", "response_ms", "TEXT DEFAULT ''")?;
    ensure_column(conn, "channels", "upstream_headers", "TEXT")?;

    // Legacy data migration: persist temporary hardcoded headers into channel configuration
    let claude_headers = r#"{"user-agent":"claude-cli/2.1.176 (external, cli)","x-app":"cli","anthropic-beta":"claude-code-20250219"}"#;
    let codex_headers = r#"{"originator":"codex_cli_rs"}"#;

    // Existing channels with api_type = 'anthropic' write Claude CLI Header
    conn.execute(
        "UPDATE channels SET upstream_headers = ?1 WHERE (upstream_headers IS NULL OR TRIM(upstream_headers) = '') AND api_type = 'anthropic'",
        rusqlite::params![claude_headers],
    ).map_err(|e| AppError::Database(e.to_string()))?;

    // Channels with api_type = 'responses' and base_url containing 'codex' write CODEX Header
    conn.execute(
        "UPDATE channels SET upstream_headers = ?1 WHERE (upstream_headers IS NULL OR TRIM(upstream_headers) = '') AND api_type = 'responses' AND base_url LIKE '%codex%'",
        rusqlite::params![codex_headers],
    ).map_err(|e| AppError::Database(e.to_string()))?;

    Ok(())
}

fn ensure_usage_log_columns(conn: &Connection) -> Result<(), AppError> {
    ensure_column(conn, "usage_logs", "type", "INTEGER NOT NULL DEFAULT 2")?;
    ensure_column(conn, "usage_logs", "content", "TEXT DEFAULT ''")?;
    ensure_column(conn, "usage_logs", "token_name", "TEXT DEFAULT ''")?;
    ensure_column(conn, "usage_logs", "quota", "INTEGER DEFAULT 0")?;
    ensure_column(conn, "usage_logs", "use_time", "INTEGER DEFAULT 0")?;
    ensure_column(conn, "usage_logs", "request_id", "TEXT DEFAULT ''")?;
    ensure_column(conn, "usage_logs", "log_group", "TEXT DEFAULT ''")?;
    ensure_column(conn, "usage_logs", "other", "TEXT DEFAULT ''")?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), AppError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| AppError::Database(e.to_string()))?;

    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| AppError::Database(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(e.to_string()))?;

    if !columns.iter().any(|existing| existing == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }

    Ok(())
}
