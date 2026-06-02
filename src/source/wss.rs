//! WSS source — wraps the existing Helius WebSocket client.
//!
//! This is the beta testing mode. Latency: ~50-200ms from chain to bot.

use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedSender;
use crate::helius::wss;
use crate::models::types::{BotConfig, DecodedTradeEvent};

/// Run the Helius WSS source.
pub async fn run(
    config: BotConfig,
    whales: HashSet<String>,
    sender: UnboundedSender<Vec<DecodedTradeEvent>>,
) -> anyhow::Result<()> {
    wss::run(config, whales, sender).await
}
