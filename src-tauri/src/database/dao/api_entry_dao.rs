use crate::database::dao::PaginatedResult;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params_from_iter;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEntry {
    pub id: String,
    pub channel_id: String,
    pub model: String,
    pub display_name: String,
    pub sort_index: i32,
    pub enabled: bool,
    #[serde(default)]
    pub cooldown_until: Option<i64>,
    #[serde(default = "default_circuit_state")]
    pub circuit_state: String,
    pub created_at: i64,
    pub updated_at: i64,
    // Joined from channel
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_api_type: Option<String>,
    // Model's owned_by from channel_api_type mapping
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub owned_by: Option<String>,
    // Response time in milliseconds string, or "X" on failure.
    #[serde(default)]
    pub response_ms: Option<String>,
    #[serde(default)]
    pub provider_logo: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub model_meta_zh: Option<String>,
    #[serde(default)]
    pub model_meta_en: Option<String>,
    // Group name for entry grouping
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub score: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntryCatalogMetaInput {
    pub id: String,
    pub display_name: String,
    pub provider_logo: String,
    pub release_date: String,
    pub model_meta_zh: String,
    pub model_meta_en: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelCatalogMetaInput {
    pub model: String,
    #[serde(default)]
    pub display_name: String,
    pub provider_logo: String,
    pub release_date: String,
    pub model_meta_zh: String,
    pub model_meta_en: String,
}

fn default_circuit_state() -> String {
    "closed".to_string()
}

fn empty_to_none(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn owned_by_from_api_type(api_type: Option<String>) -> Option<String> {
    api_type.and_then(|api_type| match api_type.as_str() {
        "openai" | "responses" | "custom" => Some("openai".to_string()),
        "claude" | "anthropic" => Some("anthropic".to_string()),
        "gemini" => Some("google".to_string()),
        "azure" => Some("openai".to_string()),
        _ => Some(api_type),
    })
}

fn row_to_entry(row: &rusqlite::Row<'_>, include_channel: bool) -> rusqlite::Result<ApiEntry> {
    let enabled: i32 = row.get(5)?;
    let response_ms: String = row.get(11).unwrap_or_default();
    let provider_logo: String = row.get(12).unwrap_or_default();
    let release_date: String = row.get(13).unwrap_or_default();
    let model_meta_zh: String = row.get(14).unwrap_or_default();
    let model_meta_en: String = row.get(15).unwrap_or_default();
    let group_name: String = row.get(16).unwrap_or_default();
    let score: f64 = row.get(17).unwrap_or(0.0);
    let channel_api_type = if include_channel {
        row.get(10).ok()
    } else {
        None
    };
    let owned_by = owned_by_from_api_type(channel_api_type.clone());

    Ok(ApiEntry {
        id: row.get(0)?,
        channel_id: row.get(1)?,
        model: row.get(2)?,
        display_name: row.get(3)?,
        sort_index: row.get(4)?,
        enabled: enabled != 0,
        cooldown_until: row.get(6).ok(),
        circuit_state: "closed".to_string(),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        channel_name: if include_channel {
            row.get(9).ok()
        } else {
            None
        },
        channel_api_type,
        owned_by,
        response_ms: empty_to_none(response_ms),
        provider_logo: empty_to_none(provider_logo),
        release_date: empty_to_none(release_date),
        model_meta_zh: empty_to_none(model_meta_zh),
        model_meta_en: empty_to_none(model_meta_en),
        group_name: empty_to_none(group_name),
        score,
    })
}

const ENTRY_SELECT_WITH_CHANNEL: &str =
    "SELECT e.id, e.channel_id, e.model, e.display_name, e.sort_index, e.enabled,
        e.cooldown_until, e.created_at, e.updated_at, c.name, c.api_type,
        e.response_ms, e.provider_logo, e.release_date, e.model_meta_zh, e.model_meta_en, e.group_name, e.score
        FROM api_entries e
        LEFT JOIN channels c ON e.channel_id = c.id";

impl Database {
    pub fn list_entries(&self) -> Result<Vec<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!("{ENTRY_SELECT_WITH_CHANNEL} ORDER BY e.sort_index, e.created_at");
        let mut stmt = conn.prepare(&sql)?;

        let entries = stmt
            .query_map([], |row| row_to_entry(row, true))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(entries)
    }

    pub fn list_entries_paginated(
        &self,
        page: i32,
        page_size: i32,
        group_name: Option<&str>,
        search: Option<&str>,
        channel_id: Option<&str>,
    ) -> Result<PaginatedResult<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let page = page.max(1);
        let page_size = page_size.max(1).min(100);
        let offset = i64::from(page.saturating_sub(1)) * i64::from(page_size);

        let mut where_clauses = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(gn) = group_name {
            where_clauses.push(format!("e.group_name = ?{}", params.len() + 1));
            params.push(Box::new(gn.to_string()));
        }
        if let Some(cid) = channel_id {
            where_clauses.push(format!("e.channel_id = ?{}", params.len() + 1));
            params.push(Box::new(cid.to_string()));
        }
        if let Some(term) = search {
            let like = format!("%{}%", term.trim());
            where_clauses.push(format!(
                "(e.display_name LIKE ?{} OR e.model LIKE ?{} OR c.name LIKE ?{})",
                params.len() + 1,
                params.len() + 2,
                params.len() + 3
            ));
            params.push(Box::new(like.clone()));
            params.push(Box::new(like.clone()));
            params.push(Box::new(like));
        }

        let where_str = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        // Count
        let count_sql = format!(
            "SELECT COUNT(*) FROM api_entries e LEFT JOIN channels c ON e.channel_id = c.id {}",
            where_str
        );
        let total: i64 = conn.query_row(&count_sql, params_from_iter(params.iter()), |row| {
            row.get(0)
        })?;

        // Query
        let query_sql = format!(
            "{} {} ORDER BY e.sort_index, e.created_at LIMIT ?{} OFFSET ?{}",
            ENTRY_SELECT_WITH_CHANNEL,
            where_str,
            params.len() + 1,
            params.len() + 2
        );
        params.push(Box::new(i64::from(page_size)));
        params.push(Box::new(offset));

        let mut stmt = conn.prepare(&query_sql)?;
        let entries = stmt
            .query_map(params_from_iter(params.iter()), |row| {
                row_to_entry(row, true)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(PaginatedResult {
            items: entries,
            total,
            page,
            page_size,
        })
    }

    pub fn create_entry(
        &self,
        channel_id: &str,
        model: &str,
        display_name: &str,
        sort_index: i32,
        provider_logo: &str,
        release_date: &str,
        model_meta_zh: &str,
        model_meta_en: &str,
        group_name: &str,
    ) -> Result<ApiEntry, AppError> {
        let conn = lock_conn!(self.conn);
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO api_entries (
            id, channel_id, model, display_name, sort_index, enabled,
            provider_logo, release_date, model_meta_zh, model_meta_en, group_name,
            created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            rusqlite::params![
                id,
                channel_id,
                model,
                display_name,
                sort_index,
                provider_logo,
                release_date,
                model_meta_zh,
                model_meta_en,
                group_name,
                now
            ],
        )?;

        Ok(ApiEntry {
            id,
            channel_id: channel_id.to_string(),
            model: model.to_string(),
            display_name: display_name.to_string(),
            sort_index,
            enabled: true,
            cooldown_until: None,
            circuit_state: "closed".to_string(),
            created_at: now,
            updated_at: now,
            channel_name: None,
            channel_api_type: None,
            owned_by: None,
            response_ms: None,
            provider_logo: empty_to_none(provider_logo.to_string()),
            release_date: empty_to_none(release_date.to_string()),
            model_meta_zh: empty_to_none(model_meta_zh.to_string()),
            model_meta_en: empty_to_none(model_meta_en.to_string()),
            group_name: empty_to_none(group_name.to_string()),
            score: 0.0,
        })
    }

    pub fn create_entry_auto(
        &self,
        channel_id: &str,
        model: &str,
        display_name: &str,
        provider_logo: &str,
        release_date: &str,
        model_meta_zh: &str,
        model_meta_en: &str,
        group_name: &str,
    ) -> Result<ApiEntry, AppError> {
        if let Some(existing) = self.find_entry_by_channel_and_model(channel_id, model)? {
            return Ok(existing);
        }

        let conn = lock_conn!(self.conn);
        let next_sort: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_index), -1) + 1 FROM api_entries",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        drop(conn);

        self.create_entry(
            channel_id,
            model,
            display_name,
            next_sort,
            provider_logo,
            release_date,
            model_meta_zh,
            model_meta_en,
            group_name,
        )
    }

    pub fn toggle_entry(&self, id: &str, enabled: bool) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE api_entries SET enabled=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![enabled as i32, now, id],
        )?;
        Ok(())
    }

    pub fn set_entry_cooldown(
        &self,
        id: &str,
        cooldown_until: Option<i64>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE api_entries SET cooldown_until=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![cooldown_until, now, id],
        )?;
        Ok(())
    }

    /// Set a single entry's sort_index to 0 and shift others down to keep relative order.
    pub fn set_entry_priority(&self, entry_id: &str, sort_index: i32) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_entries SET sort_index = sort_index + 1, updated_at = (SELECT strftime('%s','now')) WHERE id != ?1",
            rusqlite::params![entry_id],
        )?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE api_entries SET sort_index = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![sort_index, now, entry_id],
        )?;
        Ok(())
    }

    pub fn reorder_entries(&self, ordered_ids: &[String]) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        for (i, id) in ordered_ids.iter().enumerate() {
            conn.execute(
                "UPDATE api_entries SET sort_index=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![i as i32, now, id],
            )?;
        }
        Ok(())
    }

    pub fn delete_entries_by_channel(&self, channel_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM api_entries WHERE channel_id = ?1",
            [channel_id],
        )?;
        Ok(())
    }

    pub fn delete_entry(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM api_entries WHERE id = ?1", [id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_entry_response_ms(&self, id: &str, response_ms: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_entries SET response_ms = ?1 WHERE id = ?2",
            rusqlite::params![response_ms, id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_entry_score(&self, id: &str, score: f64) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_entries SET score = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![score, chrono::Utc::now().timestamp(), id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_entry_sort_index(&self, id: &str, sort_index: i32) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_entries SET sort_index = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![sort_index, chrono::Utc::now().timestamp(), id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn backfill_entry_catalog_meta(
        &self,
        items: &[EntryCatalogMetaInput],
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().timestamp();
        {
            let mut stmt = tx.prepare(
                "UPDATE api_entries
                 SET display_name = ?1, provider_logo = ?2, release_date = ?3,
                     model_meta_zh = ?4, model_meta_en = ?5, updated_at = ?6
                 WHERE id = ?7",
            )?;
            for item in items {
                stmt.execute(rusqlite::params![
                    item.display_name,
                    item.provider_logo,
                    item.release_date,
                    item.model_meta_zh,
                    item.model_meta_en,
                    now,
                    item.id,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn disable_entries_for_channel(&self, channel_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM api_entries WHERE channel_id = ?1",
            [channel_id],
        )?;
        Ok(())
    }

    /// Freeze all model entries under the same channel; only writes cooldown_until without modifying user enabled switches.
    pub fn freeze_entries_for_channel(
        &self,
        channel_id: &str,
        cooldown_until: i64,
    ) -> Result<Vec<String>, AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        let ids = {
            let mut stmt = tx.prepare("SELECT id FROM api_entries WHERE channel_id = ?1")?;
            let rows = stmt.query_map([channel_id], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let now = chrono::Utc::now().timestamp();
        tx.execute(
            "UPDATE api_entries
             SET cooldown_until = ?1, updated_at = ?2
             WHERE channel_id = ?3",
            rusqlite::params![cooldown_until, now, channel_id],
        )?;
        tx.commit()?;
        Ok(ids)
    }

    pub fn find_entry_by_channel_and_model(
        &self,
        channel_id: &str,
        model: &str,
    ) -> Result<Option<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!(
            "SELECT id, channel_id, model, display_name, sort_index, enabled, cooldown_until, created_at, updated_at,
                    '' as channel_name, '' as api_type, response_ms, provider_logo, release_date, model_meta_zh, model_meta_en, group_name, score
             FROM api_entries WHERE channel_id = ?1 AND model = ?2"
        );
        let mut stmt = conn.prepare(&sql)?;

        let result = stmt.query_row(rusqlite::params![channel_id, model], |row| {
            row_to_entry(row, false)
        });

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Sync api_entries with selected_models of a channel:
    /// - Add entries for newly selected models
    /// - Remove entries for unselected models
    /// - Refresh catalog metadata for selected models so UI/API/AUTO sorting stay in sync
    pub fn sync_entries_for_channel_with_meta(
        &self,
        channel_id: &str,
        selected_models: &[String],
        catalog_meta: &[ModelCatalogMetaInput],
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn.prepare("SELECT model FROM api_entries WHERE channel_id = ?1")?;
        let current_models: Vec<String> = stmt
            .query_map([channel_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        let now = chrono::Utc::now().timestamp();
        let max_sort: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_index), -1) FROM api_entries",
                [],
                |row| row.get(0),
            )
            .unwrap_or(-1);
        let mut next_sort = max_sort + 1;

        for model in selected_models {
            let meta = catalog_meta.iter().find(|item| item.model == *model);
            if !current_models.contains(model) {
                let id = uuid::Uuid::new_v4().to_string();
                let alias = meta
                    .map(|m| m.display_name.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(model);
                conn.execute(
                    "INSERT INTO api_entries (
                    id, channel_id, model, display_name, sort_index, enabled,
                    provider_logo, release_date, model_meta_zh, model_meta_en, group_name,
                    created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?10, ?4, 1, ?5, ?6, ?7, ?8, 'auto', ?9, ?9)",
                    rusqlite::params![
                        id,
                        channel_id,
                        model,
                        next_sort,
                        meta.map(|m| m.provider_logo.as_str()).unwrap_or(""),
                        meta.map(|m| m.release_date.as_str()).unwrap_or(""),
                        meta.map(|m| m.model_meta_zh.as_str()).unwrap_or(""),
                        meta.map(|m| m.model_meta_en.as_str()).unwrap_or(""),
                        now,
                        alias,
                    ],
                )?;
                next_sort += 1;
            } else if let Some(meta) = meta {
                let alias = if meta.display_name.is_empty() {
                    model.as_str()
                } else {
                    &meta.display_name
                };
                conn.execute(
                    "UPDATE api_entries
                     SET display_name = ?1, provider_logo = ?2, release_date = ?3,
                         model_meta_zh = ?4, model_meta_en = ?5, updated_at = ?6
                     WHERE channel_id = ?7 AND model = ?8",
                    rusqlite::params![
                        alias,
                        meta.provider_logo,
                        meta.release_date,
                        meta.model_meta_zh,
                        meta.model_meta_en,
                        now,
                        channel_id,
                        model,
                    ],
                )?;
            }
        }

        for model in &current_models {
            if !selected_models.contains(model) {
                conn.execute(
                    "DELETE FROM api_entries WHERE channel_id = ?1 AND model = ?2",
                    rusqlite::params![channel_id, model],
                )?;
            }
        }

        Ok(())
    }

    /// Get all API pool entries for listing models / direct model routing.
    /// Includes disabled entries; entry.enabled only means "enter AUTO", not "usable".
    pub fn get_entries_for_routing(&self) -> Result<Vec<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!("{ENTRY_SELECT_WITH_CHANNEL} ORDER BY e.sort_index, e.created_at");
        let mut stmt = conn.prepare(&sql)?;

        let entries = stmt
            .query_map([], |row| row_to_entry(row, true))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(entries)
    }

    /// Get enabled entries from enabled channels for AUTO routing.
    /// Only enabled + non-cooldown entries enter the AUTO pool.
    pub fn get_enabled_entries_for_auto(&self) -> Result<Vec<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!(
            "{ENTRY_SELECT_WITH_CHANNEL}
            WHERE e.enabled = 1 AND c.enabled = 1
            AND (e.cooldown_until IS NULL OR e.cooldown_until <= strftime('%s','now'))
            ORDER BY e.sort_index, e.created_at"
        );
        let mut stmt = conn.prepare(&sql)?;

        let entries = stmt
            .query_map([], |row| row_to_entry(row, true))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(entries)
    }

    /// Get all entries (including disabled) with channel info. Used for test chat.
    pub fn get_entries_for_routing_all(&self) -> Result<Vec<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!("{ENTRY_SELECT_WITH_CHANNEL} ORDER BY e.sort_index, e.created_at");
        let mut stmt = conn.prepare(&sql)?;

        let entries = stmt
            .query_map([], |row| row_to_entry(row, true))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(entries)
    }

    /// Get all distinct group names from api_entries.
    pub fn get_all_group_names(&self) -> Result<Vec<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare("SELECT DISTINCT group_name FROM api_entries WHERE group_name != '' AND group_name IS NOT NULL ORDER BY group_name")?;

        let groups = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(groups)
    }

    /// Get enabled entries for a specific group (for tray menu).
    /// Only enabled + non-cooldown entries enter the pool, filtered by group_name.
    pub fn get_enabled_entries_for_group(
        &self,
        group_name: &str,
    ) -> Result<Vec<ApiEntry>, AppError> {
        let conn = lock_conn!(self.conn);
        let sql = format!(
            "{ENTRY_SELECT_WITH_CHANNEL}
        WHERE e.enabled = 1 AND c.enabled = 1
        AND e.group_name = ?1
        AND (e.cooldown_until IS NULL OR e.cooldown_until <= strftime('%s','now'))
        ORDER BY e.sort_index, e.created_at"
        );
        let mut stmt = conn.prepare(&sql)?;

        let entries = stmt
            .query_map([group_name], |row| row_to_entry(row, true))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(entries)
    }

    /// Update the group_name for a specific entry.
    pub fn update_entry_group(&self, id: &str, group_name: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE api_entries SET group_name = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![group_name, now, id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_entry_display_name(&self, id: &str, display_name: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE api_entries SET display_name = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![display_name, now, id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
}
