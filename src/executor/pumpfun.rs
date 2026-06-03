#![allow(deprecated)]
//! PumpFun bonding-curve trade executor.
//!
//! Builds buy/sell instructions for the PumpFun bonding curve program.
//!
//! Strategy: copy ALL accounts from the whale's decoded instruction (via
//! `params.ix_accounts`) and substitute only the user-specific accounts
//! (user wallet, user ATA, user_volume_accumulator).
//!
//! Source: official IDL `github.com/pump-fun/pump-public-docs/idl/pump.json`
//! and `docs/dex-reference.md` §2.
//!
//! # Account Layout (from IDL, 2026-06-03)
//!
//! ## Buy (16 accounts)
//! | idx | account                  | writable | substitute? |
//! |-----|--------------------------|----------|-------------|
//! |  0  | global                   | no       | no          |
//! |  1  | fee_recipient            | yes      | no          |
//! |  2  | mint                     | no       | no          |
//! |  3  | bonding_curve            | yes      | no          |
//! |  4  | associated_bonding_curve | yes      | no          |
//! |  5  | associated_user (ATA)    | yes      | YES         |
//! |  6  | user (trader)            | yes      | YES         |
//! |  7  | system_program           | no       | no          |
//! |  8  | token_program            | no       | no          |
//! |  9  | creator_vault            | yes      | no          |
//! | 10  | event_authority          | no       | no          |
//! | 11  | program                  | no       | no          |
//! | 12  | global_volume_accumulator| no       | no          |
//! | 13  | user_volume_accumulator  | yes      | YES         |
//! | 14  | fee_config               | no       | no          |
//! | 15  | fee_program              | no       | no          |
//!
//! ## Sell (14 accounts)
//! | idx | account                  | writable | substitute? |
//! |-----|--------------------------|----------|-------------|
//! |  0  | global                   | no       | no          |
//! |  1  | fee_recipient            | yes      | no          |
//! |  2  | mint                     | no       | no          |
//! |  3  | bonding_curve            | yes      | no          |
//! |  4  | associated_bonding_curve | yes      | no          |
//! |  5  | associated_user (ATA)    | yes      | YES         |
//! |  6  | user (trader)            | yes      | YES         |
//! |  7  | system_program           | no       | no          |
//! |  8  | creator_vault            | yes      | no          |
//! |  9  | token_program            | no       | no          |
//! | 10  | event_authority          | no       | no          |
//! | 11  | program                  | no       | no          |
//! | 12  | fee_config               | no       | no          |
//! | 13  | fee_program              | no       | no         

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// PumpFun bonding-curve program ID.
const PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

// Discriminators = sha256("global:<name>")[..8]
const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const DISC_BUY_EXACT_SOL_IN: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];
const DISC_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Minimum expected accounts for a PumpFun instruction (sell has 14).
const MIN_ACCOUNTS: usize = 12;

/// Account indices that must be substituted with our own.
const IDX_ASSOCIATED_USER: usize = 5;
const IDX_USER: usize = 6;
/// User volume accumulator is only in buy (index 13).
const IDX_USER_VOLUME_ACCUMULATOR_BUY: usize = 13;

/// PumpFun bonding curve executor.
pub struct PumpFunExecutor {
    program_id: Pubkey,
}

impl PumpFunExecutor {
    pub fn new() -> Self {
        Self {
            program_id: PROGRAM_ID.parse().expect("valid PumpFun program ID"),
        }
    }

    /// Build instruction accounts by taking the whale's accounts and
    /// substituting user-specific entries.
    fn build_accounts_from_whale(
        &self,
        payer: &Pubkey,
        mint: &Pubkey,
        whale_accounts: &[Pubkey],
        is_buy: bool,
    ) -> Result<Vec<AccountMeta>, ExecutorError> {
        if whale_accounts.len() < MIN_ACCOUNTS {
            return Err(ExecutorError::InvalidAccount(format!(
                "PumpFun whale accounts too short: {} (need >= {})",
                whale_accounts.len(),
                MIN_ACCOUNTS,
            )));
        }

        // Detect which token program the whale used (idx 8 for buy, idx 9 for sell)
        let token_program_idx = if is_buy { 8 } else { 9 };
        let token_program = whale_accounts[token_program_idx];

        // Derive user's ATA using the SAME token program as the whale
        let user_ata = spl_associated_token_account::get_associated_token_address_with_program_id(
            payer,
            mint,
            &token_program,
        );

        // Clone whale accounts and substitute user-specific ones
        let mut accounts: Vec<AccountMeta> = whale_accounts
            .iter()
            .enumerate()
            .map(|(i, pk)| {
                let (writable, signer) = match i {
                    1 | 3 | 4 | 5 | 9 => (true, false), // fee_recipient, bonding_curve, associated_bonding_curve, associated_user, creator_vault
                    6 => (true, true),                  // user (signer)
                    _ if is_buy && i == 13 => (true, false), // user_volume_accumulator
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
        accounts[IDX_USER].pubkey = *payer;
        accounts[IDX_ASSOCIATED_USER].pubkey = user_ata;

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

impl DexExecutor for PumpFunExecutor {
    fn protocol_name(&self) -> &str {
        "PumpFun"
    }

    fn build_buy_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let whale_accounts = params.ix_accounts.as_ref().ok_or_else(|| {
            ExecutorError::PoolNotFound("PumpFun buy requires ix_accounts from whale tx".into())
        })?;

        let accounts = self.build_accounts_from_whale(payer, &params.mint, whale_accounts, true)?;

        // Use buy_exact_sol_in: spend specific SOL, get as many tokens as possible
        let mut data = Vec::with_capacity(25);
        data.extend_from_slice(&DISC_BUY_EXACT_SOL_IN);
        data.extend_from_slice(&params.amount.to_le_bytes()); // spendable_sol_in (lamports)
        data.extend_from_slice(&0u64.to_le_bytes()); // min_tokens_out = 0 (any)
        data.push(1); // track_volume

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Detect token program from whale accounts and create ATA with matching program
        let token_program = whale_accounts[8];
        let mut ixs = Vec::with_capacity(2);
        ixs.push(tx_utils::create_ata_idempotent_ix(
            payer,
            payer,
            &params.mint,
            &token_program,
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
            ExecutorError::PoolNotFound("PumpFun sell requires ix_accounts from whale tx".into())
        })?;

        let accounts =
            self.build_accounts_from_whale(payer, &params.mint, whale_accounts, false)?;

        // For sells: amount = token amount to sell, min_sol_output = minimum SOL to receive
        let min_sol_output = tx_utils::min_output_after_slippage(0, params.slippage_bps);

        let mut data = Vec::with_capacity(8 + 8 + 8);
        data.extend_from_slice(&DISC_SELL);
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&min_sol_output.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], min_sol_output))
    }
}
