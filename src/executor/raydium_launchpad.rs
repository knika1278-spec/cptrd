//! Raydium Launchpad (LaunchLab) trade executor.
//!
//! Builds buy/sell instructions for the Raydium Launchpad token-launch program.
//!
//! Strategy: copy ALL accounts from the whale's decoded instruction (via
//! `params.ix_accounts`) and substitute only the user-specific accounts.
//!
//! Source: official IDL `github.com/raydium-io/raydium-idl/raydium_launchpad/
//! raydium_launchpad.json` (`raydium_launchpad` v0.2.0) and `docs/dex-reference.md` §4.
//!
//! # Account Layout (15 total)
//!
//! | idx | account              | substitute? |
//! |-----|----------------------|-------------|
//! |  0  | payer (signer)       | YES         |
//! |  1  | authority (PDA)      | no          |
//! |  2  | global_config        | no          |
//! |  3  | platform_config      | no          |
//! |  4  | pool_state           | no          |
//! |  5  | user_base_token      | YES         |
//! |  6  | user_quote_token     | YES         |
//! |  7  | base_vault           | no          |
//! |  8  | quote_vault          | no          |
//! |  9  | base_token_mint      | no          |
//! | 10  | quote_token_mint     | no          |
//! | 11  | base_token_program   | no          |
//! | 12  | quote_token_program  | no          |
//! | 13  | event_authority      | no          |
//! | 14  | program              | no          |

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// Raydium Launchpad / LaunchLab program ID.
const PROGRAM_ID: &str = "LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj";

// Discriminators (sha256("global:<name>")[..8])
const DISC_BUY_EXACT_IN: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];
const DISC_SELL_EXACT_IN: [u8; 8] = [149, 39, 222, 155, 211, 124, 152, 26];

/// Minimum expected accounts for Raydium Launchpad.
const MIN_ACCOUNTS: usize = 15;

/// Account indices that must be substituted with our own.
const IDX_PAYER: usize = 0;
const IDX_USER_BASE_TOKEN: usize = 5;
const IDX_USER_QUOTE_TOKEN: usize = 6;

/// Raydium Launchpad executor.
pub struct RaydiumLaunchpadExecutor {
    program_id: Pubkey,
}

impl RaydiumLaunchpadExecutor {
    pub fn new() -> Self {
        Self {
            program_id: PROGRAM_ID
                .parse()
                .expect("valid Raydium Launchpad program ID"),
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
                "Raydium Launchpad whale accounts too short: {} (need >= {})",
                whale_accounts.len(),
                MIN_ACCOUNTS,
            )));
        }

        // Derive user ATAs
        let user_base =
            spl_associated_token_account::get_associated_token_address(payer, base_mint);
        let user_quote =
            spl_associated_token_account::get_associated_token_address(payer, quote_mint);

        // Clone whale accounts and substitute user-specific ones
        let mut accounts: Vec<AccountMeta> = whale_accounts
            .iter()
            .enumerate()
            .map(|(i, pk)| {
                let (writable, signer) = match i {
                    0 => (true, true),                  // payer (signer)
                    4 | 5 | 6 | 7 | 8 => (true, false), // pool_state, user tokens, vaults
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
        accounts[IDX_PAYER].pubkey = *payer;
        accounts[IDX_USER_BASE_TOKEN].pubkey = user_base;
        accounts[IDX_USER_QUOTE_TOKEN].pubkey = user_quote;

        Ok(accounts)
    }
}

impl DexExecutor for RaydiumLaunchpadExecutor {
    fn protocol_name(&self) -> &str {
        "Raydium Launchpad"
    }

    fn build_buy_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let whale_accounts = params.ix_accounts.as_ref().ok_or_else(|| {
            ExecutorError::PoolNotFound(
                "Raydium Launchpad buy requires ix_accounts from whale tx".into(),
            )
        })?;

        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        // For buys: base = token, quote = WSOL
        let accounts =
            self.build_accounts_from_whale(payer, &params.mint, &quote_mint, whale_accounts)?;

        let max_amount_in = tx_utils::max_input_after_slippage(params.amount, params.slippage_bps);

        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&DISC_BUY_EXACT_IN);
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // minimum_amount_out = 0
        data.extend_from_slice(&0u64.to_le_bytes()); // share_fee_rate = 0

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Create user token ATA (idempotent)
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
                "Raydium Launchpad sell requires ix_accounts from whale tx".into(),
            )
        })?;

        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        // For sells: base = token, quote = WSOL
        let accounts =
            self.build_accounts_from_whale(payer, &params.mint, &quote_mint, whale_accounts)?;

        let min_amount_out = tx_utils::min_output_after_slippage(0, params.slippage_bps);

        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&DISC_SELL_EXACT_IN);
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // share_fee_rate = 0

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], min_amount_out))
    }
}
