//! Common utilities for SSE stream processing
//!
//! Consolidates SSE stream processing logic in one place:
//! - UTF-8 cross-chunk boundary safe concatenation (`append_utf8_safe`)
//! - SSE `data:` prefix parsing (`sse_data_payload`)
//! - Unified stream buffer limit (`MAX_STREAM_BUFFER_BYTES`)
//!
//! Consolidates common SSE processing logic, eliminating character splitting across chunks.
//!
//! Core axiom: as a proxy and translator, byte streams must not be corrupted during transit.

/// Unified stream buffer maximum capacity (10MB).
pub const MAX_STREAM_BUFFER_BYTES: usize = 10 * 1024 * 1024;

/// Check whether current buffer state exceeds unified limit.
pub fn stream_buffer_exceeded(buffer: &str, remainder: &[u8], streamed_bytes: usize) -> bool {
    buffer.len() + remainder.len() + streamed_bytes > MAX_STREAM_BUFFER_BYTES
}

/// Append `bytes` to `buffer` (UTF-8 string), ensuring multi-byte characters are not corrupted.
///
/// Principles:
/// - `remainder` stores incomplete UTF-8 bytes from previous call
/// - When new bytes arrive, append to remainder
/// - Attempt to interpret remainder as UTF-8; push valid prefix into buffer, keep remaining bytes
/// - If remaining bytes cannot form valid UTF-8, perform lossy fallback
///
/// This ensures multi-byte characters across chunks are properly restored without replacement characters.
///
/// # Example
///
/// ```ignore
/// let text = "€€";                    // E2 82 AC E2 82 AC
/// let bytes = text.as_bytes();
/// let mut buffer = String::new();
/// let mut remainder = Vec::new();
///
/// // TCP slices across multi-byte character boundary
/// append_utf8_safe(&mut buffer, &mut remainder, &bytes[..4]);
/// assert_eq!(buffer, "€");              // trailing byte stays in remainder
///
/// // Second chunk delivers remaining bytes
/// append_utf8_safe(&mut buffer, &mut remainder, &bytes[4..]);
/// assert_eq!(buffer, "€€");            // completely restored
/// assert!(remainder.is_empty());
/// ```
pub fn append_utf8_safe(buffer: &mut String, remainder: &mut Vec<u8>, bytes: &[u8]) {
    remainder.extend_from_slice(bytes);

    match std::str::from_utf8(remainder) {
        Ok(valid) => {
            buffer.push_str(valid);
            remainder.clear();
        }
        Err(err) => {
            let valid_up_to = err.valid_up_to();
            if valid_up_to > 0 {
                // valid_up_to guaranteed by Utf8Error to point to valid UTF-8 boundary
                let valid = std::str::from_utf8(&remainder[..valid_up_to])
                    .expect("valid UTF-8 prefix guaranteed by valid_up_to contract");
                buffer.push_str(valid);
                remainder.drain(..valid_up_to);
            }

            // error_len().is_some() indicates an explicit invalid sequence (not a simple truncation)
            // In this case lossy fallback is used, though rare in valid upstream streams
            if err.error_len().is_some() && !remainder.is_empty() {
                buffer.push_str(&String::from_utf8_lossy(remainder));
                remainder.clear();
            }
        }
    }
}

/// Parse SSE `data:` line, returning payload (without "data:" prefix and optional spaces).
///
/// - `"data: {json}"` -> `Some("{json}")`
/// - `"data:{json}"` -> `Some("{json}")` (compact format)
/// - `"event: message"` -> `None` (non-data line)
/// - Empty lines / others -> `None`
pub fn sse_data_payload(line: &str) -> Option<&str> {
    let payload = line.strip_prefix("data:")?;
    Some(payload.strip_prefix(' ').unwrap_or(payload))
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_utf8_safe_basic_ascii() {
        let mut buffer = String::new();
        let mut remainder = Vec::new();
        append_utf8_safe(&mut buffer, &mut remainder, b"hello");
        assert_eq!(buffer, "hello");
        assert!(remainder.is_empty());
    }

    #[test]
    fn append_utf8_safe_preserves_split_chinese() {
        let text = "€€";
        let bytes = text.as_bytes();
        let mut buffer = String::new();
        let mut remainder = Vec::new();

        // Slice at non-boundary position
        let split_at = bytes.iter().position(|b| *b >= 0x80).unwrap() + 1;

        append_utf8_safe(&mut buffer, &mut remainder, &bytes[..split_at]);
        assert!(
            !buffer.contains('\u{FFFD}'),
            "Should not introduce replacement characters in first chunk"
        );
        assert!(!remainder.is_empty(), "Incomplete bytes should remain in remainder");

        append_utf8_safe(&mut buffer, &mut remainder, &bytes[split_at..]);
        assert_eq!(buffer, text);
        assert!(remainder.is_empty());
    }

    #[test]
    fn append_utf8_safe_preserves_split_emoji() {
        // emoji is 4-byte UTF-8, test splitting across various positions
        let text = "🎉abc";
        let bytes = text.as_bytes();
        let mut buffer = String::new();
        let mut remainder = Vec::new();

        // Split after 2nd byte of emoji
        append_utf8_safe(&mut buffer, &mut remainder, &bytes[..2]);
        assert!(!buffer.contains('\u{FFFD}'));

        append_utf8_safe(&mut buffer, &mut remainder, &bytes[2..]);
        assert_eq!(buffer, text);
    }

    #[test]
    fn append_utf8_safe_handles_many_small_chunks() {
        // Simulate extreme case: 1 byte per chunk
        let text = "Hello € World 🎉";
        let bytes = text.as_bytes();
        let mut buffer = String::new();
        let mut remainder = Vec::new();

        for byte in bytes {
            append_utf8_safe(&mut buffer, &mut remainder, &[*byte]);
        }

        assert_eq!(buffer, text);
        assert!(remainder.is_empty());
    }

    #[test]
    fn sse_data_payload_with_space() {
        assert_eq!(
            sse_data_payload("data: {\"ok\":true}"),
            Some("{\"ok\":true}")
        );
    }

    #[test]
    fn sse_data_payload_without_space() {
        assert_eq!(
            sse_data_payload("data:{\"ok\":true}"),
            Some("{\"ok\":true}")
        );
    }

    #[test]
    fn sse_data_payload_rejects_other_lines() {
        assert_eq!(sse_data_payload("event: message"), None);
        assert_eq!(sse_data_payload(""), None);
        assert_eq!(sse_data_payload(": comment"), None);
        assert_eq!(sse_data_payload("id: 1"), None);
    }

    #[test]
    fn sse_data_payload_done_marker() {
        assert_eq!(sse_data_payload("data: [DONE]"), Some("[DONE]"));
    }
}
