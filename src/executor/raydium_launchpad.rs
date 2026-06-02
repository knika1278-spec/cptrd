//! Raydium Launchpad (LaunchLab) trade executor.
//!
//! Builds buy/sell instructions for the Raydium Launchpad token-launch program.
//!
//! Source: official IDL `github.com/raydium-io/raydium-idl/raydium_launchpad/
//! raydium_launchpad.json` (`raydium_launchpad` v0.2.0) and `docs/dex-reference.md` §4.
//!
//! **NOTE:** This is the Launchpad/LaunchLab program. It is NOT Raydium AMM v4,
//! CLMM, or CPMM — those are separate programs.
//!
//! # Instruction Variants
//!
//! | Instruction        | Discriminator                            | Args                                    |
//! |--------------------|------------------------------------------|-----------------------------------------|
//! | buy_exact_in       | `[250,234,13,123,213,156,19,236]`        | amount_in, minimum_amount_out, share_fee_rate |
//! | buy_exact_out      | `[24,211,116,40,105,3,153,56]`           | amount_out, maximum_amount_in, share_fee_rate |
//! | sell_exact_in      | `[149,39,222,155,211,124,152,26]`        | amount_in, minimum_amount_out, share_fee_rate |
//! | sell_exact_out     | `[95,200,71,34,8,9,11,166]`              | amount_out, maximum_amount_in, share_fee_rate |
//!
//! # Accounts (identical for all 4 variants)
//! - 0: payer (trader, signer)
//! - 1: authority (PDA)
//! - 2: global_config
//! - 3: platform_config
//! - 4: pool_state
//! - 5: user_base_token
//! - 6: user_quote_token
//! - 7: base_vault
//! - 8: quote_vault
//! - 9: base_token_mint (launched token)
//! - 10: quote_token_mint (WSOL)
//! - 11: base_token_program
//! - 12: quote_token_program
//! - 13: event_authority
//! - 14: program

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// Raydium Launchpad / LaunchLab program ID.
const PROGRAM_ID: &str = "LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj";

// Discriminators (sha256("global:<name>")[..8])
const DISC_BUY_EXACT_IN: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];
#[allow(dead_code)]
const DISC_BUY_EXACT_OUT: [u8; 8] = [24, 211, 116, 40, 105, 3, 153, 56];
const DISC_SELL_EXACT_IN: [u8; 8] = [149, 39, 222, 155, 211, 124, 152, 26];
#[allow(dead_code)]
const DISC_SELL_EXACT_OUT: [u8; 8] = [95, 200, 71, 34, 8, 9, 11, 166];

/// Well-known Raydium Launchpad accounts.
const GLOBAL_CONFIG: &str = "8JdGBm6BPE4XPR5MJRz6kBo4CYzWnKSNzkVRS3tb1pLh";
const PLATFORM_CONFIG: &str = "4abfirDoCpJwWYC7LqFbn5F3vMRWwJU670mEbiGPJ9iq";
const EVENT_AUTHORITY: &str = "2DPAtEq3eJYbDRJLzDAc7URx9Y3iQ6CTNpoLhRTcQDyK";

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

    /// Derive the authority PDA for the launchpad program.
    fn derive_authority(&self, pool_state: &Pubkey) -> Pubkey {
        let (pda, _bump) =
            Pubkey::find_program_address(&[b"authority", pool_state.as_ref()], &self.program_id);
        pda
    }

    /// Find the pool state for a given token mint.
    ///
    /// Launchpad pools are created with a specific base token mint.
    /// In production, this should be fetched from an indexer.
    fn find_pool_state(&self, base_mint: &Pubkey) -> Option<Pubkey> {
        let wsol_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        // Pool state PDA seeds vary by pool type. For AMM pools:
        // ["pool_state", base_mint, quote_mint]
        let (pda, _bump) = Pubkey::find_program_address(
            &[b"pool_state", base_mint.as_ref(), wsol_mint.as_ref()],
            &self.program_id,
        );
        Some(pda)
    }

    fn build_accounts(
        &self,
        payer: &Pubkey,
        base_mint: &Pubkey,
        pool_state: &Pubkey,
    ) -> Vec<AccountMeta> {
        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();
        let authority = self.derive_authority(pool_state);
        let global_config: Pubkey = GLOBAL_CONFIG.parse().unwrap();
        let platform_config: Pubkey = PLATFORM_CONFIG.parse().unwrap();
        let event_authority: Pubkey = EVENT_AUTHORITY.parse().unwrap();

        let user_base_ata =
            spl_associated_token_account::get_associated_token_address(payer, base_mint);
        let user_quote_ata =
            spl_associated_token_account::get_associated_token_address(payer, &quote_mint);

        // Vault ATAs (pool's token accounts)
        let base_vault =
            spl_associated_token_account::get_associated_token_address(pool_state, base_mint);
        let quote_vault =
            spl_associated_token_account::get_associated_token_address(pool_state, &quote_mint);

        let token_program = spl_token::id();

        vec![
            AccountMeta::new(*payer, true),                  // 0: payer
            AccountMeta::new_readonly(authority, false),     // 1: authority (PDA)
            AccountMeta::new_readonly(global_config, false), // 2: global_config
            AccountMeta::new_readonly(platform_config, false), // 3: platform_config
            AccountMeta::new(*pool_state, false),            // 4: pool_state
            AccountMeta::new(user_base_ata, false),          // 5: user_base_token
            AccountMeta::new(user_quote_ata, false),         // 6: user_quote_token
            AccountMeta::new(base_vault, false),             // 7: base_vault
            AccountMeta::new(quote_vault, false),            // 8: quote_vault
            AccountMeta::new_readonly(*base_mint, false),    // 9: base_token_mint
            AccountMeta::new_readonly(quote_mint, false),    // 10: quote_token_mint
            AccountMeta::new_readonly(token_program, false), // 11: base_token_program
            AccountMeta::new_readonly(token_program, false), // 12: quote_token_program
            AccountMeta::new_readonly(event_authority, false), // 13: event_authority
            AccountMeta::new_readonly(self.program_id, false), // 14: program
        ]
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
        let pool_state = self
            .find_pool_state(&params.mint)
            .ok_or_else(|| ExecutorError::PoolNotFound(params.mint.to_string()))?;

        let accounts = self.build_accounts(payer, &params.mint, &pool_state);

        // Use buy_exact_in: spend SOL, get tokens
        let max_amount_in = tx_utils::max_input_after_slippage(params.amount, params.slippage_bps);
        let minimum_amount_out = 0u64;
        let share_fee_rate = 0u64;

        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&DISC_BUY_EXACT_IN);
        data.extend_from_slice(&max_amount_in.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out.to_le_bytes());
        data.extend_from_slice(&share_fee_rate.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Create user token ATA (idempotent — no-op if exists)
        let mut ixs = Vec::with_capacity(2);
        ixs.push(tx_utils::create_ata_idempotent_ix(payer, payer, &params.mint));
        ixs.push(ix);

        Ok((ixs, minimum_amount_out))
    }

    fn build_sell_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let pool_state = self
            .find_pool_state(&params.mint)
            .ok_or_else(|| ExecutorError::PoolNotFound(params.mint.to_string()))?;

        let accounts = self.build_accounts(payer, &params.mint, &pool_state);

        // Use sell_exact_in: sell tokens, get SOL
        let minimum_amount_out = tx_utils::min_output_after_slippage(0, params.slippage_bps);
        let share_fee_rate = 0u64;

        let mut data = Vec::with_capacity(8 + 8 + 8 + 8);
        data.extend_from_slice(&DISC_SELL_EXACT_IN);
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out.to_le_bytes());
        data.extend_from_slice(&share_fee_rate.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], minimum_amount_out))
    }
}
