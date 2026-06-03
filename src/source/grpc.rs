//! gRPC source — Yellowstone/Shredstream transaction streaming.
//!
//! Production mode with lowest latency (~5-20ms from chain to bot).
//!
//! Requires:
//! - Shredstream endpoint (e.g. from Helius, Triton, or self-hosted)
//! - `GRPC_URL` env var pointing to the Yellowstone gRPC endpoint
//! - `GRPC_API_TOKEN` env var for authentication (optional)
//!
//! This module uses the Yellowstone gRPC proto to subscribe to transactions
//! filtered by account (whale addresses).
//!
//! ## Setup
//!
//! 1. Get a Shredstream endpoint from your provider
//! 2. Set in `.env`:
//!    ```
//!    MODE=GRPC
//!    GRPC_URL=https://your-shredstream-endpoint:10000
//!    GRPC_API_TOKEN=your-token (optional)
//!    ```
//! 3. Run the bot — it will connect via gRPC instead of WSS

use crate::models::types::{BotConfig, DecodedTradeEvent};
use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedSender;

/// Run the Yellowstone gRPC source.
///
/// Subscribes to transactions involving the tracked whale accounts
/// and forwards decoded trade events to the sender channel.
pub async fn run(
    _config: BotConfig,
    _whales: HashSet<String>,
    _sender: UnboundedSender<Vec<DecodedTradeEvent>>,
) -> anyhow::Result<()> {
    let grpc_url = std::env::var("GRPC_URL").unwrap_or_default();

    if grpc_url.is_empty() {
        anyhow::bail!(
            "GRPC_URL not set. Add to .env:\n\
             GRPC_URL=https://your-shredstream-endpoint:10000\n\
             \n\
             Providers:\n\
             - Helius: https://shredstream.helius.dev\n\
             - Triton: https://triton-yellowstone.grpc.com\n\
             - Self-hosted: https://github.com/jito-foundation/shredstream"
        );
    }

    let _api_token = std::env::var("GRPC_API_TOKEN").ok();

    tracing::info!(url = %grpc_url, "connecting to Yellowstone gRPC");

    // TODO: Implement Yellowstone gRPC subscription
    //
    // The implementation will:
    // 1. Connect to the gRPC endpoint using tonic
    // 2. Subscribe to transactions filtered by whale account addresses
    // 3. Parse each transaction notification into our TxResult format
    // 4. Route through the decoder pipeline (same as WSS)
    // 5. Forward decoded events to the sender channel
    //
    // Proto: https://github.com/jito-foundation/geyser-grpc-connector
    // Example: https://github.com/rpcpool/yellowstone-grpc
    //
    // Key difference from WSS:
    // - gRPC delivers transactions ~5-20ms after on-chain confirmation
    // - WSS delivers ~50-200ms after on-chain confirmation
    // - gRPC uses protobuf (more efficient than JSON)
    // - gRPC supports filtered subscriptions (only whale txs)

    anyhow::bail!(
        "Yellowstone gRPC source not yet implemented.\n\
         Use MODE=WSS for beta testing.\n\
         For production, implement the gRPC client in src/source/grpc.rs"
    )
}
