//! Shared data types for the copytrade bot.
//!
//! Two groups live here:
//! - **Domain types** — config and the decoded trade event the pipeline emits.
//! - **Helius notification types** — model the verified `transactionSubscribe`
//!   notification shape (`encoding: "jsonParsed"`) from `docs/dex-reference.md` §1.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// A. Domain types
// ---------------------------------------------------------------------------

/// A whale wallet to track, with an optional human-friendly label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhaleConfig {
    pub address: String,
    pub label: Option<String>,
}

/// Default minimum SOL threshold when absent from config.
fn default_min_sol_threshold() -> f64 {
    2.0
}

/// Top-level bot configuration (typically loaded from `whales.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotConfig {
    pub helius_api_key: String,
    /// Built from the key if absent (see `docs/dex-reference.md` §1).
    pub helius_wss_url: Option<String>,
    #[serde(default)]
    pub whales: Vec<WhaleConfig>,
    #[serde(default = "default_min_sol_threshold")]
    pub min_sol_threshold: f64,
}

/// Supported DEX programs. Identify by program ID, never by discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DexProtocol {
    PumpFun,
    PumpSwap,
    RaydiumLaunchpad,
    MeteoraDlmmV2,
    Unknown,
}

/// Trade direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeAction {
    Buy,
    Sell,
}

/// A fully decoded trade event produced by the decode pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedTradeEvent {
    pub signature: String,
    pub slot: u64,
    pub timestamp: Option<i64>,
    pub whale_address: String,
    pub dex: DexProtocol,
    pub action: TradeAction,
    pub mint: String,
    pub sol_amount: f64,
    pub token_amount: u64,
    pub token_decimals: u8,
    pub is_bot_whale: bool,
    pub passed_threshold: bool,
    pub guard_skip_reason: Option<String>,
    /// Set later by the output layer (ISO-8601). Defaults to `None`.
    pub decoded_at: Option<String>,
}

// ---------------------------------------------------------------------------
// B. Helius `transactionSubscribe` notification (jsonParsed) — §1 contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeliusNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: NotificationParams,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationParams {
    pub subscription: u64,
    pub result: TxResult,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxResult {
    pub signature: String,
    pub slot: u64,
    pub transaction: TxEnvelope,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxEnvelope {
    pub transaction: TxInner,
    pub meta: TxMeta,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxInner {
    pub message: TxMessage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxMessage {
    pub account_keys: Vec<AccountKey>,
    pub instructions: Vec<UiInstruction>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountKey {
    pub pubkey: String,
    pub signer: bool,
    pub writable: bool,
}

/// A jsonParsed instruction. Helius returns EITHER a `parsed` form (known
/// programs) OR a partially-decoded `{ programId, accounts, data }` form for
/// our DEX programs. All fields are optional so both shapes deserialize.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiInstruction {
    #[serde(default)]
    pub program_id: Option<String>,
    #[serde(default)]
    pub accounts: Option<Vec<String>>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub parsed: Option<serde_json::Value>,
    #[serde(default)]
    pub program: Option<String>,
}

impl UiInstruction {
    /// Program id of this instruction, if present.
    pub fn program_id(&self) -> Option<&str> {
        self.program_id.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxMeta {
    pub err: Option<serde_json::Value>,
    pub fee: u64,
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    #[serde(default)]
    pub pre_token_balances: Vec<TokenBalance>,
    #[serde(default)]
    pub post_token_balances: Vec<TokenBalance>,
    #[serde(default)]
    pub inner_instructions: Vec<InnerInstructions>,
    #[serde(default)]
    pub log_messages: Option<Vec<String>>,
    #[serde(default)]
    pub loaded_addresses: Option<LoadedAddresses>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBalance {
    pub account_index: u64,
    pub mint: String,
    pub owner: Option<String>,
    pub ui_token_amount: UiTokenAmount,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTokenAmount {
    pub amount: String,
    pub decimals: u8,
    pub ui_amount: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerInstructions {
    pub index: u32,
    pub instructions: Vec<UiInstruction>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedAddresses {
    pub writable: Vec<String>,
    pub readonly: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_trade_event_round_trips_through_serde() {
        let event = DecodedTradeEvent {
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
            is_bot_whale: true,
            passed_threshold: true,
            guard_skip_reason: None,
            decoded_at: None,
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let back: DecodedTradeEvent = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back, event);
        assert_eq!(back.dex, DexProtocol::PumpFun);
        assert_eq!(back.action, TradeAction::Buy);
        assert_eq!(back.token_decimals, 6);
    }

    #[test]
    fn bot_config_defaults_min_sol_threshold_to_two() {
        let json = r#"{
            "helius_api_key": "key-abc",
            "helius_wss_url": null
        }"#;

        let cfg: BotConfig = serde_json::from_str(json).expect("deserialize default config");

        assert_eq!(cfg.min_sol_threshold, 2.0);
        assert!(cfg.whales.is_empty());
        assert_eq!(cfg.helius_api_key, "key-abc");
    }

    #[test]
    fn bot_config_uses_explicit_min_sol_threshold() {
        let json = r#"{
            "helius_api_key": "key-abc",
            "helius_wss_url": null,
            "whales": [{ "address": "Whale1", "label": "alpha" }],
            "min_sol_threshold": 5.0
        }"#;

        let cfg: BotConfig = serde_json::from_str(json).expect("deserialize config");

        assert_eq!(cfg.min_sol_threshold, 5.0);
        assert_eq!(cfg.whales.len(), 1);
        assert_eq!(cfg.whales[0].address, "Whale1");
        assert_eq!(cfg.whales[0].label.as_deref(), Some("alpha"));
    }

    #[test]
    fn parses_realistic_transaction_notification() {
        // Shape per docs/dex-reference.md §1: camelCase JSON keys, one
        // partially-decoded instruction, meta with balance deltas + token balances.
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
                                    { "pubkey": "WhaleTrader1111111111111111111111111111111", "signer": true, "writable": true },
                                    { "pubkey": "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P", "signer": false, "writable": false }
                                ],
                                "instructions": [
                                    {
                                        "programId": "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
                                        "accounts": ["WhaleTrader1111111111111111111111111111111", "Mint1111111111111111111111111111111111111111"],
                                        "data": "3Bxs4h24hBtQy9rw"
                                    }
                                ]
                            },
                            "signatures": ["5xSignatureExample"]
                        },
                        "meta": {
                            "err": null,
                            "fee": 5000,
                            "preBalances": [10000000000, 0],
                            "postBalances": [6500000000, 0],
                            "preTokenBalances": [
                                {
                                    "accountIndex": 0,
                                    "mint": "Mint1111111111111111111111111111111111111111",
                                    "owner": "WhaleTrader1111111111111111111111111111111",
                                    "uiTokenAmount": { "amount": "0", "decimals": 6, "uiAmount": null }
                                }
                            ],
                            "postTokenBalances": [
                                {
                                    "accountIndex": 0,
                                    "mint": "Mint1111111111111111111111111111111111111111",
                                    "owner": "WhaleTrader1111111111111111111111111111111",
                                    "uiTokenAmount": { "amount": "1000000", "decimals": 6, "uiAmount": 1.0 }
                                }
                            ],
                            "innerInstructions": [],
                            "logMessages": ["Program log: Instruction: Buy"],
                            "loadedAddresses": { "writable": [], "readonly": [] }
                        }
                    }
                }
            }
        }"#;

        let notif: HeliusNotification =
            serde_json::from_str(json).expect("parse transactionNotification");

        assert_eq!(notif.method, "transactionNotification");
        assert_eq!(notif.params.subscription, 42);

        let result = &notif.params.result;
        assert_eq!(result.signature, "5xSignatureExample");
        assert_eq!(result.slot, 987_654_321);

        let ix = &result.transaction.transaction.message.instructions[0];
        assert_eq!(
            ix.program_id(),
            Some("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")
        );
        assert_eq!(ix.accounts.as_ref().unwrap().len(), 2);
        assert_eq!(ix.data.as_deref(), Some("3Bxs4h24hBtQy9rw"));

        let meta = &result.transaction.meta;
        assert_eq!(meta.pre_balances[0], 10_000_000_000);
        assert_eq!(meta.post_balances[0], 6_500_000_000);

        let post = &meta.post_token_balances[0];
        assert_eq!(post.mint, "Mint1111111111111111111111111111111111111111");
        assert_eq!(post.ui_token_amount.decimals, 6);
        assert_eq!(post.ui_token_amount.amount, "1000000");
        assert_eq!(post.owner.as_deref(), Some("WhaleTrader1111111111111111111111111111111"));
    }
}
