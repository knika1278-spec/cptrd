#![allow(deprecated)]
//! PumpFun bonding-curve trade executor.
//!
//! Builds buy/sell instructions for the PumpFun bonding curve program.
//!
//! Source: official IDL `github.com/pump-fun/pump-public-docs/idl/pump.json`
//! and `docs/dex-reference.md` §2.
//!
//! # Instruction Layout
//!
//! ## Buy
//! - Discriminator: `[102, 6, 61, 18, 1, 218, 235, 234]`
//! - Args: `amount: u64` (tokens out), `max_sol_cost: u64`, `track_volume: Option<bool>` (1 byte)
//! - Accounts: global, fee_recipient, mint, bonding_curve, associated_bonding_curve,
//!   associated_user, user (trader), system_program, token_program, creator_vault,
//!   event_authority, program
//!
//! ## Sell
//! - Discriminator: `[51, 230, 133, 164, 1, 127, 131, 173]`
//! - Args: `amount: u64` (tokens in), `min_sol_output: u64`
//! - Accounts: global, fee_recipient, mint, bonding_curve, associated_bonding_curve,
//!   associated_user, user (trader), system_program, creator_vault, token_program,
//!   event_authority, program

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};
use spl_associated_token_account;

use super::{tx_utils, DexExecutor, ExecutorError, TradeParams};

/// PumpFun bonding-curve program ID.
const PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

// Discriminators = sha256("global:<name>")[..8]
const DISC_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const DISC_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// Well-known PumpFun accounts.
const GLOBAL_ACCOUNT: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf";
const FEE_RECIPIENT: &str = "CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbCJ27gGHyHjRY";
const EVENT_AUTHORITY: &str = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1";
const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

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

    /// Derive the bonding curve PDA for a given mint.
    ///
    /// Seeds: ["bonding-curve", mint.as_ref()]
    fn derive_bonding_curve(&self, mint: &Pubkey) -> Pubkey {
        let (pda, _bump) = Pubkey::find_program_address(
            &[b"bonding-curve", mint.as_ref()],
            &self.program_id,
        );
        pda
    }

    /// Derive the associated bonding curve token account.
    /// PumpFun uses Token-2022 since late 2024.
    fn derive_associated_bonding_curve(&self, bonding_curve: &Pubkey, mint: &Pubkey) -> Pubkey {
        spl_associated_token_account::get_associated_token_address_with_program_id(
            bonding_curve,
            mint,
            &tx_utils::token_2022_program_id(),
        )
    }

    fn build_accounts(
        &self,
        payer: &Pubkey,
        mint: &Pubkey,
        bonding_curve: &Pubkey,
        associated_bonding_curve: &Pubkey,
        is_buy: bool,
    ) -> Vec<AccountMeta> {
        let user_ata = spl_associated_token_account::get_associated_token_address(payer, mint);

        let global: Pubkey = GLOBAL_ACCOUNT.parse().unwrap();
        let fee_recipient: Pubkey = FEE_RECIPIENT.parse().unwrap();
        let event_authority: Pubkey = EVENT_AUTHORITY.parse().unwrap();
        let program: Pubkey = PUMP_FUN_PROGRAM.parse().unwrap();
        let token_program = spl_token::id();

        // Buy and sell differ at indices 8/9 (creator_vault vs token_program)
        let creator_vault: Pubkey = "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1"
            .parse()
            .unwrap();

        if is_buy {
            vec![
                AccountMeta::new_readonly(global, false),
                AccountMeta::new(fee_recipient, false),
                AccountMeta::new(*mint, false),
                AccountMeta::new(*bonding_curve, false),
                AccountMeta::new(*associated_bonding_curve, false),
                AccountMeta::new(user_ata, false),
                AccountMeta::new(*payer, true),
                AccountMeta::new_readonly(system_program::ID, false),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new(creator_vault, false),
                AccountMeta::new_readonly(event_authority, false),
                AccountMeta::new_readonly(program, false),
            ]
        } else {
            vec![
                AccountMeta::new_readonly(global, false),
                AccountMeta::new(fee_recipient, false),
                AccountMeta::new(*mint, false),
                AccountMeta::new(*bonding_curve, false),
                AccountMeta::new(*associated_bonding_curve, false),
                AccountMeta::new(user_ata, false),
                AccountMeta::new(*payer, true),
                AccountMeta::new_readonly(system_program::ID, false),
                AccountMeta::new(creator_vault, false),
                AccountMeta::new_readonly(token_program, false),
                AccountMeta::new_readonly(event_authority, false),
                AccountMeta::new_readonly(program, false),
            ]
        }
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
        let bonding_curve = self.derive_bonding_curve(&params.mint);
        let associated_bonding_curve =
            self.derive_associated_bonding_curve(&bonding_curve, &params.mint);

        let accounts = self.build_accounts(payer, &params.mint, &bonding_curve, &associated_bonding_curve, true);

        // For buys: amount = 0 (any tokens), max_sol_cost = SOL with slippage
        let max_sol_cost = tx_utils::max_input_after_slippage(params.amount, params.slippage_bps);

        let mut data = Vec::with_capacity(25);
        data.extend_from_slice(&DISC_BUY);
        data.extend_from_slice(&0u64.to_le_bytes()); // amount_out (any)
        data.extend_from_slice(&max_sol_cost.to_le_bytes());
        data.push(1); // track_volume

        let ix = Instruction::new_with_bytes(self.program_id, &data, accounts);

        // Idempotent ATA creation — no-op if already exists (1 ix, not 2)
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
        let bonding_curve = self.derive_bonding_curve(&params.mint);
        let associated_bonding_curve =
            self.derive_associated_bonding_curve(&bonding_curve, &params.mint);

        let accounts = self.build_accounts(
            payer,
            &params.mint,
            &bonding_curve,
            &associated_bonding_curve,
            false,
        );

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
