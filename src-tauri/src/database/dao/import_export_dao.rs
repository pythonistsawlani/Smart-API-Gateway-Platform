use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::services::import_export_service::ChannelModelTransfer;
use uuid::Uuid;

impl Database {
    pub fn replace_channels_and_models_from_transfer(
        &self,
        transfer: &ChannelModelTransfer,
    ) -> Result<(usize, usize), AppError> {
        let mut conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().timestamp();

        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute("DELETE FROM api_entries", [])?;
        tx.execute("DELETE FROM channels", [])?;

        let mut channel_count = 0;
        let mut model_count = 0;

        for channel in &transfer.data.channels {
            let channel_id = Uuid::new_v4().to_string();
            let available_models = serde_json::to_string(&channel.available_models)
                .map_err(|e| AppError::Internal(format!("Failed to serialize channel available models: {e}")))?;
            let selected_models = serde_json::to_string(&channel.selected_models)
                .map_err(|e| AppError::Internal(format!("Failed to serialize channel selected models: {e}")))?;

            tx.execute(
                "INSERT INTO channels (
                    id, name, api_type, base_url, api_key, available_models, selected_models,
                    enabled, last_fetch_at, notes, upstream_headers, response_ms, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, '', ?11, ?11)",
                rusqlite::params![
                    channel_id,
                    channel.name.trim(),
                    channel.api_type.trim(),
                    channel.base_url.trim(),
                    channel.api_key,
                    available_models,
                    selected_models,
                    if channel.enabled { 1 } else { 0 },
                    channel.notes,
                    channel.upstream_headers,
                    now,
                ],
            )?;
            channel_count += 1;

            for model in &channel.models {
                let entry_id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO api_entries (
                        id, channel_id, model, display_name, sort_index, enabled, cooldown_until,
                        response_ms, provider_logo, release_date, model_meta_zh, model_meta_en,
                        group_name, score, created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                    rusqlite::params![
                        entry_id,
                        channel_id,
                        model.model.trim(),
                        model.display_name,
                        model.sort_index,
                        if model.enabled { 1 } else { 0 },
                        model.provider_logo,
                        model.release_date,
                        model.model_meta_zh,
                        model.model_meta_en,
                        model.group_name.trim(),
                        model.score,
                        now,
                    ],
                )?;
                model_count += 1;
            }
        }

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;

        Ok((channel_count, model_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{ApiEntry, Channel, ModelInfo};
    use crate::services::import_export_service::{build_transfer_json, validate_transfer_payload};
    use rusqlite::Connection;
    use std::sync::Mutex;

    fn test_db() -> Database {
        let db = Database {
            conn: Mutex::new(Connection::open_in_memory().unwrap()),
        };
        db.create_tables().unwrap();
        db
    }

    #[test]
    fn replace_channels_and_models_clears_old_data_and_rebuilds_imported_data() {
        let db = test_db();
        let old_channel = db
            .create_channel(
                "Old Channel",
                "openai",
                "https://old.example.com",
                "old-key",
                None,
                None,
            )
            .unwrap();
        db.create_entry(
            &old_channel.id,
            "old-model",
            "Old Model",
            0,
            "",
            "",
            "",
            "",
            "auto",
        )
        .unwrap();

        let source_channel = Channel {
            id: "source-channel-id".to_string(),
            name: "New Channel".to_string(),
            api_type: "openai".to_string(),
            base_url: "https://new.example.com".to_string(),
            api_key: "new-key".to_string(),
            available_models: vec![ModelInfo {
                id: "new-model".to_string(),
                name: "new-model".to_string(),
                owned_by: Some("openai".to_string()),
            }],
            selected_models: vec!["new-model".to_string()],
            enabled: true,
            last_fetch_at: 999,
            notes: "Migrated Channel".to_string(),
            upstream_headers: Some(r#"{"x-custom":"abc"}"#.to_string()),
            response_ms: "999".to_string(),
            created_at: 1,
            updated_at: 2,
        };
        let source_entry = ApiEntry {
            id: "source-entry-id".to_string(),
            channel_id: "source-channel-id".to_string(),
            model: "new-model".to_string(),
            display_name: "New Model".to_string(),
            sort_index: 3,
            enabled: false,
            cooldown_until: Some(888),
            circuit_state: "open".to_string(),
            created_at: 3,
            updated_at: 4,
            channel_name: Some("New Channel".to_string()),
            channel_api_type: Some("openai".to_string()),
            owned_by: Some("openai".to_string()),
            response_ms: Some("777".to_string()),
            provider_logo: Some("logo".to_string()),
            release_date: Some("2025-01-01".to_string()),
            model_meta_zh: Some("English".to_string()),
            model_meta_en: Some("English".to_string()),
            group_name: Some("coding".to_string()),
            score: 9.5,
        };
        let json = build_transfer_json(&[source_channel], &[source_entry]).unwrap();
        let transfer = validate_transfer_payload(&json).unwrap();

        let (channel_count, model_count) = db
            .replace_channels_and_models_from_transfer(&transfer)
            .unwrap();

        assert_eq!(channel_count, 1);
        assert_eq!(model_count, 1);
        let channels = db.list_channels().unwrap();
        let entries = db.list_entries().unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(channels[0].name, "New Channel");
        assert_eq!(channels[0].api_key, "new-key");
        assert_eq!(channels[0].last_fetch_at, 0);
        assert_eq!(channels[0].response_ms, "");
        assert_ne!(channels[0].id, "source-channel-id");
        assert_eq!(entries[0].model, "new-model");
        assert_eq!(entries[0].display_name, "New Model");
        assert_eq!(entries[0].sort_index, 3);
        assert!(!entries[0].enabled);
        assert_eq!(entries[0].group_name.as_deref(), Some("coding"));
        assert_eq!(entries[0].response_ms, None);
        assert_eq!(entries[0].cooldown_until, None);
        assert_ne!(entries[0].id, "source-entry-id");
        assert_eq!(entries[0].channel_id, channels[0].id);
        assert_eq!(
            channels[0].upstream_headers.as_deref(),
            Some(r#"{"x-custom":"abc"}"#)
        );
    }

    #[test]
    fn replace_channels_and_models_rolls_back_when_insert_fails() {
        let db = test_db();
        let old_channel = db
            .create_channel(
                "Old Channel",
                "openai",
                "https://old.example.com",
                "old-key-12345",
                None,
                None,
            )
            .unwrap();
        db.create_entry(
            &old_channel.id,
            "old-model",
            "Old Model",
            0,
            "",
            "",
            "",
            "",
            "auto",
        )
        .unwrap();
        let before_channels = db.list_channels().unwrap();
        let before_entries = db.list_entries().unwrap();

        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "CREATE TRIGGER fail_import_channel_insert
                 BEFORE INSERT ON channels
                 FOR EACH ROW
                 BEGIN
                   SELECT RAISE(ABORT, 'Simulated import insertion failure');
                 END;",
                [],
            )
            .unwrap();
        }

        let source_channel = Channel {
            id: "source-channel-id".to_string(),
            name: "New Channel".to_string(),
            api_type: "openai".to_string(),
            base_url: "https://new.example.com".to_string(),
            api_key: "new-key".to_string(),
            available_models: vec![],
            selected_models: vec![],
            enabled: true,
            last_fetch_at: 999,
            notes: "Migrated Channel".to_string(),
            upstream_headers: None,
            response_ms: "999".to_string(),
            created_at: 1,
            updated_at: 2,
        };
        let source_entry = ApiEntry {
            id: "source-entry-id".to_string(),
            channel_id: "source-channel-id".to_string(),
            model: "new-model".to_string(),
            display_name: "New Model".to_string(),
            sort_index: 3,
            enabled: true,
            cooldown_until: None,
            circuit_state: "closed".to_string(),
            created_at: 3,
            updated_at: 4,
            channel_name: Some("New Channel".to_string()),
            channel_api_type: Some("openai".to_string()),
            owned_by: Some("openai".to_string()),
            response_ms: Some("777".to_string()),
            provider_logo: None,
            release_date: None,
            model_meta_zh: None,
            model_meta_en: None,
            group_name: Some("auto".to_string()),
            score: 1.0,
        };
        let json = build_transfer_json(&[source_channel], &[source_entry]).unwrap();
        let transfer = validate_transfer_payload(&json).unwrap();

        let result = db.replace_channels_and_models_from_transfer(&transfer);

        assert!(result.is_err());
        let channels = db.list_channels().unwrap();
        let entries = db.list_entries().unwrap();
        assert_eq!(channels.len(), before_channels.len());
        assert_eq!(entries.len(), before_entries.len());
        assert!(channels
            .iter()
            .any(|channel| channel.name == "Old Channel" && channel.api_key == "old-key-12345"));
        assert!(entries.iter().any(|entry| entry.model == "old-model"));
    }
}
