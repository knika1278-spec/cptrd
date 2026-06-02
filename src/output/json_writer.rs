//! JSON output writer.
//!
//! Each decoded event is stamped with a `decoded_at` (RFC3339 UTC) timestamp
//! and appended to ONE of three JSON-array files under the output directory,
//! based on its guard flags:
//! - `is_bot_whale == true`           -> `bot_whales.json`
//! - else `passed_threshold == false` -> `skipped_events.json`
//! - else (passed)                    -> `decoded_events.json`
//!
//! Append semantics: read the existing array, push the new event, write back
//! pretty-printed. A missing/empty/malformed file is treated as an empty array.

use std::path::Path;

use anyhow::Context;

use crate::models::types::DecodedTradeEvent;

/// File names (relative to the output dir).
pub const DECODED_FILE: &str = "decoded_events.json";
pub const SKIPPED_FILE: &str = "skipped_events.json";
pub const BOT_FILE: &str = "bot_whales.json";

/// Classify an event into its target file name (bot takes precedence over threshold).
fn target_file(event: &DecodedTradeEvent) -> &'static str {
    if event.is_bot_whale {
        BOT_FILE
    } else if !event.passed_threshold {
        SKIPPED_FILE
    } else {
        DECODED_FILE
    }
}

/// Stamp the event with `decoded_at` (RFC3339 UTC) and append it to the
/// appropriate file under `output_dir`. Creates the file (as a `[]` array) if
/// missing, and creates `output_dir` if missing.
pub fn write_event(event: &DecodedTradeEvent, output_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create output dir {}", output_dir.display()))?;

    let mut stamped = event.clone();
    stamped.decoded_at = Some(chrono::Utc::now().to_rfc3339());

    let file_name = target_file(&stamped);
    let path = output_dir.join(file_name);

    append_event(&path, &stamped)?;

    tracing::info!(
        signature = %stamped.signature,
        file = file_name,
        "wrote decoded event to output file"
    );

    Ok(())
}

/// Read the existing array (recovering from missing/empty/malformed files),
/// push the new event, and write the whole array back pretty-printed.
fn append_event(path: &Path, event: &DecodedTradeEvent) -> anyhow::Result<()> {
    let mut events: Vec<DecodedTradeEvent> = match std::fs::read_to_string(path) {
        Ok(contents) if contents.trim().is_empty() => Vec::new(),
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|err| {
            tracing::warn!(
                file = %path.display(),
                error = %err,
                "existing output file is malformed; treating as empty array"
            );
            Vec::new()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    events.push(event.clone());

    let serialized =
        serde_json::to_string_pretty(&events).context("failed to serialize event array")?;

    std::fs::write(path, serialized)
        .with_context(|| format!("failed to write {}", path.display()))?;

    tracing::debug!(file = %path.display(), count = events.len(), "appended event");

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::{DexProtocol, TradeAction};
    use std::path::Path;

    fn sample_event(is_bot_whale: bool, passed_threshold: bool) -> DecodedTradeEvent {
        DecodedTradeEvent {
            signature: "5xSig".to_string(),
            slot: 123_456,
            timestamp: Some(1_717_000_000),
            whale_address: "WhaLe11111111111111111111111111111111111111".to_string(),
            dex: DexProtocol::PumpFun,
            action: TradeAction::Buy,
            mint: "Mint1111111111111111111111111111111111111111".to_string(),
            sol_amount: 3.5,
            token_amount: 1_000_000,
            token_decimals: 6,
            is_bot_whale,
            passed_threshold,
            guard_skip_reason: None,
            decoded_at: None,
        }
    }

    fn read_events(path: &Path) -> Vec<DecodedTradeEvent> {
        let contents = std::fs::read_to_string(path).expect("read file");
        serde_json::from_str(&contents).expect("parse array")
    }

    #[test]
    fn passed_event_goes_to_decoded_file_with_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let event = sample_event(false, true);

        write_event(&event, dir.path()).expect("write event");

        let decoded = read_events(&dir.path().join(DECODED_FILE));
        assert_eq!(decoded.len(), 1);
        assert!(decoded[0].decoded_at.is_some());
        assert!(!decoded[0].decoded_at.as_ref().unwrap().is_empty());

        assert!(!dir.path().join(SKIPPED_FILE).exists());
        assert!(!dir.path().join(BOT_FILE).exists());
    }

    #[test]
    fn skipped_event_goes_to_skipped_file() {
        let dir = tempfile::tempdir().unwrap();
        let event = sample_event(false, false);

        write_event(&event, dir.path()).expect("write event");

        let skipped = read_events(&dir.path().join(SKIPPED_FILE));
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].decoded_at.is_some());

        assert!(!dir.path().join(DECODED_FILE).exists());
        assert!(!dir.path().join(BOT_FILE).exists());
    }

    #[test]
    fn bot_event_goes_to_bot_file_regardless_of_threshold() {
        let dir = tempfile::tempdir().unwrap();

        // passed_threshold = true, but is_bot_whale = true takes precedence.
        let event = sample_event(true, true);
        write_event(&event, dir.path()).expect("write event");

        let bots = read_events(&dir.path().join(BOT_FILE));
        assert_eq!(bots.len(), 1);

        assert!(!dir.path().join(DECODED_FILE).exists());
        assert!(!dir.path().join(SKIPPED_FILE).exists());
    }

    #[test]
    fn bot_event_with_failed_threshold_still_goes_to_bot_file() {
        let dir = tempfile::tempdir().unwrap();

        let event = sample_event(true, false);
        write_event(&event, dir.path()).expect("write event");

        let bots = read_events(&dir.path().join(BOT_FILE));
        assert_eq!(bots.len(), 1);
        assert!(!dir.path().join(SKIPPED_FILE).exists());
    }

    #[test]
    fn two_passed_events_append_into_array_of_two() {
        let dir = tempfile::tempdir().unwrap();

        write_event(&sample_event(false, true), dir.path()).expect("write first");
        write_event(&sample_event(false, true), dir.path()).expect("write second");

        let decoded = read_events(&dir.path().join(DECODED_FILE));
        assert_eq!(decoded.len(), 2);
    }

    #[test]
    fn malformed_existing_file_recovers_to_single_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DECODED_FILE);
        std::fs::write(&path, "not json").expect("seed malformed file");

        write_event(&sample_event(false, true), dir.path()).expect("write event recovers");

        let decoded = read_events(&path);
        assert_eq!(decoded.len(), 1);
    }
}
