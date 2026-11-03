use crate::database::{lock_conn, Database};
use crate::error::AppError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub proxy_enabled: bool,
    pub listen_port: i32,
    pub access_key_required: bool,
    pub circuit_failure_threshold: i32,
    pub proxy_connect_timeout_secs: u64,
    pub circuit_recovery_secs: i64,
    pub circuit_disable_codes: String,
    pub circuit_retry_codes: String,
    pub disable_keywords: String,
    pub keyword_freeze_scope: String,
    pub locale: String,
    pub theme: String,
    pub autostart: bool,
    pub start_minimized: bool,
    pub show_guide: bool,
    pub default_sort_mode: String,
    pub active_group: String,
    pub web_admin_enabled: bool,
    pub web_admin_username: String,
    pub web_admin_password: String,
    pub web_admin_port: i32,
    pub bind_external: bool,
    pub rate_limit_enabled: bool,
    pub rate_limit_rpm_per_ip: u32,
    pub rate_limit_rpm_per_key: u32,
    pub rate_limit_rpm_per_model: u32,
    pub cors_allowed_origins: String,
    pub tls_enabled: bool,
    pub tls_cert_path: String,
    pub tls_key_path: String,
    pub show_conversation_model: bool,
    pub disable_reasoning: bool,
    pub record_raw_protocol_data: bool,
    pub passthrough_header_injection: bool,
    pub app_version: String,
    #[serde(skip_serializing, skip_deserializing, default)]
    pub updated_at: i64,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            proxy_enabled: true,
            listen_port: 9090,
            access_key_required: true,
            circuit_failure_threshold: 5,
            proxy_connect_timeout_secs: 30,
            circuit_recovery_secs: 300,
            circuit_disable_codes: "401,403,410".to_string(),
            circuit_retry_codes: "100-199,300-399,401-407,409-499,500-503,505-523,525-599".to_string(),
            disable_keywords: "Your credit balance is too low\nThis organization has been disabled.\nYou exceeded your current quota\nPermission denied\nThe security token included in the request is invalid\nOperation not allowed\nYour account is not authorized\ninsufficient_quota\nquota_exceeded_error\ntoken plan limit exhausted\nUpstream rate limit exceeded\ninvalid api key\nUnauthorized - Invalid token".to_string(),
            keyword_freeze_scope: "model".to_string(),
            autostart: false,
            start_minimized: false,
            show_guide: true,
            default_sort_mode: "custom".to_string(),
            active_group: "auto".to_string(),
            web_admin_enabled: false,
            web_admin_username: String::new(),
            web_admin_password: String::new(),
            web_admin_port: 9099,
            bind_external: false,
            rate_limit_enabled: true,
            rate_limit_rpm_per_ip: 120,
            rate_limit_rpm_per_key: 240,
            rate_limit_rpm_per_model: 600,
            cors_allowed_origins: String::new(),
            tls_enabled: false,
            tls_cert_path: String::new(),
            tls_key_path: String::new(),
            show_conversation_model: false,
            disable_reasoning: true,
            record_raw_protocol_data: false,
            passthrough_header_injection: false,
            app_version: "0.6.9".to_string(),
            locale: String::new(),
            theme: String::new(),
            updated_at: 0,
            }
    }
}

impl Database {
    pub fn get_settings(&self) -> Result<AppSettings, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare("SELECT key, value FROM config")?;

        let kv: std::collections::HashMap<String, String> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let mut settings = AppSettings::default();

        if let Some(v) = kv.get("proxy_enabled") {
            settings.proxy_enabled = v == "1";
        }
        if let Some(v) = kv.get("listen_port") {
            settings.listen_port = v.parse().unwrap_or(9090);
        }
        if let Some(v) = kv.get("access_key_required") {
            settings.access_key_required = v == "1";
        }
        if let Some(v) = kv.get("circuit_failure_threshold") {
            settings.circuit_failure_threshold = v.parse().unwrap_or(5);
        }
        if let Some(v) = kv.get("proxy_connect_timeout_secs") {
            settings.proxy_connect_timeout_secs = v.parse().unwrap_or(30);
        }
        if let Some(v) = kv.get("circuit_recovery_secs") {
            settings.circuit_recovery_secs = v.parse().unwrap_or(300);
        }
        if let Some(v) = kv.get("circuit_disable_codes") {
            settings.circuit_disable_codes = v.clone();
        }
        if let Some(v) = kv.get("circuit_retry_codes") {
            settings.circuit_retry_codes = v.clone();
        }
        if let Some(v) = kv.get("disable_keywords") {
            settings.disable_keywords = v.clone();
        }
        if let Some(v) = kv.get("keyword_freeze_scope") {
            settings.keyword_freeze_scope = v.clone();
        }
        if let Some(v) = kv.get("locale") {
            settings.locale = v.clone();
        }
        if let Some(v) = kv.get("theme") {
            settings.theme = v.clone();
        }
        if let Some(v) = kv.get("autostart") {
            settings.autostart = v == "1";
        }
        if let Some(v) = kv.get("start_minimized") {
            settings.start_minimized = v == "1";
        }
        if let Some(v) = kv.get("show_guide") {
            settings.show_guide = v == "1";
        }
        if let Some(v) = kv.get("default_sort_mode") {
            settings.default_sort_mode = v.clone();
        }
        if let Some(v) = kv.get("active_group") {
            settings.active_group = v.clone();
        }
        if let Some(v) = kv.get("web_admin_enabled") {
            settings.web_admin_enabled = v == "1";
        }
        if let Some(v) = kv.get("web_admin_username") {
            settings.web_admin_username = v.clone();
        }
        if let Some(v) = kv.get("web_admin_password") {
            settings.web_admin_password = v.clone();
        }
        if let Some(v) = kv.get("web_admin_port") {
            settings.web_admin_port = v.parse().unwrap_or(9099);
        }
        if let Some(v) = kv.get("bind_external") {
            settings.bind_external = v == "1";
        }
        if let Some(v) = kv.get("rate_limit_enabled") {
            settings.rate_limit_enabled = v == "1";
        }
        if let Some(v) = kv.get("rate_limit_rpm_per_ip") {
            settings.rate_limit_rpm_per_ip = v.parse().unwrap_or(120);
        }
        if let Some(v) = kv.get("rate_limit_rpm_per_key") {
            settings.rate_limit_rpm_per_key = v.parse().unwrap_or(240);
        }
        if let Some(v) = kv.get("rate_limit_rpm_per_model") {
            settings.rate_limit_rpm_per_model = v.parse().unwrap_or(600);
        }
        if let Some(v) = kv.get("cors_allowed_origins") {
            settings.cors_allowed_origins = v.clone();
        }
        if let Some(v) = kv.get("tls_enabled") {
            settings.tls_enabled = v == "1";
        }
        if let Some(v) = kv.get("tls_cert_path") {
            settings.tls_cert_path = v.clone();
        }
        if let Some(v) = kv.get("tls_key_path") {
            settings.tls_key_path = v.clone();
        }
        if let Some(v) = kv.get("show_conversation_model") {
            settings.show_conversation_model = v == "1";
        }
        if let Some(v) = kv.get("disable_reasoning") {
            settings.disable_reasoning = v == "1";
        }
        if let Some(v) = kv.get("record_raw_protocol_data") {
            settings.record_raw_protocol_data = v == "1";
        }
        if let Some(v) = kv.get("passthrough_header_injection") {
            settings.passthrough_header_injection = v == "1";
        }
        if let Some(v) = kv.get("app_version") {
            settings.app_version = v.clone();
        }
        if let Some(v) = kv.get("updated_at") {
            settings.updated_at = v.parse().unwrap_or(0);
        }

        Ok(settings)
    }

    pub fn update_settings(&self, updates: &AppSettings) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let updated_at = chrono::Utc::now().timestamp_millis();

        let kv = [
            (
                "proxy_enabled",
                if updates.proxy_enabled { "1" } else { "0" },
            ),
            ("listen_port", &updates.listen_port.to_string()),
            (
                "access_key_required",
                if updates.access_key_required {
                    "1"
                } else {
                    "0"
                },
            ),
            (
                "circuit_failure_threshold",
                &updates.circuit_failure_threshold.to_string(),
            ),
            (
                "proxy_connect_timeout_secs",
                &updates.proxy_connect_timeout_secs.to_string(),
            ),
            (
                "circuit_recovery_secs",
                &updates.circuit_recovery_secs.to_string(),
            ),
            ("circuit_disable_codes", &updates.circuit_disable_codes),
            ("circuit_retry_codes", &updates.circuit_retry_codes),
            ("disable_keywords", &updates.disable_keywords),
            ("keyword_freeze_scope", &updates.keyword_freeze_scope),
            ("locale", &updates.locale),
            ("theme", &updates.theme),
            ("autostart", if updates.autostart { "1" } else { "0" }),
            (
                "start_minimized",
                if updates.start_minimized { "1" } else { "0" },
            ),
            ("show_guide", if updates.show_guide { "1" } else { "0" }),
            ("default_sort_mode", &updates.default_sort_mode),
            ("active_group", &updates.active_group),
            (
                "web_admin_enabled",
                if updates.web_admin_enabled { "1" } else { "0" },
            ),
            ("web_admin_username", &updates.web_admin_username),
            ("web_admin_password", &updates.web_admin_password),
            ("web_admin_port", &updates.web_admin_port.to_string()),
            (
                "bind_external",
                if updates.bind_external { "1" } else { "0" },
            ),
            (
                "rate_limit_enabled",
                if updates.rate_limit_enabled { "1" } else { "0" },
            ),
            (
                "rate_limit_rpm_per_ip",
                &updates.rate_limit_rpm_per_ip.to_string(),
            ),
            (
                "rate_limit_rpm_per_key",
                &updates.rate_limit_rpm_per_key.to_string(),
            ),
            (
                "rate_limit_rpm_per_model",
                &updates.rate_limit_rpm_per_model.to_string(),
            ),
            ("cors_allowed_origins", &updates.cors_allowed_origins),
            (
                "tls_enabled",
                if updates.tls_enabled { "1" } else { "0" },
            ),
            ("tls_cert_path", &updates.tls_cert_path),
            ("tls_key_path", &updates.tls_key_path),
            (
                "show_conversation_model",
                if updates.show_conversation_model {
                    "1"
                } else {
                    "0"
                },
            ),
            (
                "disable_reasoning",
                if updates.disable_reasoning { "1" } else { "0" },
            ),
            (
                "record_raw_protocol_data",
                if updates.record_raw_protocol_data {
                    "1"
                } else {
                    "0"
                },
            ),
            (
                "passthrough_header_injection",
                if updates.passthrough_header_injection {
                    "1"
                } else {
                    "0"
                },
            ),
            ("app_version", &updates.app_version),
            ("updated_at", &updated_at.to_string()),
        ];

        for (key, value) in kv {
            conn.execute(
                "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, value],
            )?;
        }

        Ok(())
    }

    pub fn get_config_value(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row("SELECT value FROM config WHERE key = ?1", [key], |row| {
            row.get(0)
        });

        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    pub fn set_config_value(&self, key: &str, value: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// Ensure admin credentials are configured (safe-by-default).
    ///
    /// Generates a random Web Admin password on first run, or when the
    /// legacy hardcoded `admin`/`admin` default is still active. Returns
    /// `Some(new_password)` when a password was generated so the caller can
    /// surface it to the user exactly once.
    pub fn ensure_admin_credentials(&self, settings: &mut AppSettings) -> Result<Option<String>, AppError> {
        let username = settings.web_admin_username.trim();
        let password = settings.web_admin_password.trim();
        let missing = username.is_empty() || password.is_empty();
        let is_default = username.eq_ignore_ascii_case("admin") && password == "admin";
        if !missing && !is_default {
            return Ok(None);
        }

        let generated = uuid::Uuid::new_v4().to_string().replace('-', "").to_lowercase();
        let new_username = if username.is_empty() {
            "admin".to_string()
        } else {
            username.to_string()
        };

        settings.web_admin_username = new_username.clone();
        settings.web_admin_password = generated.clone();
        self.set_config_value("web_admin_username", &new_username)?;
        self.set_config_value("web_admin_password", &generated)?;
        Ok(Some(generated))
    }
}
