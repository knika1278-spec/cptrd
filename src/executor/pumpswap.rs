#![allow(deprecated)]
//! PumpSwap AMM trade executor.
//!
//! Builds buy/sell instructions for the PumpSwap AMM program.
//!
//! Strategy: copy ALL accounts from the whale's decoded instruction (via
//! `params.ix_accounts`) and substitute only the user-specific accounts
//! (user wallet, user ATAs, user_volume_accumulator). This avoids fragile
//! hardcoded addresses that break when the IDL evolves.
//!
//! Source: official IDL `github.com/pump-fun/pump-public-docs/idl/pump_amm.json`
//! and `docs/dex-reference.md` §3.
//!
//! # Account Layout (from IDL, 2026-06-03)
//!
//! | idx | account                                | writable | substitute? |
//! |-----|----------------------------------------|----------|-------------|
//! |  0  | pool                                   | yes      | no          |
//! |  1  | user (trader)                          | yes      | YES         |
//! |  2  | global_config                          | no       | no          |
//! |  3  | base_mint (token)                      | no       | no          |
//! |  4  | quote_mint (WSOL)                      | no       | no          |
//! |  5  | user_base_token_account                | yes      | YES         |
//! |  6  | user_quote_token_account               | yes      | YES         |
//! |  7  | pool_base_token_account                | yes      | no          |
//! |  8  | pool_quote_token_account               | yes      | no          |
//! |  9  | protocol_fee_recipient                 | no       | no          |
//! | 10  | protocol_fee_recipient_token_account   | yes      | no          |
//! | 11  | base_token_program                     | no       | no          |
//! | 12  | quote_token_program                    | no       | no          |
//! | 13  | system_program                         | no       | no          |
//! | 14  | associated_token_program               | no       | no          |
//! | 15  | event_authority                        | no       | no          |
//! | 16  | program (self)                         | no       | no          |
//! | 17  | coin_creator_vault_ata                 | yes      | no          |
//! | 18  | coin_creator_vault_authority           | no       | no          |
//! | 19  | global_volume_accumulator (buy only)   | no       | no          |
//! | 20  | user_volume_accumulator (buy only)     | yes      | YES         |
//! | 21  | fee_config (buy idx 21 / sell idx 19)  | no       | no          |
//! | 22  | fee_program  (buy idx 22 / sell idx 20)| no       | no          |

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// PumpSwap AMM program ID.
const PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

// Discriminators — IDENTICAL to PumpFun; disambiguate by program ID.
const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const DISC_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Minimum expected accounts for a PumpSwap instruction (sell has fewer).
const MIN_ACCOUNTS: usize = 19;

/// Account indices that must be substituted with our own.
const IDX_USER: usize = 1;
const IDX_USER_BASE_ATA: usize = 5;
const IDX_USER_QUOTE_ATA: usize = 6;
/// User volume accumulator is only in buy (index 20). Sell doesn't have it.
const IDX_USER_VOLUME_ACCUMULATOR_BUY: usize = 20;

/// PumpSwap AMM executor.
pub struct PumpSwapExecutor {
    program_id: Pubkey,
}

impl PumpSwapExecutor {
    pub fn new() -> Self {
        Self {
            program_id: PROGRAM_ID.parse().expect("valid PumpSwap program ID"),
        }
    }

    /// Build the instruction accounts by taking the whale's accounts and
    /// substituting user-specific entries (user wallet, user ATAs, user vol).
    fn build_accounts_from_whale(
        &self,
        payer: &Pubkey,
        base_mint: &Pubkey,
        whale_accounts: &[Pubkey],
        is_buy: bool,
    ) -> Result<Vec<AccountMeta>, ExecutorError> {
        if whale_accounts.len() < MIN_ACCOUNTS {
            return Err(ExecutorError::InvalidAccount(format!(
                "PumpSwap whale accounts too short: {} (need >= {})",
                whale_accounts.len(),
                MIN_ACCOUNTS,
            )));
        }

        // Derive our user-specific accounts
        let user_base_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                payer,
                base_mint,
                &tx_utils::token_2022_program_id(),
            );
        let quote_mint = whale_accounts[4]; // WSOL
        let user_quote_ata =
            spl_associated_token_account::get_associated_token_address(payer, &quote_mint);

        // Clone the whale accounts and substitute user-specific ones
        let mut accounts: Vec<AccountMeta> = whale_accounts
            .iter()
            .enumerate()
            .map(|(i, pk)| {
                // Determine writable/signer based on the known layout
                let (writable, signer) = match i {
                    0 | 5 | 6 | 7 | 8 | 10 | 17 => (true, false), // pool, user ATAs, pool ATAs, fee_recip_ata, creator_vault_ata
                    1 => (true, true),                            // user (signer)
                    _ if is_buy && i == 20 => (true, false),      // user_volume_accumulator (buy)
                    _ => (false, false),                          // everything else is readonly
                };
                AccountMeta {
                    pubkey: *pk,
                    is_signer: signer,
                    is_writable: writable,
                }
            })
            .collect();

        // Substitute user-specific accounts
        accounts[IDX_USER].pubkey = *payer;
        accounts[IDX_USER_BASE_ATA].pubkey = user_base_ata;
        accounts[IDX_USER_QUOTE_ATA].pubkey = user_quote_ata;

        // For buy: substitute user_volume_accumulator
        if is_buy && whale_accounts.len() > IDX_USER_VOLUME_ACCUMULATOR_BUY {
            let user_vol = Pubkey::find_program_address(
                &[b"user_volume_accumulator", payer.as_ref()],
                &self.program_id,
            )
            .0;
            accounts[IDX_USER_VOLUME_ACCUMULATOR_BUY].pubkey = user_vol;
        }

        Ok(accounts)
    }
}

impl DexExecutor for PumpSwapExecutor {
    fn protocol_name(&self) -> &str {
        "PumpSwap"
    }

    fn build_buy_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let whale_accounts = params.ix_accounts.as_ref().ok_or_else(|| {
            ExecutorError::PoolNotFound("PumpSwap buy requires ix_accounts from whale tx".into())
        })?;

        let accounts = self.build_accounts_from_whale(payer, &params.mint, whale_accounts, true)?;

        // For buys: base_amount_out = 0 (any), max_quote_amount_in = SOL with slippage
        let max_quote_in = tx_utils::max_input_after_slippage(params.amount, params.slippage_bps);

        let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
        data.extend_from_slice(&DISC_BUY);
        data.extend_from_slice(&0u64.to_le_bytes()); // base_amount_out
        data.extend_from_slice(&max_quote_in.to_le_bytes());
        data.push(1); // track_volume

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Create ATA (idempotent — no-op if exists)
        let mut ixs = Vec::with_capacity(2);
        ixs.push(tx_utils::create_ata_idempotent_ix(
            payer,
            payer,
            &params.mint,
            &tx_utils::token_2022_program_id(),
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
            ExecutorError::PoolNotFound("PumpSwap sell requires ix_accounts from whale tx".into())
        })?;

        let accounts =
            self.build_accounts_from_whale(payer, &params.mint, whale_accounts, false)?;

        let min_quote_out = tx_utils::min_output_after_slippage(0, params.slippage_bps);

        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&DISC_SELL);
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&min_quote_out.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], min_quote_out))
    }
}
