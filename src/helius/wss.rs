//! Helius `transactionSubscribe` WebSocket client.
//!
//! Implements the Geyser Enhanced WebSocket subscription verified in
//! `docs/dex-reference.md` §1 (request shape, `jsonParsed` encoding, notification
//! shape, ack/ping frames). Network-free logic lives in small pure helpers that are
//! unit-tested; the async connect/reconnect loop is a thin shell around them.

use std::collections::HashSet;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::models::types::{BotConfig, DecodedTradeEvent, HeliusNotification};

/// Initial reconnect backoff (doubles up to [`MAX_BACKOFF`], resets on success).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Maximum reconnect backoff cap.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// JSON-RPC id used for the single `transactionSubscribe` request per connection.
const SUBSCRIBE_ID: u64 = 1;

// ---------------------------------------------------------------------------
// Pure helpers (no network — unit-tested below)
// ---------------------------------------------------------------------------

/// Build the WebSocket URL.
///
/// Prefers `config.helius_wss_url` when set and non-empty. Otherwise builds the
/// roadmap default `wss://atlas-mainnet.helius-rpc.com/?api-key=<key>`.
///
/// Note (docs §1): some accounts are served from `mainnet.helius-rpc.com` instead
/// of the historical Atlas host — that is why the URL stays config-overridable.
pub fn build_wss_url(config: &BotConfig) -> String {
    match &config.helius_wss_url {
        Some(url) if !url.is_empty() => url.clone(),
        _ => format!(
            "wss://atlas-mainnet.helius-rpc.com/?api-key={}",
            config.helius_api_key
        ),
    }
}

/// Build the JSON-RPC `transactionSubscribe` request string per docs §1.
pub fn build_subscribe_message(id: u64, accounts: &[String]) -> String {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "transactionSubscribe",
        "params": [
            {
                "accountInclude": accounts,
                "vote": false,
                "failed": false
            },
            {
                "commitment": "confirmed",
                "encoding": "jsonParsed",
                "transactionDetails": "full",
                "showRewards": false,
                "maxSupportedTransactionVersion": 0
            }
        ]
    });
    request.to_string()
}

/// Parse a text frame into a [`HeliusNotification`].
///
/// Returns `None` for any frame that is not a fully-formed transaction
/// notification — subscribe acks (`{result:<subId>,id}`), pings, and errors do not
/// match the struct and so deserialize to `None` (also guarded on `method`).
pub fn parse_notification(text: &str) -> Option<HeliusNotification> {
    let notification = serde_json::from_str::<HeliusNotification>(text).ok()?;
    if notification.method == "transactionNotification" {
        Some(notification)
    } else {
        None
    }
}

/// Decode a notification into a trade event by routing through the decoder.
pub fn decode_notification(
    notification: &HeliusNotification,
    whales: &HashSet<String>,
) -> Option<DecodedTradeEvent> {
    crate::decoder::route(&notification.params.result, whales)
}

// ---------------------------------------------------------------------------
// Async connect/reconnect loop (thin)
// ---------------------------------------------------------------------------

/// Connect to Helius, subscribe to every whale account, and forward decoded trade
/// events to `sender`. Reconnects forever with exponential backoff (1s → 30s cap,
/// reset to 1s after a successful connect+subscribe). Never panics. Returns `Ok(())`
/// only when the receiver half of `sender` is closed (nothing left to do).
pub async fn run(
    config: BotConfig,
    whales: HashSet<String>,
    sender: UnboundedSender<DecodedTradeEvent>,
) -> anyhow::Result<()> {
    let url = build_wss_url(&config);
    let accounts: Vec<String> = config
        .whales
        .iter()
        .map(|whale| whale.address.clone())
        .collect();

    let mut backoff = INITIAL_BACKOFF;

    loop {
        match connect_once(&url, &accounts, &whales, &sender).await {
            ConnectionOutcome::ReceiverClosed => {
                tracing::info!("event receiver closed; stopping Helius WSS client");
                return Ok(());
            }
            ConnectionOutcome::Disconnected => {
                // A clean connect happened (backoff already reset inside); a normal
                // disconnect just retries immediately at the reset interval.
                tracing::warn!("Helius WSS disconnected; reconnecting in {:?}", backoff);
            }
            ConnectionOutcome::Failed => {
                tracing::error!("Helius WSS connection failed; backing off {:?}", backoff);
            }
        }

        tokio::time::sleep(backoff).await;
        // Double on each failed attempt up to the cap. A successful connect resets
        // `backoff` to INITIAL inside `connect_once` via the returned outcome below.
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Outcome of a single connection attempt.
enum ConnectionOutcome {
    /// Connected and subscribed, then the stream closed/errored (reconnect).
    Disconnected,
    /// Failed to connect or subscribe (reconnect with backoff).
    Failed,
    /// The downstream receiver was dropped — stop the loop.
    ReceiverClosed,
}

/// One connection lifecycle: connect, subscribe, then pump messages until the
/// stream ends. Reset-on-success is signaled to the caller via the outcome.
async fn connect_once(
    url: &str,
    accounts: &[String],
    whales: &HashSet<String>,
    sender: &UnboundedSender<DecodedTradeEvent>,
) -> ConnectionOutcome {
    tracing::info!("connecting to Helius WSS endpoint: {}", url);

    let stream = match connect_async(url).await {
        Ok((stream, _response)) => stream,
        Err(err) => {
            tracing::error!("Helius WSS connect error: {}", err);
            return ConnectionOutcome::Failed;
        }
    };

    let (mut write, mut read) = stream.split();

    let subscribe = build_subscribe_message(SUBSCRIBE_ID, accounts);
    if let Err(err) = write.send(Message::Text(subscribe.into())).await {
        tracing::error!("Helius WSS subscribe send error: {}", err);
        return ConnectionOutcome::Failed;
    }
    tracing::info!(
        "subscribed to transactionSubscribe for {} whale account(s)",
        accounts.len()
    );

    while let Some(message) = read.next().await {
        let message = match message {
            Ok(message) => message,
            Err(err) => {
                tracing::warn!("Helius WSS read error: {}", err);
                return ConnectionOutcome::Disconnected;
            }
        };

        match message {
            Message::Text(text) => {
                if let Some(notification) = parse_notification(&text) {
                    if let Some(event) = decode_notification(&notification, whales) {
                        if sender.send(event).is_err() {
                            return ConnectionOutcome::ReceiverClosed;
                        }
                    }
                }
            }
            Message::Ping(payload) => {
                if let Err(err) = write.send(Message::Pong(payload)).await {
                    tracing::warn!("Helius WSS pong send error: {}", err);
                    return ConnectionOutcome::Disconnected;
                }
            }
            Message::Close(_) => {
                tracing::info!("Helius WSS received close frame");
                return ConnectionOutcome::Disconnected;
            }
            _ => {
                // Pong / Binary / Frame: nothing to act on.
            }
        }
    }

    ConnectionOutcome::Disconnected
}

// ---------------------------------------------------------------------------
// Tests — pure helpers only (no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::{
        AccountKey, DexProtocol, TokenBalance, TxEnvelope, TxInner, TxMessage, TxMeta, TxResult,
        UiInstruction, UiTokenAmount, WhaleConfig,
    };

    const TRADER: &str = "WhaleTrader1111111111111111111111111111111";
    const MINT: &str = "Mint1111111111111111111111111111111111111111";
    const PUMPFUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

    fn config(wss_url: Option<&str>) -> BotConfig {
        BotConfig {
            helius_api_key: "key-abc".to_string(),
            helius_wss_url: wss_url.map(|url| url.to_string()),
            whales: vec![WhaleConfig {
                address: TRADER.to_string(),
                label: None,
            }],
            min_sol_threshold: 2.0,
        }
    }

    #[test]
    fn build_wss_url_uses_config_url_when_set() {
        let cfg = config(Some("wss://custom.example.com/?api-key=xyz"));
        assert_eq!(build_wss_url(&cfg), "wss://custom.example.com/?api-key=xyz");
    }

    #[test]
    fn build_wss_url_builds_atlas_url_when_unset() {
        let cfg = config(None);
        assert_eq!(
            build_wss_url(&cfg),
            "wss://atlas-mainnet.helius-rpc.com/?api-key=key-abc"
        );
    }

    #[test]
    fn build_wss_url_builds_atlas_url_when_empty() {
        let cfg = config(Some(""));
        assert_eq!(
            build_wss_url(&cfg),
            "wss://atlas-mainnet.helius-rpc.com/?api-key=key-abc"
        );
    }

    #[test]
    fn build_subscribe_message_matches_docs_shape() {
        let accounts = vec!["Whale1".to_string(), "Whale2".to_string()];
        let message = build_subscribe_message(1, &accounts);

        let value: serde_json::Value =
            serde_json::from_str(&message).expect("subscribe message is valid JSON");

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert_eq!(value["method"], "transactionSubscribe");

        let filter = &value["params"][0];
        assert_eq!(
            filter["accountInclude"],
            serde_json::json!(["Whale1", "Whale2"])
        );
        assert_eq!(filter["vote"], false);
        assert_eq!(filter["failed"], false);

        let options = &value["params"][1];
        assert_eq!(options["commitment"], "confirmed");
        assert_eq!(options["encoding"], "jsonParsed");
        assert_eq!(options["transactionDetails"], "full");
        assert_eq!(options["showRewards"], false);
        assert_eq!(options["maxSupportedTransactionVersion"], 0);
    }

    #[test]
    fn parse_notification_accepts_transaction_notification() {
        let json = r#"{
            "jsonrpc": "2.0",
            "method": "transactionNotification",
            "params": {
                "subscription": 42,
                "result": {
                    "signature": "5xSignatureExample",
                    "slot": 987654321,
                    "transactionIndex": 3,
                    "transaction": {
                        "transaction": {
                            "message": {
                                "accountKeys": [
                                    { "pubkey": "WhaleTrader1111111111111111111111111111111", "signer": true, "writable": true }
                                ],
                                "instructions": []
                            },
                            "signatures": ["5xSignatureExample"]
                        },
                        "meta": {
                            "err": null,
                            "fee": 5000,
                            "preBalances": [10000000000],
                            "postBalances": [6500000000],
                            "preTokenBalances": [],
                            "postTokenBalances": [],
                            "innerInstructions": [],
                            "logMessages": [],
                            "loadedAddresses": { "writable": [], "readonly": [] }
                        }
                    }
                }
            }
        }"#;

        let notification = parse_notification(json).expect("notification parses");
        assert_eq!(notification.method, "transactionNotification");
        assert_eq!(notification.params.subscription, 42);
        assert_eq!(notification.params.result.signature, "5xSignatureExample");
    }

    #[test]
    fn parse_notification_rejects_subscribe_ack() {
        let ack = r#"{"jsonrpc":"2.0","result":123,"id":1}"#;
        assert!(parse_notification(ack).is_none());
    }

    #[test]
    fn parse_notification_rejects_junk() {
        assert!(parse_notification("not json at all").is_none());
        assert!(parse_notification("{}").is_none());
    }

    fn token_balance(amount: &str, decimals: u8) -> TokenBalance {
        TokenBalance {
            account_index: 0,
            mint: MINT.to_string(),
            owner: Some(TRADER.to_string()),
            ui_token_amount: UiTokenAmount {
                amount: amount.to_string(),
                decimals,
                ui_amount: None,
            },
        }
    }

    fn pumpfun_buy_result() -> TxResult {
        let args = [0u8, 0, 0, 0, 0, 0, 0, 0, 1];
        let data = bs58::encode([&DISC_BUY[..], &args].concat()).into_string();
        let instruction = UiInstruction {
            program_id: Some(PUMPFUN_PROGRAM_ID.to_string()),
            accounts: Some(vec![TRADER.to_string(), MINT.to_string()]),
            data: Some(data),
            parsed: None,
            program: None,
        };

        TxResult {
            signature: "5xSignatureExample".to_string(),
            slot: 987_654_321,
            transaction: TxEnvelope {
                transaction: TxInner {
                    message: TxMessage {
                        account_keys: vec![AccountKey {
                            pubkey: TRADER.to_string(),
                            signer: true,
                            writable: true,
                        }],
                        instructions: vec![instruction],
                    },
                },
                meta: TxMeta {
                    err: None,
                    fee: 5000,
                    pre_balances: vec![10_000_000_000],
                    post_balances: vec![6_500_000_000],
                    pre_token_balances: vec![token_balance("0", 6)],
                    post_token_balances: vec![token_balance("1000000", 6)],
                    inner_instructions: vec![],
                    log_messages: None,
                    loaded_addresses: None,
                },
            },
        }
    }

    #[test]
    fn decode_notification_decodes_pumpfun_buy() {
        let notification = HeliusNotification {
            jsonrpc: "2.0".to_string(),
            method: "transactionNotification".to_string(),
            params: crate::models::types::NotificationParams {
                subscription: 42,
                result: pumpfun_buy_result(),
            },
        };
        let whales: HashSet<String> = HashSet::new();

        let event = decode_notification(&notification, &whales).expect("pumpfun buy decodes");
        assert_eq!(event.dex, DexProtocol::PumpFun);
    }
}
