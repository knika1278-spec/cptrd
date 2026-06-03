//! JSON output writer.
//!
//! Each decoded event is stamped with a `decoded_at` (RFC3339 UTC) timestamp
//! and appended to ONE JSON-array file per whale, named `<whale_address>.json`
//! under the output directory. Every event for a given whale — regardless of
//! guard outcome — lands in that whale's file; the per-event flags
//! (`is_bot_whale`, `passed_threshold`, `guard_skip_reason`) preserve the guard
//! classification inline, so a single file is enough to audit one whale.
//!
//! Append semantics: read the existing array, push the new event, write back
//! pretty-printed. A missing/empty/malformed file is treated as an empty array.

use std::path::Path;

use anyhow::Context;

use crate::models::types::DecodedTradeEvent;

/// File extension for per-whale output files.
const FILE_EXT: &str = "json";

/// The output file name for a whale: `<whale_address>.json`.
///
/// The address is validated as base58 of pubkey length before use. This is a
/// path-traversal guard: the address becomes a file name via `Path::join`, so a
/// value bearing `/`, `\`, `.`, or `..` must never reach the filesystem. A valid
/// Solana pubkey is 32 bytes → 32..=44 base58 chars, none of which are path
/// separators, so rejecting anything else keeps writes inside the output dir.
fn whale_file_name(whale_address: &str) -> anyhow::Result<String> {
    const BASE58_MIN: usize = 32;
    const BASE58_MAX: usize = 44;
    let is_base58 = whale_address.bytes().all(|b| {
        matches!(b,
            b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
    });
    anyhow::ensure!(
        (BASE58_MIN..=BASE58_MAX).contains(&whale_address.len()) && is_base58,
        "refusing to use non-base58 whale address as a file name: {whale_address:?}"
    );
    Ok(format!("{whale_address}.{FILE_EXT}"))
}

/// Stamp the event with `decoded_at` (RFC3339 UTC) and append it to the file for
/// its whale (`<whale_address>.json`) under `output_dir`. Creates the file (as a
/// `[]` array) if missing, and creates `output_dir` if missing.
pub fn write_event(event: &DecodedTradeEvent, output_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create output dir {}", output_dir.display()))?;

    let mut stamped = event.clone();
    stamped.decoded_at = Some(chrono::Utc::now().to_rfc3339());

    let file_name = whale_file_name(&stamped.whale_address)?;
    let path = output_dir.join(&file_name);

    append_event(&path, &stamped)?;

    tracing::info!(
        signature = %stamped.signature,
        whale = %stamped.whale_address,
        file = %file_name,
        "wrote decoded event to whale output file"
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

    fn sample_event(
        whale_address: &str,
        is_bot_whale: bool,
        passed_threshold: bool,
    ) -> DecodedTradeEvent {
        DecodedTradeEvent {
            signature: "5xSig".to_string(),
            slot: 123_456,
            timestamp: Some(1_717_000_000),
            whale_address: whale_address.to_string(),
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
            ix_accounts: None,
            execution: None,
        }
    }

    fn read_events(path: &Path) -> Vec<DecodedTradeEvent> {
        let contents = std::fs::read_to_string(path).expect("read file");
        serde_json::from_str(&contents).expect("parse array")
    }

    #[test]
    fn event_goes_to_file_named_after_whale_with_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let event = sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, true);

        write_event(&event, dir.path()).expect("write event");

        let path = dir
            .path()
            .join("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt.json");
        let events = read_events(&path);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].whale_address,
            "EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt"
        );
        assert!(events[0].decoded_at.is_some());
        assert!(!events[0].decoded_at.as_ref().unwrap().is_empty());
    }

    #[test]
    fn events_for_different_whales_go_to_separate_files() {
        let dir = tempfile::tempdir().unwrap();

        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, true),
            dir.path(),
        )
        .expect("write a");
        write_event(
            &sample_event("D2wBctC1K2mEtA17i8ZfdEubkiksiAH2j8F7ri3ec71V", false, true),
            dir.path(),
        )
        .expect("write b");

        let a = read_events(
            &dir.path()
                .join("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt.json"),
        );
        let b = read_events(
            &dir.path()
                .join("D2wBctC1K2mEtA17i8ZfdEubkiksiAH2j8F7ri3ec71V.json"),
        );
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(
            a[0].whale_address,
            "EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt"
        );
        assert_eq!(
            b[0].whale_address,
            "D2wBctC1K2mEtA17i8ZfdEubkiksiAH2j8F7ri3ec71V"
        );
    }

    #[test]
    fn all_guard_outcomes_for_one_whale_share_one_file() {
        let dir = tempfile::tempdir().unwrap();

        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, true),
            dir.path(),
        )
        .expect("passed");
        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, false),
            dir.path(),
        )
        .expect("skipped");
        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", true, true),
            dir.path(),
        )
        .expect("bot");

        let events = read_events(
            &dir.path()
                .join("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt.json"),
        );
        assert_eq!(events.len(), 3);
        // Flags are preserved inline for auditing.
        assert!(events.iter().any(|e| e.passed_threshold && !e.is_bot_whale));
        assert!(events
            .iter()
            .any(|e| !e.passed_threshold && !e.is_bot_whale));
        assert!(events.iter().any(|e| e.is_bot_whale));
    }

    #[test]
    fn two_events_same_whale_append_into_array_of_two() {
        let dir = tempfile::tempdir().unwrap();

        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, true),
            dir.path(),
        )
        .expect("write first");
        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, true),
            dir.path(),
        )
        .expect("write second");

        let events = read_events(
            &dir.path()
                .join("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt.json"),
        );
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn malformed_existing_file_recovers_to_single_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt.json");
        std::fs::write(&path, "not json").expect("seed malformed file");

        write_event(
            &sample_event("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt", false, true),
            dir.path(),
        )
        .expect("write recovers");

        let events = read_events(&path);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn rejects_whale_address_with_path_separators() {
        let dir = tempfile::tempdir().unwrap();
        // A traversal attempt must error out, not write outside the output dir.
        let event = sample_event("../../etc/evil", false, true);

        let result = write_event(&event, dir.path());

        assert!(result.is_err());
        assert!(!dir.path().parent().unwrap().join("etc").exists());
    }

    #[test]
    fn rejects_too_short_address() {
        assert!(whale_file_name("short").is_err());
    }

    #[test]
    fn accepts_valid_base58_address() {
        let name = whale_file_name("EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt")
            .expect("valid base58 address");
        assert_eq!(name, "EwTNPYTuwxMzrvL19nzBsSLXdAoEmVBKkisN87csKgtt.json");
    }
}
