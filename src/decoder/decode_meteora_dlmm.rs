//! Meteora DLMM (Liquidity Book) swap decoder.
//!
//! Source: `docs/dex-reference.md` §5 (official IDL
//! `github.com/MeteoraAg/dlmm-sdk/idls/dlmm.json`, `lb_clmm` v0.12.0) and §0
//! (cross-cutting decode strategy). Per §0, the DEX is identified by program ID
//! upstream (the router), the discriminator confirms it is a swap, and amounts
//! come from `meta` balance deltas — never from instruction args, which are
//! only slippage limits.
//!
//! DIRECTION DIFFERS from PumpFun/PumpSwap: Meteora swap instruction names carry
//! no buy/sell, so direction is derived from SOL/WSOL flow (docs §0/§5): WSOL
//! leaves the trader ⇒ Buy; WSOL returns to the trader ⇒ Sell.

use std::collections::HashSet;

use crate::decoder::common;
use crate::models::types::{DecodedTradeEvent, DexProtocol, TradeAction, TxResult, UiInstruction};

/// Meteora DLMM program. Source: docs/dex-reference.md §5.
///
/// This is the DLMM (Liquidity Book) program. It MUST NOT be confused with
/// Meteora DAMM v2 (`cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`), a separate
/// constant-product AMM program (docs §5). DLMM & DAMM v2 even share the same
/// `swap` discriminator, so program-ID matching upstream is what disambiguates.
pub const PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

// SWAP discriminators (docs §5). We decode ANY of these as "this is a swap".
// Direction is NOT encoded here — it comes from WSOL flow (see `decode` step 4).
const DISC_SWAP: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
const DISC_SWAP2: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
const DISC_SWAP_EXACT_OUT: [u8; 8] = [250, 73, 101, 33, 38, 207, 75, 184];
const DISC_SWAP_EXACT_OUT2: [u8; 8] = [43, 215, 247, 132, 137, 60, 243, 81];
const DISC_SWAP_WITH_PRICE_IMPACT: [u8; 8] = [56, 173, 230, 208, 173, 228, 156, 205];
const DISC_SWAP_WITH_PRICE_IMPACT2: [u8; 8] = [74, 98, 192, 214, 177, 51, 75, 51];

/// True if `disc` is one of the six DLMM swap discriminators (docs §5).
///
/// Liquidity ops (`add_liquidity`, `remove_liquidity`, `rebalance_liquidity`,
/// etc. and their `*2` variants) are intentionally excluded — Phase 1 only
/// decodes swaps, so any other discriminator falls through to `None` in
/// `decode`.
fn is_swap(disc: &[u8; 8]) -> bool {
    matches!(
        *disc,
        DISC_SWAP
            | DISC_SWAP2
            | DISC_SWAP_EXACT_OUT
            | DISC_SWAP_EXACT_OUT2
            | DISC_SWAP_WITH_PRICE_IMPACT
            | DISC_SWAP_WITH_PRICE_IMPACT2
    )
}

// NOTE: Meteora DAMM v2 (`cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`) is a
// separate program and is out of Phase-1 scope; it would need its own decoder.

/// Decode a single instruction (already matched to `PROGRAM_ID` by the router)
/// into a trade event. Returns `None` if the instruction is not a swap, if the
/// trader can't be resolved, or if SOL/token movement can't be classified.
pub fn decode(
    result: &TxResult,
    instruction: &UiInstruction,
    whales: &HashSet<String>,
) -> Option<DecodedTradeEvent> {
    // 1. Defensive: confirm this instruction targets Meteora DLMM.
    if instruction.program_id() != Some(PROGRAM_ID) {
        return None;
    }

    // 2. Discriminator → must be a swap. Liquidity ops / anything else are skipped.
    let disc = common::discriminator(instruction.data.as_deref()?)?;
    if !is_swap(&disc) {
        return None;
    }

    // 3. Trader = tracked whale signer (falls back to first signer, see §0/common).
    let msg = &result.transaction.transaction.message;
    let (trader_idx, trader) = common::find_trader(msg, whales)?;

    // 4. DIRECTION + SOL amount from SOL/WSOL movement (§0/§5) — NOT from the
    //    discriminator. Negative = trader spent SOL/WSOL (Buy); positive =
    //    trader received SOL/WSOL (Sell). Zero net movement can't be classified
    //    (likely a token↔token swap) → out of scope.
    let sol_lamports =
        common::sol_movement_lamports(&result.transaction.meta, trader_idx, &trader)?;
    let action = if sol_lamports < 0 {
        TradeAction::Buy
    } else if sol_lamports > 0 {
        TradeAction::Sell
    } else {
        return None;
    };
    let sol_amount = (sol_lamports.unsigned_abs() as f64) / 1_000_000_000.0;

    // 5. Token mint/amount from balance deltas (§0) — the non-WSOL mint moved.
    let (mint, tok_delta, decimals) =
        common::traded_mint_for_owner(&result.transaction.meta, &trader)?;
    // Overflow-safe: a delta exceeding u64 is a non-trade artifact → skip.
    let token_amount = u64::try_from(tok_delta.unsigned_abs()).ok()?;

    // 6. Build the event. is_bot_whale/passed_threshold/guard_skip_reason/
    //    decoded_at are placeholders filled by the guard/output layers later.
    //    `passed_threshold` starts `false` (conservative sentinel): an unchecked
    //    trade must never read as having passed.
    Some(DecodedTradeEvent {
        signature: result.signature.clone(),
        slot: result.slot,
        timestamp: None,
        whale_address: trader,
        dex: DexProtocol::MeteoraDlmmV2,
        action,
        mint,
        sol_amount,
        token_amount,
        token_decimals: decimals,
        is_bot_whale: false,
        passed_threshold: false,
        guard_skip_reason: None,
        decoded_at: None,
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

    /// Instruction whose data is `disc` followed by ignored slippage args,
    /// base58-encoded as Helius delivers it.
    fn instruction(disc: &[u8; 8]) -> UiInstruction {
        let some_args = [0u8, 0, 0, 0, 0, 0, 0, 0, 1]; // amount_in/min_amount_out-ish; ignored
        let data = bs58::encode([&disc[..], &some_args].concat()).into_string();
        UiInstruction {
            program_id: Some(PROGRAM_ID.to_string()),
            accounts: Some(vec![TRADER.to_string(), MINT.to_string()]),
            data: Some(data),
            parsed: None,
            program: None,
        }
    }

    /// Assemble a `TxResult`. Native lamport arrays carry only a fee delta so
    /// the WSOL token movement is the dominant SOL signal (Meteora routes via
    /// WSOL), matching `common::sol_movement_lamports`.
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
                    // Tiny native fee; WSOL token delta dominates per common.rs.
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

        let event = decode(&result, &instruction(&DISC_SWAP), &whales).expect("buy decodes");

        assert_eq!(event.action, TradeAction::Buy);
        assert_eq!(event.dex, DexProtocol::MeteoraDlmmV2);
        assert_eq!(event.whale_address, TRADER);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.sol_amount, 2.0);
        assert_eq!(event.token_amount, 1_000_000);
        assert_eq!(event.token_decimals, 6);
        assert!(!event.passed_threshold);
    }

    #[test]
    fn decodes_sell_from_wsol_inflow() {
        // SELL: trader WSOL 1e9 -> 4e9 (+3 SOL received), token 5_000_000 -> 1_000_000.
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

        let event = decode(&result, &instruction(&DISC_SWAP), &whales).expect("sell decodes");

        assert_eq!(event.action, TradeAction::Sell);
        assert_eq!(event.mint, MINT);
        assert_eq!(event.sol_amount, 3.0);
        assert_eq!(event.token_amount, 4_000_000);
    }

    #[test]
    fn decodes_swap2_variant() {
        // A second swap variant (swap2) must also decode as a swap.
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

        assert!(decode(&result, &instruction(&DISC_SWAP2), &whales).is_some());
    }

    #[test]
    fn returns_none_for_liquidity_discriminator() {
        // add_liquidity disc (docs §5) — a non-swap op must be skipped.
        let liquidity_disc: [u8; 8] = [181, 157, 89, 67, 143, 182, 52, 72];
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

        assert!(decode(&result, &instruction(&liquidity_disc), &whales).is_none());
    }

    #[test]
    fn returns_none_when_program_id_mismatches() {
        let result = build_result(vec![], vec![]);
        let mut ix = instruction(&DISC_SWAP);
        ix.program_id = Some("SomeOtherProgram1111111111111111111111111111".to_string());
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &ix, &whales).is_none());
    }

    #[test]
    fn returns_none_on_zero_sol_movement() {
        // Token↔token swap with no WSOL balance and a zeroed native delta: the
        // swap can't be classified as buy/sell → out of scope (None).
        let mut result = build_result(
            vec![token_balance(MINT, TRADER, "1000000", 6)],
            vec![token_balance(MINT, TRADER, "2000000", 6)],
        );
        result.transaction.meta.pre_balances = vec![1_000_000_000, 0];
        result.transaction.meta.post_balances = vec![1_000_000_000, 0];
        let whales: HashSet<String> = HashSet::new();

        assert!(decode(&result, &instruction(&DISC_SWAP), &whales).is_none());
    }
}
