//! Raydium AMM v4 swap decoder.
//!
//! Source: Raydium AMM v4 program — the main constant-product AMM on Solana.
//! This is NOT the Launchpad/LaunchLab (those are separate programs).
//!
//! **Program ID:** `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`
//!
//! # Swap Instruction (type = 9)
//!
//! Raydium AMM v4 uses a legacy (non-Anchor) instruction format:
//! - Byte 0: instruction type (`9` = swap)
//! - Bytes 1-8: `amount_in` (u64 LE)
//! - Bytes 9-16: `minimum_amount_out` (u64 LE)
//!
//! Direction is NOT encoded in the instruction — it's determined by which
//! token the user sends vs receives. We derive direction from SOL/WSOL
//! balance deltas (same strategy as Meteora DLMM, per `docs/dex-reference.md` §0):
//! - WSOL leaves trader ⇒ Buy (spend SOL to get token)
//! - WSOL returns to trader ⇒ Sell (sell token to get SOL)
//!
//! # Accounts (17 total, legacy Serum DEX integration)
//!
//! | idx | account |
//! |-----|---------|
//! | 0  | token_program |
//! | 1  | amm (pool state) |
//! | 2  | amm_authority (PDA) |
//! | 3  | amm_open_orders |
//! | 4  | amm_target_orders |
//! | 5  | pool_coin_token_account |
//! | 6  | pool_pc_token_account |
//! | 7  | serum_program |
//! | 8  | serum_market |
//! | 9  | serum_bids |
//! | 10 | serum_asks |
//! | 11 | serum_event_queue |
//! | 12 | serum_coin_vault |
//! | 13 | serum_pc_vault |
//! | 14 | serum_vault_signer |
//! | 15 | user_source_token_account |
//! | 16 | user_dest_token_account |
//! | 17 | user_owner (signer) |

use std::collections::HashSet;

use crate::decoder::common;
use crate::models::types::{DecodedTradeEvent, DexProtocol, TradeAction, TxResult, UiInstruction};

/// Raydium AMM v4 program ID.
pub const PROGRAM_ID: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// Swap instruction type (legacy format, byte 0).
const IX_TYPE_SWAP: u8 = 9;

/// `user_owner` (trader, signer) account index in the swap instruction.
const TRADER_ACCOUNT_INDEX: usize = 17;

/// Decode a single instruction (already matched to `PROGRAM_ID` by the router)
/// into a trade event. Returns `None` if the instruction is not a swap, if the
/// trader can't be resolved, or if SOL/token movement can't be classified.
pub fn decode(
    result: &TxResult,
    instruction: &UiInstruction,
    whales: &HashSet<String>,
) -> Option<DecodedTradeEvent> {
    // 1. Defensive: confirm this instruction targets Raydium AMM v4.
    if instruction.program_id() != Some(PROGRAM_ID) {
        return None;
    }

    // 2. Check instruction type (first byte = 9 for swap).
    let data_bytes = common::discriminator(instruction.data.as_deref()?)?;
    let ix_type = data_bytes[0];
    if ix_type != IX_TYPE_SWAP {
        return None;
    }

    // 3. Trader = tracked whale signer, cross-checked against the DEX trader-index account.
    let (trader_idx, trader) =
        common::resolve_trader(result, instruction, whales, TRADER_ACCOUNT_INDEX)?;

    // 4. DIRECTION + SOL amount from SOL/WSOL movement (§0).
    //    Same strategy as Meteora DLMM: WSOL outflow = Buy, WSOL inflow = Sell.
    let sol_lamports =
        common::sol_movement_lamports(&result.transaction.meta, trader_idx, &trader)?;
    let action = if sol_lamports < 0 {
        TradeAction::Buy
    } else if sol_lamports > 0 {
        TradeAction::Sell
    } else {
        return None; // Token↔token swap, out of scope
    };
    let sol_amount = (sol_lamports.unsigned_abs() as f64) / 1_000_000_000.0;

    // 5. Token mint/amount from balance deltas (§0) — the non-WSOL mint moved.
    let (mint, tok_delta, decimals) =
        common::traded_mint_for_owner(&result.transaction.meta, &trader)?;
    let token_amount = u64::try_from(tok_delta.unsigned_abs()).ok()?;

    // 6. Build the event.
    Some(DecodedTradeEvent {
        signature: result.signature.clone(),
        slot: result.slot,
        timestamp: None,
        whale_address: trader,
        dex: DexProtocol::RaydiumAmmV4,
        action,
        mint,
        sol_amount,
        token_amount,
        token_decimals: decimals,
        is_bot_whale: false,
        passed_threshold: false,
        guard_skip_reason: None,
        decoded_at: None,
        ix_accounts: Some(instruction.accounts.clone().unwrap_or_default()),
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
    const WSOL: &str = common::WSOL_MINT;

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

    /// Build a swap instruction with type=9 + amount_in + min_amount_out.
    fn swap_instruction() -> UiInstruction {
        let mut data = vec![9u8]; // ix type = swap
        data.extend_from_slice(&1_000_000u64.to_le_bytes()); // amount_in
        data.extend_from_slice(&0u64.to_le_bytes()); // min_amount_out
        let encoded = bs58::encode(&data).into_string();
        UiInstruction {
            program_id: Some(PROGRAM_ID.to_string()),
            accounts: Some(vec![TRADER.to_string()]),
            data: Some(encoded),
            parsed: None,
            program: None,
        }
    }

    /// Build a non-swap instruction (type != 9).
    fn non_swap_instruction() -> UiInstruction {
        let data = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let encoded = bs58::encode(&data).into_string();
        UiInstruction {
            program_id: Some(PROGRAM_ID.to_string()),
            accounts: Some(vec![TRADER.to_string()]),
            data: Some(encoded),
            parsed: None,
            program: None,
        }
    }

    /// Assemble a `TxResult` with WSOL-based direction signal.
    fn build_result(pre_token: Vec<TokenBalance>, post_token: Vec<TokenBalance>) -> TxResult {
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
                    pre_balances: vec![1_000_000_000, 0],
                    post_balances: vec![999_995_000, 0],
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
    fn decodes_buy_from_wsol_outflow() {
        // BUY: trader WSOL 5e9 -> 3e9 (-2 SOL), token 0 -> 1_000_000.
        let result = build_result(
            vec![
                token_balance(WSOL, TRADER, "5000000000", 9),
                token_balance(MINT, TRADER, "0", 6),
            ],
            vec![
                token_balance(WSOL, TRADER, "3000000000", 9),
                token_balance(MINT, TRADER, "1000000", 6),
            ],
        );
        let whales: HashSet<String> = HashSet::new();

        let event = decode(&result, &swap_instruction(), &whales).expect("buy decodes");

        assert_eq!(event.action, TradeAction::Buy);
        assert_eq!(event.dex, DexProtocol::RaydiumAmmV4);
        assert_eq!(event.whale_address, TRADER);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.sol_amount, 2.0);
        assert_eq!(event.token_amount, 1_000_000);
    }

    #[test]
    fn decodes_sell_from_wsol_inflow() {
        // SELL: trader WSOL 1e9 -> 4e9 (+3 SOL), token 5_000_000 -> 1_000_000.
        let result = build_result(
            vec![
                token_balance(WSOL, TRADER, "1000000000", 9),
                token_balance(MINT, TRADER, "5000000", 6),
            ],
            vec![
                token_balance(WSOL, TRADER, "4000000000", 9),
                token_balance(MINT, TRADER, "1000000", 6),
            ],
        );
        let mut whales: HashSet<String> = HashSet::new();
        whales.insert(TRADER.to_string());

        let event = decode(&result, &swap_instruction(), &whales).expect("sell decodes");

        assert_eq!(event.action, TradeAction::Sell);
        assert_eq!(event.dex, DexProtocol::RaydiumAmmV4);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.sol_amount, 3.0);
        assert_eq!(event.token_amount, 4_000_000);
    }

    #[test]
    fn returns_none_for_non_swap_instruction_type() {
        let result = build_result(
            vec![
                token_balance(WSOL, TRADER, "5000000000", 9),
                token_balance(MINT, TRADER, "0", 6),
            ],
            vec![
                token_balance(WSOL, TRADER, "3000000000", 9),
                token_balance(MINT, TRADER, "1000000", 6),
            ],
        );
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &non_swap_instruction(), &whales).is_none());
    }

    #[test]
    fn returns_none_on_zero_sol_movement() {
        let mut result = build_result(
            vec![token_balance(MINT, TRADER, "1000000", 6)],
            vec![token_balance(MINT, TRADER, "2000000", 6)],
        );
        result.transaction.meta.pre_balances = vec![1_000_000_000, 0];
        result.transaction.meta.post_balances = vec![1_000_000_000, 0];
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &swap_instruction(), &whales).is_none());
    }

    #[test]
    fn returns_none_when_program_id_mismatches() {
        let result = build_result(vec![], vec![]);
        let mut ix = swap_instruction();
        ix.program_id = Some("SomeOtherProgram1111111111111111111111111111".to_string());
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &ix, &whales).is_none());
    }
}
