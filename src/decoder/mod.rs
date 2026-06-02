//! Transaction decoders per DEX/launchpad program.

pub mod common;
pub mod decode_meteora_dlmm;
pub mod decode_pumpfun;
pub mod decode_pumpswap;
pub mod decode_raydium_launchpad;

use std::collections::HashSet;

use crate::models::types::{DecodedTradeEvent, TxResult, UiInstruction};

/// Route every instruction (top-level and inner/CPI) to the decoder whose PROGRAM_ID matches the
/// instruction's program id. Returns the first successfully-decoded trade event, or None if no DEX
/// instruction in the transaction decodes. Disambiguation is by PROGRAM_ID (PumpFun/PumpSwap and
/// DLMM/DAMM share discriminators) — see docs/dex-reference.md §0.
// Single-event routing API retained for callers/tests; Phase 1 production path uses `route_all`.
#[allow(dead_code)]
pub fn route(result: &TxResult, whales: &HashSet<String>) -> Option<DecodedTradeEvent> {
    all_instructions(result)
        .into_iter()
        .find_map(|instr| dispatch(result, instr, whales))
}

/// Decode EVERY DEX instruction in the transaction (top-level + inner/CPI), not just the first.
/// Used by the bot filter to detect multi-swap / buy+sell-same-tx arbitrage patterns. Reuses the
/// same instruction-collection and dispatch logic as `route`.
pub fn route_all(result: &TxResult, whales: &HashSet<String>) -> Vec<DecodedTradeEvent> {
    all_instructions(result)
        .into_iter()
        .filter_map(|instr| dispatch(result, instr, whales))
        .collect()
}

/// Collect references to every instruction: top-level `message.instructions` first, then each
/// `meta.inner_instructions[].instructions` (CPIs), preserving order.
fn all_instructions(result: &TxResult) -> Vec<&UiInstruction> {
    let mut instructions: Vec<&UiInstruction> = result
        .transaction
        .transaction
        .message
        .instructions
        .iter()
        .collect();

    for inner in &result.transaction.meta.inner_instructions {
        instructions.extend(inner.instructions.iter());
    }

    instructions
}

/// Dispatch a single instruction to the decoder whose PROGRAM_ID matches its program id. Unknown
/// program ids (and instructions with no program id) yield `None`.
fn dispatch(
    result: &TxResult,
    instruction: &UiInstruction,
    whales: &HashSet<String>,
) -> Option<DecodedTradeEvent> {
    match instruction.program_id()? {
        decode_pumpfun::PROGRAM_ID => decode_pumpfun::decode(result, instruction, whales),
        decode_pumpswap::PROGRAM_ID => decode_pumpswap::decode(result, instruction, whales),
        decode_raydium_launchpad::PROGRAM_ID => {
            decode_raydium_launchpad::decode(result, instruction, whales)
        }
        decode_meteora_dlmm::PROGRAM_ID => decode_meteora_dlmm::decode(result, instruction, whales),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::{
        AccountKey, DexProtocol, InnerInstructions, TokenBalance, TradeAction, TxEnvelope, TxInner,
        TxMessage, TxMeta, UiTokenAmount,
    };

    const TRADER: &str = "WhaleTrader1111111111111111111111111111111";
    const MINT: &str = "Mint1111111111111111111111111111111111111111";
    // Shared buy/sell bytes across PumpFun and PumpSwap (docs §0/§3).
    const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];

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

    /// A buy instruction for `program_id` (data = buy discriminator + ignored args).
    fn buy_instruction(program_id: &str) -> UiInstruction {
        let args = [0u8, 0, 0, 0, 0, 0, 0, 0, 1];
        let data = bs58::encode([&DISC_BUY[..], &args].concat()).into_string();
        UiInstruction {
            program_id: Some(program_id.to_string()),
            accounts: Some(vec![TRADER.to_string(), MINT.to_string()]),
            data: Some(data),
            parsed: None,
            program: None,
        }
    }

    /// Build a `TxResult` from explicit top-level + inner instructions and a buy-shaped balance set
    /// (3.5 SOL spent, MINT 0 -> 1_000_000) so DEX instructions decode.
    fn build_result(top_level: Vec<UiInstruction>, inner: Vec<UiInstruction>) -> TxResult {
        let inner_instructions = if inner.is_empty() {
            vec![]
        } else {
            vec![InnerInstructions {
                index: 0,
                instructions: inner,
            }]
        };

        TxResult {
            signature: "5xSignatureExample".to_string(),
            slot: 987_654_321,
            transaction: TxEnvelope {
                transaction: TxInner {
                    message: TxMessage {
                        account_keys: vec![account_key(TRADER, true)],
                        instructions: top_level,
                    },
                },
                meta: TxMeta {
                    err: None,
                    fee: 5000,
                    pre_balances: vec![10_000_000_000],
                    post_balances: vec![6_500_000_000],
                    pre_token_balances: vec![token_balance(MINT, TRADER, "0", 6)],
                    post_token_balances: vec![token_balance(MINT, TRADER, "1000000", 6)],
                    inner_instructions,
                    log_messages: None,
                    loaded_addresses: None,
                },
            },
        }
    }

    #[test]
    fn routes_top_level_pumpfun_buy() {
        let result = build_result(vec![buy_instruction(decode_pumpfun::PROGRAM_ID)], vec![]);
        let whales: HashSet<String> = HashSet::new();

        let event = route(&result, &whales).expect("pumpfun buy routes");

        assert_eq!(event.dex, DexProtocol::PumpFun);
        assert_eq!(event.action, TradeAction::Buy);
        assert_eq!(event.mint, MINT);
    }

    #[test]
    fn returns_none_for_unknown_program_id() {
        let mut ix = buy_instruction(decode_pumpfun::PROGRAM_ID);
        ix.program_id = Some("UnknownProgram11111111111111111111111111111".to_string());
        let result = build_result(vec![ix], vec![]);
        let whales: HashSet<String> = HashSet::new();

        assert!(route(&result, &whales).is_none());
    }

    #[test]
    fn finds_dex_instruction_inside_inner_cpi() {
        // No top-level DEX ix; the PumpFun buy lives only in inner_instructions (a CPI).
        let result = build_result(vec![], vec![buy_instruction(decode_pumpfun::PROGRAM_ID)]);
        let whales: HashSet<String> = HashSet::new();

        let event = route(&result, &whales).expect("inner CPI buy routes");

        assert_eq!(event.dex, DexProtocol::PumpFun);
        assert_eq!(event.action, TradeAction::Buy);
    }

    #[test]
    fn disambiguates_pumpswap_from_pumpfun_by_program_id() {
        // Same shared buy discriminator, but programId == PumpSwap → must route to PumpSwap.
        let result = build_result(vec![buy_instruction(decode_pumpswap::PROGRAM_ID)], vec![]);
        let whales: HashSet<String> = HashSet::new();

        let event = route(&result, &whales).expect("pumpswap buy routes");

        assert_eq!(event.dex, DexProtocol::PumpSwap);
        assert_ne!(event.dex, DexProtocol::PumpFun);
    }

    #[test]
    fn returns_none_for_empty_transaction() {
        let result = build_result(vec![], vec![]);
        let whales: HashSet<String> = HashSet::new();

        assert!(route(&result, &whales).is_none());
    }

    #[test]
    fn route_all_collects_top_level_and_inner_dex_instructions() {
        // One PumpFun buy top-level + another PumpFun buy in an inner CPI → both decode.
        let result = build_result(
            vec![buy_instruction(decode_pumpfun::PROGRAM_ID)],
            vec![buy_instruction(decode_pumpfun::PROGRAM_ID)],
        );
        let whales: HashSet<String> = HashSet::new();

        let events = route_all(&result, &whales);

        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.dex == DexProtocol::PumpFun));
        assert!(events.iter().all(|e| e.action == TradeAction::Buy));
    }

    #[test]
    fn route_all_returns_empty_for_no_dex_transaction() {
        let result = build_result(vec![], vec![]);
        let whales: HashSet<String> = HashSet::new();

        assert!(route_all(&result, &whales).is_empty());
    }
}
