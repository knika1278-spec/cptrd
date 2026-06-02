//! copytrade — Solana copytrade bot entrypoint.
//!
//! Orchestration: load config, connect to Helius `transactionSubscribe`, and run the
//! decode → guard → output pipeline. The WSS client forwards per-transaction groups of
//! decoded events; Guard 1 (bot filter) needs the whole group, then Guard 2 (threshold)
//! and the JSON writer run per event. Ctrl-C triggers graceful shutdown.

mod decoder;
mod guard;
mod helius;
mod models;
mod output;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::helius::wss;
use crate::models::types::{BotConfig, DecodedTradeEvent};

/// Default config path when `WHALES_CONFIG_PATH` is unset.
const DEFAULT_CONFIG_PATH: &str = "config/whales.json";
/// Default output directory when `OUTPUT_DIR` is unset.
const DEFAULT_OUTPUT_DIR: &str = "output";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path =
        std::env::var("WHALES_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());
    let mut config = load_config(&config_path)?;

    // Resolve API key + optional WSS override from env.
    config.helius_api_key =
        resolve_api_key(&config.helius_api_key, std::env::var("HELIUS_API_KEY").ok())?;
    if let Ok(url) = std::env::var("HELIUS_WSS_URL") {
        if !url.is_empty() {
            config.helius_wss_url = Some(url);
        }
    }

    let whales: HashSet<String> = config
        .whales
        .iter()
        .map(|whale| whale.address.clone())
        .collect();
    if whales.is_empty() {
        anyhow::bail!(
            "no whales configured in {config_path}; add at least one entry under \"whales\" \
             (refusing to subscribe with no accounts)"
        );
    }
    tracing::info!("tracking {} whale account(s)", whales.len());

    let output_dir = PathBuf::from(
        std::env::var("OUTPUT_DIR").unwrap_or_else(|_| DEFAULT_OUTPUT_DIR.to_string()),
    );
    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create output dir {}", output_dir.display()))?;

    let min_threshold = config.min_sol_threshold;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<DecodedTradeEvent>>();
    let wss_handle = tokio::spawn(async move { wss::run(config, whales, tx).await });

    tracing::info!(
        "copytrade pipeline started (min threshold {:.4} SOL)",
        min_threshold
    );

    loop {
        tokio::select! {
            maybe_events = rx.recv() => {
                match maybe_events {
                    Some(mut events) => {
                        process_group(&mut events, min_threshold, &output_dir);
                    }
                    None => {
                        tracing::info!("WSS task ended (channel closed); shutting down");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown signal received");
                break;
            }
        }
    }

    tracing::info!("copytrade shutting down");
    wss_handle.abort();
    Ok(())
}

/// Run the guard → output pipeline over one transaction's decoded events:
/// Guard 1 (bot filter) over the whole group, then Guard 2 (threshold) and the JSON
/// writer per event. Logs a concise line per event.
fn process_group(
    events: &mut [DecodedTradeEvent],
    min_threshold: f64,
    output_dir: &std::path::Path,
) {
    if let Some(reason) = guard::bot_filter::apply_bot_filter(events) {
        let signature = events.first().map(|e| e.signature.as_str()).unwrap_or("");
        tracing::warn!(signature = %signature, "bot whale tx flagged: {}", reason);
    }

    for event in events.iter_mut() {
        guard::threshold_filter::apply_threshold(event, min_threshold);

        if let Err(err) = output::json_writer::write_event(event, output_dir) {
            tracing::error!(
                signature = %event.signature,
                "failed to write event: {:#}",
                err
            );
        }

        let skip = event.guard_skip_reason.as_deref().unwrap_or("-");
        tracing::info!(
            dex = ?event.dex,
            action = ?event.action,
            mint = %event.mint,
            sol_amount = event.sol_amount,
            passed_threshold = event.passed_threshold,
            is_bot_whale = event.is_bot_whale,
            skip_reason = %skip,
            "decoded trade event"
        );
    }
}

/// Load and parse the bot config from `path`. Returns a helpful error when the file is
/// missing (suggesting the example), or when JSON parsing fails.
fn load_config(path: &str) -> anyhow::Result<BotConfig> {
    let content = std::fs::read_to_string(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "config file '{path}' not found; copy config/whales.example.json to \
                 config/whales.json and fill in your whale addresses + Helius API key"
            )
        } else {
            anyhow::Error::new(err).context(format!("failed to read config file '{path}'"))
        }
    })?;

    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse config file '{path}' as JSON"))
}

/// Resolve the Helius API key: prefer the non-empty config value, else fall back to the
/// provided env value. Bails if both are empty/absent.
fn resolve_api_key(config_key: &str, env_key: Option<String>) -> anyhow::Result<String> {
    if !config_key.is_empty() {
        return Ok(config_key.to_string());
    }
    match env_key {
        Some(key) if !key.is_empty() => Ok(key),
        _ => anyhow::bail!(
            "Helius API key missing: set \"helius_api_key\" in the config file or the \
             HELIUS_API_KEY environment variable"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(json: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        file.write_all(json.as_bytes()).expect("write temp config");
        file
    }

    #[test]
    fn load_config_parses_valid_file() {
        let json = r#"{
            "helius_api_key": "key-abc",
            "helius_wss_url": "wss://custom.example.com/?api-key=key-abc",
            "min_sol_threshold": 5.0,
            "whales": [{ "address": "Whale1", "label": "alpha" }]
        }"#;
        let file = write_config(json);

        let config = load_config(file.path().to_str().unwrap()).expect("valid config parses");

        assert_eq!(config.helius_api_key, "key-abc");
        assert_eq!(config.min_sol_threshold, 5.0);
        assert_eq!(config.whales.len(), 1);
        assert_eq!(config.whales[0].address, "Whale1");
    }

    #[test]
    fn load_config_defaults_min_threshold_when_omitted() {
        let json = r#"{
            "helius_api_key": "key-abc",
            "helius_wss_url": null,
            "whales": [{ "address": "Whale1", "label": null }]
        }"#;
        let file = write_config(json);

        let config = load_config(file.path().to_str().unwrap()).expect("config parses");

        assert_eq!(config.min_sol_threshold, 2.0);
    }

    #[test]
    fn load_config_returns_err_when_missing() {
        let result = load_config("definitely/not/a/real/path/whales.json");

        let err = result.expect_err("missing file is an error");
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn resolve_api_key_prefers_config_value() {
        let key = resolve_api_key("config-key", Some("env-key".to_string())).unwrap();
        assert_eq!(key, "config-key");
    }

    #[test]
    fn resolve_api_key_falls_back_to_env_when_config_empty() {
        let key = resolve_api_key("", Some("env-key".to_string())).unwrap();
        assert_eq!(key, "env-key");
    }

    #[test]
    fn resolve_api_key_bails_when_both_empty() {
        assert!(resolve_api_key("", None).is_err());
        assert!(resolve_api_key("", Some(String::new())).is_err());
    }
}
