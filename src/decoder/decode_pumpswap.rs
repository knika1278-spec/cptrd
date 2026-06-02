//! PumpSwap AMM decoder.
//!
//! Source: `docs/dex-reference.md` §3 (official pump-public-docs IDL
//! `github.com/pump-fun/pump-public-docs/idl/pump_amm.json`) and §0 (cross-cutting
//! decode strategy). Per §0, the DEX is identified by program ID upstream
//! (the router), and amounts come from `meta` balance deltas — never from
//! instruction args, which are only slippage limits.

use std::collections::HashSet;

use crate::decoder::common;
use crate::models::types::{DecodedTradeEvent, DexProtocol, TradeAction, TxResult, UiInstruction};

/// PumpSwap AMM program. Source: docs/dex-reference.md §3.
pub const PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

// Discriminators are IDENTICAL to PumpFun's (docs §3): PumpSwap shares the same
// `buy`/`sell` bytes. This is exactly why the router disambiguates by PROGRAM_ID
// (docs §0/§3) — the discriminator alone cannot tell the two programs apart.
const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const DISC_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// `user` (trader) account index in PumpSwap buy/sell instructions (docs §3).
const TRADER_ACCOUNT_INDEX: usize = 1;

// NOTE: For more precise fills (fees included), exact amounts could later be read
// from the PumpSwap `BuyEvent` (disc [103,244,82,31,44,245,119,119]) /
// `SellEvent` (disc [62,47,55,10,165,3,220,42]) emitted via self-CPI/log
// (docs §3). Phase 1 uses balance deltas per §0.

/// Decode a single instruction (already matched to `PROGRAM_ID` by the router)
/// into a trade event. Returns `None` if the instruction is not a buy/sell, or
/// if amounts can't be resolved from balance deltas.
pub fn decode(
    result: &TxResult,
    instruction: &UiInstruction,
    whales: &HashSet<String>,
) -> Option<DecodedTradeEvent> {
    // 1. Defensive: confirm this instruction targets PumpSwap.
    if instruction.program_id() != Some(PROGRAM_ID) {
        return None;
    }

    // 2. Discriminator → action (buy/sell). Anything else is skipped.
    let disc = common::discriminator(instruction.data.as_deref()?)?;
    let action = match disc {
        DISC_BUY => TradeAction::Buy,
        DISC_SELL => TradeAction::Sell,
        _ => return None,
    };

    // 3. Trader = tracked whale signer, cross-checked against the DEX trader-index account (§0/§3).
    let (trader_idx, trader) =
        common::resolve_trader(result, instruction, whales, TRADER_ACCOUNT_INDEX)?;

    // 4. Amounts from balance deltas (§0) — NOT instruction args.
    let (mint, tok_delta, decimals) =
        common::traded_mint_for_owner(&result.transaction.meta, &trader)?;
    // Overflow-safe: `unsigned_abs()` is u128; truncating to u64 silently would
    // misreport. A delta exceeding u64 is a non-trade artifact → skip.
    let token_amount = u64::try_from(tok_delta.unsigned_abs()).ok()?;

    let lamports = common::sol_movement_lamports(&result.transaction.meta, trader_idx, &trader)?;
    let sol_amount = (lamports.unsigned_abs() as f64) / 1_000_000_000.0;

    // 5. Build the event. is_bot_whale/passed_threshold/guard_skip_reason/
    // decoded_at are placeholders filled by the guard/output layers later.
    // `passed_threshold` starts `false` (conservative sentinel): the threshold
    // guard sets it definitively, and an unchecked trade must never read as
    // having passed.
    Some(DecodedTradeEvent {
        signature: result.signature.clone(),
        slot: result.slot,
        timestamp: None,
        whale_address: trader,
        dex: DexProtocol::PumpSwap,
        action,
        mint,
        sol_amount,
        token_amount,
        token_decimals: decimals,
        is_bot_whale: false,
        passed_threshold: false,
        guard_skip_reason: None,
        decoded_at: None,
        execution: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::{
        AccountKey, TokenBalance, TxEnvelope, TxInner, TxMessage, TxMeta, UiTokenAmount,
    };

    const TRADER: &str = "WhaleTrader1111111111111111111111111111111";
    const MINT: &str = "Mint1111111111111111111111111111111111111111";

    fn account_key(pubkey: &str, signer: bool) -> AccountKey {
        AccountKey {
            pubkey: pubkey.to_string(),
            signer,
            writable: true,
        }
    }

    fn token_balance(mint: &str, owner: &str, amount: &str, decimals: u8) -> TokenBalance {
        TokenBalance {
            account_index: 0,
            mint: mint.to_string(),
            owner: Some(owner.to_string()),
            ui_token_amount: UiTokenAmount {
                amount: amount.to_string(),
                decimals,
                ui_amount: None,
            },
        }
    }

    /// Build an instruction whose data is `disc` followed by arbitrary args
    /// (which the decoder ignores), base58-encoded as Helius delivers it.
    fn instruction(disc: &[u8; 8]) -> UiInstruction {
        let some_args = [0u8, 0, 0, 0, 0, 0, 0, 0, 1]; // slippage args; ignored
        let data = bs58::encode([&disc[..], &some_args].concat()).into_string();
        UiInstruction {
            program_id: Some(PROGRAM_ID.to_string()),
            accounts: Some(vec![TRADER.to_string(), MINT.to_string()]),
            data: Some(data),
            parsed: None,
            program: None,
        }
    }

    /// Assemble a `TxResult` from balance arrays and token-balance vectors.
    fn build_result(
        pre_balances: Vec<u64>,
        post_balances: Vec<u64>,
        pre_token: Vec<TokenBalance>,
        post_token: Vec<TokenBalance>,
    ) -> TxResult {
        TxResult {
            signature: "5xSignatureExample".to_string(),
            slot: 987_654_321,
            transaction: TxEnvelope {
                transaction: TxInner {
                    message: TxMessage {
                        account_keys: vec![
                            account_key(TRADER, true),
                            account_key(PROGRAM_ID, false),
                        ],
                        instructions: vec![],
                    },
                },
                meta: TxMeta {
                    err: None,
                    fee: 5000,
                    pre_balances,
                    post_balances,
                    pre_token_balances: pre_token,
                    post_token_balances: post_token,
                    inner_instructions: vec![],
                    log_messages: None,
                    loaded_addresses: None,
                },
            },
        }
    }

    #[test]
    fn decodes_buy_with_amounts_from_balance_deltas() {
        // Trader spends 3.5 SOL; token balance for MINT goes 0 -> 1_000_000.
        let result = build_result(
            vec![10_000_000_000, 0],
            vec![6_500_000_000, 0],
            vec![token_balance(MINT, TRADER, "0", 6)],
            vec![token_balance(MINT, TRADER, "1000000", 6)],
        );
        let whales: HashSet<String> = HashSet::new();

        let event = decode(&result, &instruction(&DISC_BUY), &whales).expect("buy decodes");

        assert_eq!(event.action, TradeAction::Buy);
        assert_eq!(event.dex, DexProtocol::PumpSwap);
        assert_eq!(event.whale_address, TRADER);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.token_amount, 1_000_000);
        assert_eq!(event.token_decimals, 6);
        assert_eq!(event.sol_amount, 3.5);
        assert_eq!(event.signature, "5xSignatureExample");
        assert_eq!(event.slot, 987_654_321);
        // Conservative sentinel: the guard layer sets this definitively later.
        assert!(!event.passed_threshold);
    }

    #[test]
    fn buy_uses_wsol_delta_when_native_is_only_fee() {
        // WSOL-routed AMM buy: native balance moves only by a tiny fee (-5_000),
        // but the trader's WSOL token balance drops 3.0 -> 1.0 SOL (-2 SOL). The
        // robust helper must pick the WSOL delta, not the fee-sized native delta.
        let result = build_result(
            vec![1_000_000_000, 0],
            vec![999_995_000, 0], // -5_000 lamports (fee only)
            vec![
                token_balance(MINT, TRADER, "0", 6),
                token_balance(common::WSOL_MINT, TRADER, "3000000000", 9),
            ],
            vec![
                token_balance(MINT, TRADER, "1000000", 6),
                token_balance(common::WSOL_MINT, TRADER, "1000000000", 9),
            ],
        );
        let whales: HashSet<String> = HashSet::new();

        let event = decode(&result, &instruction(&DISC_BUY), &whales).expect("buy decodes");

        assert_eq!(event.action, TradeAction::Buy);
        assert_eq!(event.dex, DexProtocol::PumpSwap);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.token_amount, 1_000_000);
        // WSOL delta (-2 SOL) dominates the fee-sized native delta (-5_000).
        assert_eq!(event.sol_amount, 2.0);
    }

    #[test]
    fn decodes_sell_with_amounts_from_balance_deltas() {
        // Trader receives 2.0 SOL; token balance for MINT goes 5_000_000 -> 1_000_000.
        let result = build_result(
            vec![1_000_000_000, 0],
            vec![3_000_000_000, 0],
            vec![token_balance(MINT, TRADER, "5000000", 6)],
            vec![token_balance(MINT, TRADER, "1000000", 6)],
        );
        let mut whales: HashSet<String> = HashSet::new();
        whales.insert(TRADER.to_string());

        let event = decode(&result, &instruction(&DISC_SELL), &whales).expect("sell decodes");

        assert_eq!(event.action, TradeAction::Sell);
        assert_eq!(event.dex, DexProtocol::PumpSwap);
        assert_eq!(event.whale_address, TRADER);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.token_amount, 4_000_000);
        assert_eq!(event.sol_amount, 2.0);
    }

    #[test]
    fn returns_none_for_non_buy_sell_discriminator() {
        // A discriminator that is neither buy nor sell must be skipped.
        let other_disc: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let result = build_result(
            vec![10_000_000_000, 0],
            vec![6_500_000_000, 0],
            vec![token_balance(MINT, TRADER, "0", 6)],
            vec![token_balance(MINT, TRADER, "1000000", 6)],
        );
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &instruction(&other_disc), &whales).is_none());
    }

    #[test]
    fn returns_none_when_trader_has_no_token_balances() {
        // Valid Buy disc, but no token-balance entries for the trader →
        // traded_mint_for_owner returns None, which must propagate.
        let result = build_result(
            vec![10_000_000_000, 0],
            vec![6_500_000_000, 0],
            vec![],
            vec![],
        );
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &instruction(&DISC_BUY), &whales).is_none());
    }

    #[test]
    fn returns_none_when_program_id_mismatches() {
        let result = build_result(vec![1, 0], vec![1, 0], vec![], vec![]);
        let mut ix = instruction(&DISC_BUY);
        ix.program_id = Some("SomeOtherProgram1111111111111111111111111111".to_string());
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &ix, &whales).is_none());
    }
}
