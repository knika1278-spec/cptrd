#![allow(deprecated)]
//! Meteora DLMM (Liquidity Book) swap executor.
//!
//! Builds swap instructions for the Meteora DLMM program.
//!
//! Source: official IDL `github.com/MeteoraAg/dlmm-sdk/idls/dlmm.json`
//! (`lb_clmm` v0.12.0) and `docs/dex-reference.md` §5.
//!
//! # Swap Instruction
//! - Discriminator: `[248, 198, 158, 145, 225, 117, 135, 200]` (swap)
//! - Also supports: `swap2`, `swap_exact_out`, `swap_exact_out2`,
//!   `swap_with_price_impact`, `swap_with_price_impact2`
//! - Args: `amount_in: u64`, `min_amount_out: u64`
//! - Direction is NOT in the instruction name — derived from WSOL flow:
//!   WSOL leaves trader ⇒ Buy; WSOL returns ⇒ Sell.
//!
//! # Accounts (swap)
//! - 0: lb_pair (pool)
//! - 1: bin_array_bitmap_extension (optional)
//! - 2: reserve_x
//! - 3: reserve_y
//! - 4: user_token_in
//! - 5: user_token_out
//! - 6: token_x_mint
//! - 7: token_y_mint
//! - 8: oracle
//! - 9: host_fee_in (optional)
//! - 10: user (trader, signer)
//! - 11: token_x_program
//! - 12: token_y_program
//! - 13: event_authority
//! - 14: program
//!
//! NOTE: `swap2` inserts `memo_program` shifting later accounts.
//! Optional accounts shift indices — resolve by relation, not fixed offset.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// Meteora DLMM program ID.
const PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

// Swap discriminators
const DISC_SWAP: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];
#[allow(dead_code)]
const DISC_SWAP_EXACT_OUT: [u8; 8] = [250, 73, 101, 33, 38, 207, 75, 184];

/// Well-known Meteora DLMM accounts.
const EVENT_AUTHORITY: &str = "2LbAtCkRfJwU8xMZ2N6wJ8BzMLyWBcJ7p5Z4bYQ5sA";
#[allow(dead_code)]
const METEORA_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

/// Meteora DLMM executor.
pub struct MeteoraDlmmExecutor {
    program_id: Pubkey,
}

impl MeteoraDlmmExecutor {
    pub fn new() -> Self {
        Self {
            program_id: PROGRAM_ID.parse().expect("valid Meteora DLMM program ID"),
        }
    }

    /// Find an LB pair for the given token mint.
    ///
    /// Meteora DLMM pairs are created with specific token mints. In production,
    /// this should be fetched from an indexer or on-chain lookup.
    /// For now, we derive using known pair creation patterns.
    fn find_lb_pair(&self, token_mint: &Pubkey) -> Option<Pubkey> {
        let wsol_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();

        // DLMM pairs use PDA seeds: ["lb_pair", token_x, token_y]
        // Where token_x < token_y (sorted by pubkey bytes)
        let (mint_a, mint_b) = if token_mint < &wsol_mint {
            (token_mint, &wsol_mint)
        } else {
            (&wsol_mint, token_mint)
        };

        let (pda, _bump) = Pubkey::find_program_address(
            &[b"lb_pair", mint_a.as_ref(), mint_b.as_ref()],
            &self.program_id,
        );
        Some(pda)
    }

    /// Derive the oracle PDA for an LB pair.
    fn derive_oracle(&self, lb_pair: &Pubkey) -> Pubkey {
        let (pda, _bump) =
            Pubkey::find_program_address(&[b"oracle", lb_pair.as_ref()], &self.program_id);
        pda
    }

    /// Derive bin array bitmap extension PDA.
    fn derive_bitmap_extension(&self, lb_pair: &Pubkey) -> Pubkey {
        let (pda, _bump) = Pubkey::find_program_address(
            &[b"bin_array_bitmap_extension", lb_pair.as_ref()],
            &self.program_id,
        );
        pda
    }

    fn build_swap_accounts(
        &self,
        payer: &Pubkey,
        token_mint: &Pubkey,
        lb_pair: &Pubkey,
        is_buy: bool,
    ) -> Vec<AccountMeta> {
        let wsol_mint: Pubkey = "So11111111111111111111111111111111111111112"
            .parse()
            .unwrap();
        let oracle = self.derive_oracle(lb_pair);
        let bitmap_ext = self.derive_bitmap_extension(lb_pair);
        let event_authority: Pubkey = EVENT_AUTHORITY.parse().unwrap();
        let token_program = spl_token::id();

        // Determine token_x and token_y (sorted by pubkey)
        let (token_x_mint, token_y_mint) = if token_mint < &wsol_mint {
            (token_mint, &wsol_mint)
        } else {
            (&wsol_mint, token_mint)
        };

        // Derive reserve accounts (ATAs of the LB pair for each token)
        let reserve_x =
            spl_associated_token_account::get_associated_token_address(lb_pair, token_x_mint);
        let reserve_y =
            spl_associated_token_account::get_associated_token_address(lb_pair, token_y_mint);

        // User token accounts
        let user_token_x =
            spl_associated_token_account::get_associated_token_address(payer, token_x_mint);
        let user_token_y =
            spl_associated_token_account::get_associated_token_address(payer, token_y_mint);

        // For buy: user sends WSOL (quote), receives token (base)
        // For sell: user sends token (base), receives WSOL (quote)
        let (user_token_in, user_token_out) = if is_buy {
            if token_mint < &wsol_mint {
                // token is x, WSOL is y → input = y, output = x
                (user_token_y, user_token_x)
            } else {
                // WSOL is x, token is y → input = x, output = y
                (user_token_x, user_token_y)
            }
        } else {
            if token_mint < &wsol_mint {
                // token is x, WSOL is y → input = x, output = y
                (user_token_x, user_token_y)
            } else {
                // WSOL is x, token is y → input = y, output = x
                (user_token_y, user_token_x)
            }
        };

        vec![
            AccountMeta::new(*lb_pair, false),
            AccountMeta::new(bitmap_ext, false),
            AccountMeta::new(reserve_x, false),
            AccountMeta::new(reserve_y, false),
            AccountMeta::new(user_token_in, false),
            AccountMeta::new(user_token_out, false),
            AccountMeta::new_readonly(*token_x_mint, false),
            AccountMeta::new_readonly(*token_y_mint, false),
            AccountMeta::new(oracle, false),
            AccountMeta::new_readonly(system_program::ID, false), // host_fee_in placeholder
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(token_program, false), // token_y_program
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(self.program_id, false),
        ]
    }
}

impl DexExecutor for MeteoraDlmmExecutor {
    fn protocol_name(&self) -> &str {
        "Meteora DLMM"
    }

    fn build_buy_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let lb_pair = self
            .find_lb_pair(&params.mint)
            .ok_or_else(|| ExecutorError::PoolNotFound(params.mint.to_string()))?;

        let accounts = self.build_swap_accounts(payer, &params.mint, &lb_pair, true);

        // For buys: amount_in = SOL/WSOL amount, min_amount_out = 0 (any)
        let amount_in = tx_utils::max_input_after_slippage(params.amount, params.slippage_bps);
        let min_amount_out = 0u64;

        let mut data = Vec::with_capacity(8 + 8 + 8);
        data.extend_from_slice(&DISC_SWAP);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Create user token ATA (idempotent — no-op if exists)
        let mut ixs = Vec::with_capacity(2);
        ixs.push(tx_utils::create_ata_idempotent_ix(
            payer,
            payer,
            &params.mint,
            &spl_token::id(),
        ));
        ixs.push(ix);

        Ok((ixs, min_amount_out))
    }

    fn build_sell_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError> {
        let lb_pair = self
            .find_lb_pair(&params.mint)
            .ok_or_else(|| ExecutorError::PoolNotFound(params.mint.to_string()))?;

        let accounts = self.build_swap_accounts(payer, &params.mint, &lb_pair, false);

        // For sells: amount_in = token amount, min_amount_out = min SOL
        let min_amount_out = tx_utils::min_output_after_slippage(0, params.slippage_bps);

        let mut data = Vec::with_capacity(8 + 8 + 8);
        data.extend_from_slice(&DISC_SWAP);
        data.extend_from_slice(&params.amount.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        Ok((vec![ix], min_amount_out))
    }
}
