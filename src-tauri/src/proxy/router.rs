use super::circuit_breaker::CircuitBreaker;
use crate::database::ApiEntry;
use chrono::NaiveDate;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Parse response_ms field to milliseconds.
/// Supports raw milliseconds ("1234") and legacy display values ("1.2s" / "350ms").
/// Returns None if missing or unparseable.
fn parse_response_ms(entry: &ApiEntry) -> Option<i64> {
    let value = entry.response_ms.as_deref()?.trim().to_ascii_lowercase();
    if value.is_empty() || value == "x" {
        return None;
    }
    if let Some(milliseconds) = value.strip_suffix("ms") {
        return milliseconds.parse::<f64>().ok().map(|ms| ms.round() as i64);
    }
    if let Some(seconds) = value.strip_suffix('s') {
        return seconds
            .parse::<f64>()
            .ok()
            .map(|s| (s * 1000.0).round() as i64);
    }
    value.parse::<f64>().ok().map(|ms| ms.round() as i64)
}

/// Sort entries by response time ascending; entries without measurement go last.
fn sort_by_latency(entries: &mut [ApiEntry]) {
    entries.sort_by_key(|e| parse_response_ms(e).unwrap_or(i64::MAX));
}

/// Sort entries by sort_index ascending (user's custom order).
fn sort_by_index(entries: &mut [ApiEntry]) {
    entries.sort_by_key(|e| e.sort_index);
}

fn parse_release_date(entry: &ApiEntry) -> Option<NaiveDate> {
    let value = entry.release_date.as_deref()?.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Some(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d") {
        return Some(date);
    }
    NaiveDate::parse_from_str(value, "%Y%m%d").ok()
}

/// Sort entries by release date descending; entries without release date go last.
fn sort_by_release_date(entries: &mut [ApiEntry]) {
    entries.sort_by(|a, b| {
        let date_cmp = parse_release_date(b).cmp(&parse_release_date(a));
        if date_cmp == std::cmp::Ordering::Equal {
            a.sort_index.cmp(&b.sort_index)
        } else {
            date_cmp
        }
    });
}

fn is_not_long_term_frozen(entry: &ApiEntry) -> bool {
    let now = chrono::Utc::now().timestamp();
    entry
        .cooldown_until
        .map(|until| {
            // A long-term freeze (e.g. 6h freeze) is > 1 hour into the future.
            // Temporary circuit breaker cooldowns are managed by CircuitBreaker::is_available().
            until <= now || (until - now) <= 1800
        })
        .unwrap_or(true)
}

fn normalize_model(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        "auto".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn available_entries(
    entries: &[ApiEntry],
    breakers: &HashMap<String, CircuitBreaker>,
) -> Vec<ApiEntry> {
    let mut available: Vec<ApiEntry> = entries
        .iter()
        .filter(|e| e.enabled && is_not_long_term_frozen(e))
        .filter(|e| {
            if let Some(cb) = breakers.get(&e.id) {
                cb.is_available()
            } else {
                true
            }
        })
        .cloned()
        .collect();
    sort_by_index(&mut available);
    available
}

/// Resolve which entries to try for a given model request.
/// Returns an ordered list of entries to attempt (failover in order).
///
/// Rules (priority order):
/// 1. Trim request.model and replace empty string with `auto`.
/// 2. Case-insensitive group exact match.
/// 3. Case-insensitive model exact match.
/// 4. Case-insensitive model fuzzy match where `entry.model` contains `request.model`.
/// 5. Fallback to the AUTO group (`group_name == "auto"`).
/// 6. Return no-provider if the AUTO group is empty.
pub async fn resolve(
    model: &str,
    all_entries: &[ApiEntry],
    auto_entries: &[ApiEntry],
    circuit_breakers: &RwLock<HashMap<String, CircuitBreaker>>,
) -> Vec<ApiEntry> {
    let normalized_model = normalize_model(model);
    let breakers = circuit_breakers.read().await;
    let all_available = available_entries(all_entries, &breakers);

    let group_matches: Vec<ApiEntry> = all_available
        .iter()
        .filter(|entry| {
            entry
                .group_name
                .as_deref()
                .map(|group| group.eq_ignore_ascii_case(&normalized_model))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    if !group_matches.is_empty() {
        return group_matches;
    }

    let normalized_model_lower = normalized_model.to_ascii_lowercase();

    // 2.5 Exact model name match (case-insensitive)
    let exact_model_matches: Vec<ApiEntry> = all_available
        .iter()
        .filter(|entry| entry.model.to_ascii_lowercase() == normalized_model_lower)
        .cloned()
        .collect();
    if !exact_model_matches.is_empty() {
        return exact_model_matches;
    }

    // 2.7 Exact alias (display_name) match (case-insensitive)
    let alias_matches: Vec<ApiEntry> = all_available
        .iter()
        .filter(|entry| {
            !entry.display_name.trim().is_empty()
                && entry.display_name.to_ascii_lowercase() == normalized_model_lower
        })
        .cloned()
        .collect();
    if !alias_matches.is_empty() {
        return alias_matches;
    }

    let model_matches: Vec<ApiEntry> = all_available
        .iter()
        .filter(|entry| {
            entry
                .model
                .to_ascii_lowercase()
                .contains(&normalized_model_lower)
        })
        .cloned()
        .collect();
    if !model_matches.is_empty() {
        return model_matches;
    }

    let auto_available = available_entries(auto_entries, &breakers);
    auto_available
        .into_iter()
        .filter(|entry| {
            entry
                .group_name
                .as_deref()
                .map(|group| group.eq_ignore_ascii_case("auto"))
                .unwrap_or(false)
        })
        .collect()
}

/// Apply sort mode to entries: "custom" → sort_index, "fastest" → latency, "latest" → release_date.
pub(crate) fn apply_sort_mode(entries: &mut [ApiEntry], sort_mode: &str) {
    match sort_mode {
        "fastest" => sort_by_latency(entries),
        "latest" => sort_by_release_date(entries),
        _ => sort_by_index(entries),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, model: &str, enabled: bool, sort_index: i32) -> ApiEntry {
        ApiEntry {
            id: id.to_string(),
            channel_id: format!("channel-{id}"),
            model: model.to_string(),
            display_name: model.to_string(),
            sort_index,
            enabled,
            cooldown_until: None,
            circuit_state: "closed".to_string(),
            created_at: 0,
            updated_at: 0,
            channel_name: Some(format!("channel-{id}")),
            channel_api_type: Some("openai".to_string()),
            response_ms: None,
            owned_by: None,
            provider_logo: None,
            release_date: None,
            model_meta_zh: None,
            model_meta_en: None,
            group_name: None,
            score: 0.0,
        }
    }

    fn entry_with_group(
        id: &str,
        model: &str,
        enabled: bool,
        sort_index: i32,
        group: &str,
    ) -> ApiEntry {
        let mut e = entry(id, model, enabled, sort_index);
        e.group_name = Some(group.to_string());
        e
    }

    #[tokio::test]
    async fn empty_model_normalizes_to_auto_group() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            entry_with_group("auto-first", "gpt-4o", true, 0, "auto"),
            entry_with_group("coding", "claude-3", true, 1, "coding"),
        ];
        let auto = all.clone();

        let resolved = resolve("   ", &all, &auto, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["auto-first"]
        );
    }

    #[tokio::test]
    async fn group_exact_match_is_case_insensitive() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            entry_with_group("match1", "gpt-4o", true, 0, "Coding"),
            entry_with_group("match2", "claude-3", true, 1, "coding"),
            entry_with_group("other", "gemini-pro", true, 2, "other"),
        ];

        let resolved = resolve("CoDiNg", &all, &all, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["match1", "match2"]
        );
    }

    #[tokio::test]
    async fn group_match_takes_priority_over_fuzzy_model_match() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            entry_with_group("group-match", "unrelated-model", true, 0, "coding"),
            entry_with_group("fuzzy-match", "prefix-coding-suffix", true, 1, "other"),
        ];

        let resolved = resolve("coding", &all, &all, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["group-match"]
        );
    }

    #[tokio::test]
    async fn exact_model_match_takes_priority_over_fuzzy_model_match() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            // exact match (lower priority)
            entry_with_group("fuzzy-first", "gpt-4o-plus", true, 0, "other"),
            // exact match should win even with higher sort_index
            entry_with_group("exact-match", "gpt-4o", true, 5, "other"),
            // fuzzy match (higher priority)
            entry_with_group("fuzzy-second", "gpt-4o-mini", true, 1, "other"),
        ];

        let resolved = resolve("gpt-4o", &all, &all, &breakers).await;

        // Only exact match should be returned, fuzzy excluded
        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["exact-match"]
        );
    }

    #[tokio::test]
    async fn model_fuzzy_match_is_case_insensitive() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            entry_with_group("match1", "[aa]GPT-4O-Mini", true, 0, "other"),
            entry_with_group("match2", "vendor/gpt-4o-mini", true, 1, "other"),
            entry_with_group("other", "claude-3", true, 2, "other"),
        ];

        let resolved = resolve("gPt-4O", &all, &all, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["match1", "match2"]
        );
    }

    #[tokio::test]
    async fn falls_back_to_auto_group_when_no_group_or_model_match() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            entry_with_group("auto-first", "gpt-4o", true, 0, "AUTO"),
            entry_with_group("auto-second", "claude-3", true, 1, "auto"),
            entry_with_group("other", "gemini-pro", true, 2, "other"),
        ];
        let auto = all.clone();

        let resolved = resolve("missing-model", &all, &auto, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["auto-first", "auto-second"]
        );
    }

    #[tokio::test]
    async fn returns_no_provider_when_auto_group_is_empty() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![entry_with_group("other", "claude-3", true, 0, "other")];
        let auto = all.clone();

        let resolved = resolve("missing-model", &all, &auto, &breakers).await;

        assert!(resolved.is_empty());
    }

    #[tokio::test]
    async fn fallback_auto_group_skips_circuit_open_entries() {
        let breakers = RwLock::new(HashMap::new());
        let all = vec![
            entry_with_group("open-auto", "gpt-4o", true, 0, "auto"),
            entry_with_group("healthy-auto", "claude-3", true, 1, "auto"),
        ];
        let auto = all.clone();
        {
            let mut guard = breakers.write().await;
            let cb = CircuitBreaker::new(60);
            cb.record_failure(1);
            guard.insert("open-auto".to_string(), cb);
        }

        let resolved = resolve("missing-model", &all, &auto, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["healthy-auto"]
        );
    }

    #[tokio::test]
    async fn fallback_auto_group_skips_cooled_down_entries() {
        let breakers = RwLock::new(HashMap::new());
        let mut cooled = entry_with_group("cooled-auto", "gpt-4o", true, 0, "auto");
        cooled.cooldown_until = Some(chrono::Utc::now().timestamp() + 60);
        let healthy = entry_with_group("healthy-auto", "claude-3", true, 1, "auto");
        let all = vec![cooled, healthy.clone()];
        let auto = all.clone();

        let resolved = resolve("missing-model", &all, &auto, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec![healthy.id.as_str()]
        );
    }

    #[tokio::test]
    async fn latest_sort_uses_release_date_descending() {
        let mut older = entry("older", "old-model", true, 0);
        older.release_date = Some("2023-01-15".to_string());
        let mut newer = entry("newer", "new-model", true, 1);
        newer.release_date = Some("2024-08".to_string());
        let missing = entry("missing", "unknown-model", true, 2);
        let mut newest = entry("newest", "newest-model", true, 3);
        newest.release_date = Some("20240902".to_string());
        let mut entries = vec![older, missing, newest, newer];

        apply_sort_mode(&mut entries, "latest");

        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["newest", "newer", "older", "missing"]
        );
    }

    #[tokio::test]
    async fn fastest_sort_uses_response_ms_ascending_with_legacy_units() {
        let mut slow = entry("slow", "slow-model", true, 0);
        slow.response_ms = Some("1.2s".to_string());
        let mut fast = entry("fast", "fast-model", true, 1);
        fast.response_ms = Some("350ms".to_string());
        let missing = entry("missing", "unknown-model", true, 2);
        let mut entries = vec![slow, missing, fast];

        apply_sort_mode(&mut entries, "fastest");

        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["fast", "slow", "missing"]
        );
    }

    #[tokio::test]
    async fn custom_auto_uses_sort_index_order() {
        let breakers = RwLock::new(HashMap::new());
        let enabled = vec![
            entry_with_group("third", "third-model", true, 2, "auto"),
            entry_with_group("first", "first-model", true, 0, "auto"),
            entry_with_group("second", "second-model", true, 1, "auto"),
        ];

        let resolved = resolve("auto", &enabled, &enabled, &breakers).await;

        assert_eq!(
            resolved.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    }
}
