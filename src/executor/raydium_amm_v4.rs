//! Raydium AMM v4 swap executor.
//!
//! Builds swap instructions for the Raydium AMM v4 program (constant-product AMM).
//!
//! **Program ID:** `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`
//!
//! Strategy: copy ALL accounts from the whale's decoded instruction (via
//! `params.ix_accounts`) and substitute only the user-specific accounts
//! (user_source_token_account, user_dest_token_account, user_owner).
//!
//! # Account Layout (18 total)
//!
//! | idx | account                       | substitute? |
//! |-----|-------------------------------|-------------|
//! |  0  | token_program                 | no          |
//! |  1  | amm (pool state)              | no          |
//! |  2  | amm_authority (PDA)           | no          |
//! |  3  | amm_open_orders               | no          |
//! |  4  | amm_target_orders             | no          |
//! |  5  | pool_coin_token_account       | no          |
//! |  6  | pool_pc_token_account         | no          |
//! |  7  | serum_program                 | no          |
//! |  8  | serum_market                  | no          |
//! |  9  | serum_bids                    | no          |
//! | 10  | serum_asks                    | no          |
//! | 11  | serum_event_queue             | no          |
//! | 12  | serum_coin_vault              | no          |
//! | 13  | serum_pc_vault                | no          |
//! | 14  | serum_vault_signer            | no          |
//! | 15  | user_source_token_account     | YES         |
//! | 16  | user_dest_token_account       | YES         |
//! | 17  | user_owner (signer)           | YES         |

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// Raydium AMM v4 program ID.
const PROGRAM_ID: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// Minimum expected accounts for Raydium AMM v4 swap.
const MIN_ACCOUNTS: usize = 18;

/// Account indices that must be substituted with our own.
const IDX_USER_SOURCE: usize = 15;
const IDX_USER_DEST: usize = 16;
const IDX_USER_OWNER: usize = 17;

/// Raydium AMM v4 executor.
pub struct RaydiumAmmV4Executor {
    program_id: Pubkey,
}

impl RaydiumAmmV4Executor {
    pub fn new() -> Self {
        Self {
            program_id: PROGRAM_ID.parse().expect("valid Raydium AMM v4 program ID"),
        }
    }

    /// Build instruction accounts by taking the whale's accounts and
    /// substituting user-specific entries.
    fn build_accounts_from_whale(
        &self,
        payer: &Pubkey,
        base_mint: &Pubkey,
        quote_mint: &Pubkey,
        whale_accounts: &[Pubkey],
    ) -> Result<Vec<AccountMeta>, ExecutorError> {
        if whale_accounts.len() < MIN_ACCOUNTS {
            return Err(ExecutorError::InvalidAccount(format!(
                "Raydium AMM v4 whale accounts too short: {} (need >= {})",
                whale_accounts.len(),
                MIN_ACCOUNTS,
            )));
        }

        // Derive user ATAs
        let user_source =
            spl_associated_token_account::get_associated_token_address(payer, base_mint);
        let user_dest =
            spl_associated_token_account::get_associated_token_address(payer, quote_mint);

        // Clone whale accounts and substitute user-specific ones
        let mut accounts: Vec<AccountMeta> = whale_accounts
            .iter()
            .enumerate()
            .map(|(i, pk)| {
                let (writable, signer) = match i {
                    1 | 3 | 4 | 5 | 6 | 8 | 9 | 10 | 11 | 12 | 13 | 15 | 16 => (true, false),
                    17 => (true, true), // user owner (signer)
                    _ => (false, false),
                };
                AccountMeta {
                    pubkey: *pk,
                    is_signer: signer,
                    is_writable: writable,
                }
            })
            .collect();

        // Substitute user-specific accounts
        accounts[IDX_USER_SOURCE].pubkey = user_source;
        accounts[IDX_USER_DEST].pubkey = user_dest;
        accounts[IDX_USER_OWNER].pubkey = *payer;

        Ok(accounts)
    }
}

impl DexExecutor for RaydiumAmmV4Executor {
    fn protocol_name(&self) -> &str {
        "Raydium AMM v4"
    }

    fn build_buy_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let whale_accounts = params.ix_accounts.as_ref().ok_or_else(|| {
            ExecutorError::PoolNotFound(
                "Raydium AMM v4 buy requires ix_accounts from whale tx".into(),
            )
        })?;

        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        // For buys: source = WSOL, dest = token
        let accounts =
            self.build_accounts_from_whale(payer, &quote_mint, &params.mint, whale_accounts)?;

        // amount_in = SOL with slippage, min_amount_out = 0 (any)
        let amount_in = tx_utils::max_input_after_slippage(params.amount, params.slippage_bps);

        let mut data = Vec::with_capacity(17);
        data.push(9u8); // swap instruction type
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // min_amount_out = 0

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Create ATA for token (idempotent)
        let mut ixs = Vec::with_capacity(2);
        ixs.push(tx_utils::create_ata_idempotent_ix(
            payer,
            payer,
            &params.mint,
            &spl_token::id(),
        ));
        ixs.push(ix);

        Ok((ixs, 0))
    }

    fn build_sell_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let whale_accounts = params.ix_accounts.as_ref().ok_or_else(|| {
            ExecutorError::PoolNotFound(
                "Raydium AMM v4 sell requires ix_accounts from whale tx".into(),
            )
        })?;

        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        // For sells: source = token, dest = WSOL
        let accounts =
            self.build_accounts_from_whale(payer, &params.mint, &quote_mint, whale_accounts)?;

        let min_amount_out = tx_utils::min_output_after_slippage(0, params.slippage_bps);

        let mut data = Vec::with_capacity(17);
        data.push(9u8); // swap instruction type
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], min_amount_out))
    }
}
