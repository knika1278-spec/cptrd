//! Raydium AMM v4 swap executor.
//!
//! Builds swap instructions for the Raydium AMM v4 program (constant-product AMM).
//!
//! **Program ID:** `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`
//!
//! This is the main Raydium AMM — NOT the Launchpad/LaunchLab, CLMM, or CPMM.
//!
//! # Swap Instruction (legacy format, type = 9)
//!
//! - Byte 0: instruction type (`9` = swap)
//! - Bytes 1-8: `amount_in` (u64 LE)
//! - Bytes 9-16: `minimum_amount_out` (u64 LE)
//!
//! Direction is determined by which token the user sends vs receives.
//! The `user_source_token_account` and `user_dest_token_account` accounts
//! indicate the direction.
//!
//! # Accounts (18 total, legacy Serum DEX integration)
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

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{DexExecutor, ExecutorError, TradeParams};

/// Raydium AMM v4 program ID.
const PROGRAM_ID: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// Raydium AMM Authority (PDA derived from the AMM program).
#[allow(dead_code)]
const AMM_AUTHORITY: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";

/// Serum DEX v3 program (used by Raydium AMM v4 for orderbook).
#[allow(dead_code)]
const SERUM_PROGRAM: &str = "srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX";

/// Raydium AMM v4 executor.
#[allow(dead_code)]
pub struct RaydiumAmmV4Executor {
    program_id: Pubkey,
}

#[allow(dead_code)]
impl RaydiumAmmV4Executor {
    pub fn new() -> Self {
        Self {
            program_id: PROGRAM_ID.parse().expect("valid Raydium AMM v4 program ID"),
        }
    }

    /// Derive the AMM authority PDA.
    fn derive_authority(&self) -> Pubkey {
        AMM_AUTHORITY.parse().unwrap()
    }

    /// Build a swap instruction.
    ///
    /// For Raydium AMM v4, the direction is implicit from the source/dest accounts.
    /// `source_mint` = token being sold, `dest_mint` = token being bought.
    fn build_swap_ix(
        &self,
        payer: &Pubkey,
        source_mint: &Pubkey,
        dest_mint: &Pubkey,
        amount_in: u64,
        min_amount_out: u64,
        amm_account: &Pubkey,
        open_orders: &Pubkey,
        target_orders: &Pubkey,
        pool_coin_account: &Pubkey,
        pool_pc_account: &Pubkey,
        serum_market: &Pubkey,
        serum_bids: &Pubkey,
        serum_asks: &Pubkey,
        serum_event_queue: &Pubkey,
        serum_coin_vault: &Pubkey,
        serum_pc_vault: &Pubkey,
        serum_vault_signer: &Pubkey,
    ) -> Instruction {
        let authority = self.derive_authority();
        let user_source = spl_associated_token_account::get_associated_token_address(payer, source_mint);
        let user_dest = spl_associated_token_account::get_associated_token_address(payer, dest_mint);
        let token_program = spl_token::id();
        let serum_program: Pubkey = SERUM_PROGRAM.parse().unwrap();

        let accounts = vec![
            AccountMeta::new_readonly(token_program, false),     // 0
            AccountMeta::new(*amm_account, false),               // 1
            AccountMeta::new_readonly(authority, false),         // 2
            AccountMeta::new(*open_orders, false),               // 3
            AccountMeta::new(*target_orders, false),             // 4
            AccountMeta::new(*pool_coin_account, false),         // 5
            AccountMeta::new(*pool_pc_account, false),           // 6
            AccountMeta::new_readonly(serum_program, false),     // 7
            AccountMeta::new(*serum_market, false),              // 8
            AccountMeta::new(*serum_bids, false),                // 9
            AccountMeta::new(*serum_asks, false),                // 10
            AccountMeta::new(*serum_event_queue, false),         // 11
            AccountMeta::new(*serum_coin_vault, false),          // 12
            AccountMeta::new(*serum_pc_vault, false),            // 13
            AccountMeta::new_readonly(*serum_vault_signer, false), // 14
            AccountMeta::new(user_source, false),                // 15
            AccountMeta::new(user_dest, false),                  // 16
            AccountMeta::new(*payer, true),                      // 17
        ];

        // Instruction data: type(1) + amount_in(8) + min_amount_out(8)
        let mut data = Vec::with_capacity(17);
        data.push(9u8); // swap instruction type
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Instruction::new_with_bytes(self.program_id, &data, accounts)
    }
}

impl DexExecutor for RaydiumAmmV4Executor {
    fn protocol_name(&self) -> &str {
        "Raydium AMM v4"
    }

    fn build_buy_ixs(
        &self,
        _payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        // For buys: spend WSOL to get the token.
        // In Raydium AMM terms: source = WSOL (pc), dest = token (coin).
        //
        // NOTE: In production, the AMM account and all its sub-accounts (open_orders,
        // target_orders, pool_coin, pool_pc, serum_market, etc.) must be resolved
        // on-chain. For now, this executor returns an error indicating that pool
        // discovery is needed — the execute_copy_trade orchestrator will fall through
        // to the next executor.
        //
        // TODO: Implement on-chain AMM account resolution via getAccountInfo or
        // a Raydium pool indexer API.
        let _wsol_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        Err(ExecutorError::PoolNotFound(format!(
            "Raydium AMM v4 pool discovery not yet implemented for mint {}",
            params.mint
        )))
    }

    fn build_sell_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        // Same as buy — pool discovery needed first.
        let _ = (payer, params);
        Err(ExecutorError::PoolNotFound(format!(
            "Raydium AMM v4 pool discovery not yet implemented for mint {}",
            params.mint
        )))
    }
}
