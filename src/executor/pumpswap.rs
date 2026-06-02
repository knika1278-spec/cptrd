#![allow(deprecated)]
//! PumpSwap AMM trade executor.
//!
//! Builds buy/sell instructions for the PumpSwap AMM program.
//!
//! Source: official IDL `github.com/pump-fun/pump-public-docs/idl/pump_amm.json`
//! and `docs/dex-reference.md` §3.
//!
//! # Instruction Layout
//!
//! ## Buy
//! - Discriminator: `[102, 6, 61, 18, 1, 218, 235, 234]` (same as PumpFun)
//! - Args: `base_amount_out: u64`, `max_quote_amount_in: u64`, `track_volume: Option<bool>`
//! - Accounts: pool, user (trader), global_config, base_mint, quote_mint,
//!   user_base_token_account, user_quote_token_account, pool_base_token_account,
//!   pool_quote_token_account, protocol_fee_recipient, protocol_fee_recipient_token_account,
//!   base_token_program, quote_token_program, system_program, associated_token_program,
//!   event_authority, program, coin_creator_vault_ata, coin_creator_vault_authority
//!
//! ## Sell
//! - Discriminator: `[51, 230, 133, 164, 1, 127, 131, 173]` (same as PumpFun)
//! - Args: `base_amount_in: u64`, `min_quote_amount_out: u64`
//! - Same accounts as buy

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// PumpSwap AMM program ID.
const PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

// Discriminators — IDENTICAL to PumpFun; disambiguate by program ID.
const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const DISC_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Well-known PumpSwap accounts.
const GLOBAL_CONFIG: &str = "8pTAg7BqoXjE6zJzBKFD8D7G1G8FxLxMKh7AE9N1jxw";
const EVENT_AUTHORITY: &str = "GS4CU59F31iL7oaRvzYfpbKzEa6jV1YZ1z3FqrmYFJk4";
const PROTOCOL_FEE_RECIPIENT: &str = "68yFSZbGTCHkx9UBaEsK3ZmHQ2Zsx4UNMbh6LqKJ1yoN";

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

    /// Derive the pool PDA for a base/quote mint pair.
    ///
    /// Seeds: ["pool", creator.as_ref(), base_mint.as_ref(), quote_mint.as_ref()]
    /// In practice, pool accounts are discoverable on-chain. For now we derive
    /// using a common creator or use the pool directly if known.
    fn derive_pool(&self, base_mint: &Pubkey, quote_mint: &Pubkey) -> Pubkey {
        // PumpSwap pools are created by a known pool creator.
        // The PDA seeds vary; for now we use a deterministic derivation.
        // In production, this should be fetched from an indexer or on-chain lookup.
        let pool_creator: Pubkey = "FFWtrEQ4B4PKQoVuH3iBgoT6UBPSURRgCoWqC6Lj6m8R"
            .parse()
            .unwrap();
        let (pda, _bump) = Pubkey::find_program_address(
            &[
                b"pool",
                pool_creator.as_ref(),
                base_mint.as_ref(),
                quote_mint.as_ref(),
            ],
            &self.program_id,
        );
        pda
    }

    fn build_accounts(
        &self,
        payer: &Pubkey,
        base_mint: &Pubkey,
        quote_mint: &Pubkey,
        pool: &Pubkey,
    ) -> Vec<AccountMeta> {
        let user_base_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                payer, base_mint, &tx_utils::token_2022_program_id(),
            );
        let user_quote_ata =
            spl_associated_token_account::get_associated_token_address(payer, quote_mint);

        // Pool token accounts are ATAs of the pool PDA
        let pool_base_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                pool, base_mint, &tx_utils::token_2022_program_id(),
            );
        let pool_quote_ata =
            spl_associated_token_account::get_associated_token_address(pool, quote_mint);

        let global_config: Pubkey = GLOBAL_CONFIG.parse().unwrap();
        let event_authority: Pubkey = EVENT_AUTHORITY.parse().unwrap();
        let protocol_fee_recipient: Pubkey = PROTOCOL_FEE_RECIPIENT.parse().unwrap();
        let protocol_fee_recipient_ata =
            spl_associated_token_account::get_associated_token_address(
                &protocol_fee_recipient,
                quote_mint,
            );

        let base_token_program = tx_utils::token_2022_program_id();
        let quote_token_program = spl_token::id();
        let associated_token_program = spl_associated_token_account::id();

        // Coin creator vault (PumpSwap-specific)
        let coin_creator_vault_authority: Pubkey =
            "3se2H6BsCwM8MdMNPSsMfCfUuFbP2wVE7wj6WwLkEJn6"
                .parse()
                .unwrap();
        let coin_creator_vault_ata =
            spl_associated_token_account::get_associated_token_address(
                &coin_creator_vault_authority,
                quote_mint,
            );

        vec![
            AccountMeta::new(*pool, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(global_config, false),
            AccountMeta::new(*base_mint, false),
            AccountMeta::new(*quote_mint, false),
            AccountMeta::new(user_base_ata, false),
            AccountMeta::new(user_quote_ata, false),
            AccountMeta::new(pool_base_ata, false),
            AccountMeta::new(pool_quote_ata, false),
            AccountMeta::new(protocol_fee_recipient, false),
            AccountMeta::new(protocol_fee_recipient_ata, false),
            AccountMeta::new_readonly(base_token_program, false),
            AccountMeta::new_readonly(quote_token_program, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(associated_token_program, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(self.program_id, false),
            AccountMeta::new(coin_creator_vault_ata, false),
            AccountMeta::new_readonly(coin_creator_vault_authority, false),
        ]
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
        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();
        let pool = self.derive_pool(&params.mint, &quote_mint);
        let accounts = self.build_accounts(payer, &params.mint, &quote_mint, &pool);

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
        ixs.push(tx_utils::create_ata_idempotent_ix(payer, payer, &params.mint, &tx_utils::token_2022_program_id()));
        ixs.push(ix);

        Ok((ixs, 0))
    }

    fn build_sell_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let quote_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();
        let pool = self.derive_pool(&params.mint, &quote_mint);
        let accounts = self.build_accounts(payer, &params.mint, &quote_mint, &pool);

        let min_quote_out = tx_utils::min_output_after_slippage(0, params.slippage_bps);

        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&DISC_SELL);
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&min_quote_out.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], min_quote_out))
    }
}
