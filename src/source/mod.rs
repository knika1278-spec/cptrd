//! Transaction source abstraction.
//!
//! Supports two modes:
//! - `WSS`: Helius WebSocket (transactionSubscribe) — for beta testing
//! - `GRPC`: Yellowstone gRPC (Shredstream) — for production low-latency
//!
//! Mode is selected via `MODE` env var: `WSS` (default) or `GRPC`.

pub mod wss;
pub mod grpc;

use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedSender;
use crate::models::types::{BotConfig, DecodedTradeEvent};

/// Transaction source mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    /// Helius WebSocket — easy to set up, ~50-200ms latency.
    Wss,
    /// Yellowstone gRPC — lowest latency, requires Shredstream endpoint.
    Grpc,
}

impl SourceMode {
    /// Parse from env var string.
    pub fn from_env() -> Self {
        match std::env::var("MODE").unwrap_or_default().to_uppercase().as_str() {
            "GRPC" | "YELLOWSTONE" | "SHREDSTREAM" => Self::Grpc,
            _ => Self::Wss,
        }
    }
}

/// Run the transaction source in the selected mode.
///
/// Connects to the data source, subscribes to whale accounts, and forwards
/// decoded trade events to the sender channel.
pub async fn run(
    mode: SourceMode,
    config: BotConfig,
    whales: HashSet<String>,
    sender: UnboundedSender<Vec<DecodedTradeEvent>>,
) -> anyhow::Result<()> {
    match mode {
        SourceMode::Wss => {
            tracing::info!("starting WSS mode (Helius WebSocket)");
            wss::run(config, whales, sender).await
        }
        SourceMode::Grpc => {
            tracing::info!("starting gRPC mode (Yellowstone Shredstream)");
            grpc::run(config, whales, sender).await
        }
    }
}
