//! Record raw failure errors not intercepted by existing rules (status codes/keywords) for manual or AI analysis
//! to supplement disable_keywords. Status codes are recorded as well to determine which failures are retryable.
//!
//! Output file: <data_dir>/disable-keyword-candidates.txt, one `{status} {normalized_raw}` per line.
//! Deduplicated by status + normalized raw text; write is best-effort and failures only warn without affecting forwarding.

use std::collections::HashSet;

const LOG_FILE_NAME: &str = "disable-keyword-candidates.txt";

/// Normalize raw error text for deduplication and single-line storage: whitespace collapsed, trimmed, lowercased.
fn normalize_message(message: &str) -> String {
    let mut result = String::with_capacity(message.len());
    let mut last_was_space = false;
    for ch in message.chars() {
        let c = if ch.is_whitespace() { ' ' } else { ch };
        if c == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        result.push(c);
    }
    result.trim().to_lowercase()
}

/// Record an unknown failure (status code + normalized raw text). Existing identical lines are skipped.
pub async fn record_unknown_failure(status: u16, message: &str) {
    let normalized = normalize_message(message);
    if normalized.is_empty() {
        return;
    }
    let line = format!("{status} {normalized}");

    if let Err(e) = append_if_absent(&line).await {
        log::warn!("Failed to record disable-keyword candidate: {e}");
    }
}

async fn append_if_absent(line: &str) -> Result<(), String> {
    let dir = crate::data_dir::resolve_data_dir().map_err(|e| e.to_string())?;
    let path = dir.join(LOG_FILE_NAME);

    let existing = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };

    let seen: HashSet<&str> = existing.lines().collect();
    if seen.contains(line) {
        return Ok(());
    }

    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.write_all(b"\n")
        .await
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace_and_lowercases() {
        let input = "  Your   credit\n balance\tis  too LOW  ";
        assert_eq!(normalize_message(input), "your credit balance is too low");
    }

    #[test]
    fn normalize_empty_returns_empty() {
        assert_eq!(normalize_message("   \n\t  "), "");
    }
}
