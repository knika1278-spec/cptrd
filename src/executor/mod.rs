//! Trade execution module.
//!
//! Provides buy/sell execution for four Solana DEX protocols:
//! - PumpFun (bonding curve)
//! - PumpSwap (AMM)
//! - Meteora DLMM v2
//! - Raydium Launchpad (LaunchLab)
//!
//! Each protocol has its own executor implementing the [`DexExecutor`] trait.
//! Shared transaction building and submission logic lives in [`tx_utils`].

pub mod meteora_dlmm;
pub mod pumpfun;
pub mod pumpswap;
pub mod raydium_amm_v4;
pub mod raydium_launchpad;
pub mod tx_utils;
pub mod wallet;

use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signer;

use crate::models::types::{ExecutionResult, TradeAction};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during trade execution.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum ExecutorError {
    #[error("insufficient SOL balance: need {needed} lamports, have {available}")]
    InsufficientSolBalance { needed: u64, available: u64 },

    #[error("insufficient token balance: need {needed}, have {available}")]
    InsufficientTokenBalance { needed: u64, available: u64 },

    #[error("slippage exceeded: expected {expected}, got {actual}")]
    SlippageExceeded { expected: u64, actual: u64 },

    #[error("transaction failed: {0}")]
    TransactionFailed(String),

    #[error("RPC error: {0}")]
    RpcError(String),

    #[error("invalid account: {0}")]
    InvalidAccount(String),

    #[error("pool/pair not found for mint {0}")]
    PoolNotFound(String),

    #[error("max SOL per trade exceeded: trade would cost {cost} SOL, cap is {cap}")]
    MaxSolExceeded { cost: f64, cap: f64 },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// Trade parameters
// ---------------------------------------------------------------------------

/// Parameters for a buy or sell trade.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TradeParams {
    /// The token mint to buy or sell.
    pub mint: Pubkey,
    /// Trade direction.
    pub action: TradeAction,
    /// For buys: SOL amount to spend (in lamports).
    /// For sells: token amount to sell (in raw units).
    pub amount: u64,
    /// Slippage tolerance in basis points (100 = 1%).
    pub slippage_bps: u16,
    /// Priority fee in microlamports per compute unit.
    pub priority_fee_microlamports: u64,
    /// Compute unit limit.
    pub compute_unit_limit: u32,
    /// Instruction accounts from the whale's decoded transaction.
    /// Used by executors (e.g. PumpSwap) that need exact on-chain accounts.
    pub ix_accounts: Option<Vec<Pubkey>>,
}

// ---------------------------------------------------------------------------
// DexExecutor trait
// ---------------------------------------------------------------------------

/// Trait for DEX-specific trade executors.
///
/// Each protocol implements this trait to build the correct instructions
/// for buy and sell operations. The executor handles account discovery,
/// instruction construction, and amount calculation.
pub trait DexExecutor: Send + Sync {
    /// The DEX protocol name (for logging).
    fn protocol_name(&self) -> &str;

    /// Build buy instructions: spend SOL to acquire tokens.
    ///
    /// Returns the list of instructions to include in the transaction,
    /// plus the expected minimum token output (after slippage).
    fn build_buy_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError>;

    /// Build sell instructions: sell tokens to receive SOL.
    ///
    /// Returns the list of instructions to include in the transaction,
    /// plus the expected minimum SOL output (after slippage).
    fn build_sell_ixs(
        &self,
        payer: &Pubkey,
        params: &TradeParams,
    ) -> Result<(Vec<Instruction>, u64), ExecutorError>;
}

// ---------------------------------------------------------------------------
// Copy executor — orchestrates decode → guard → execute
// ---------------------------------------------------------------------------

/// Execute a copy trade based on a decoded event.
///
/// This is the main entry point for trade execution. It:
/// 1. Resolves the correct executor for the DEX protocol
/// 2. Uses per-protocol trade config (buy amount + slippage)
/// 3. Submits the transaction
/// Execute a copy trade based on a decoded event.
pub async fn execute_copy_trade(
    rpc_url: &str,
    keypair: &solana_sdk::signature::Keypair,
    mint: &str,
    action: TradeAction,
    dex: crate::models::types::DexProtocol,
    token_amount: u64,
    ix_accounts: Option<Vec<String>>,
    protocol_configs: &crate::models::types::ProtocolConfigs,
    priority_fee_microlamports: u64,
    compute_unit_limit: u32,
    blockhash_cache: &tx_utils::BlockhashCache,
    use_jito: bool,
    jito_tip_lamports: u64,
) -> Result<ExecutionResult, ExecutorError> {
    use crate::models::types::DexProtocol;

    let mint_pubkey: Pubkey = mint
        .parse()
        .map_err(|_| ExecutorError::InvalidAccount(format!("invalid mint: {mint}")))?;

    // Get per-protocol config
    let protocol_config = match dex {
        DexProtocol::PumpFun => protocol_configs.pumpfun.as_ref(),
        DexProtocol::PumpSwap => protocol_configs.pumpswap.as_ref(),
        DexProtocol::RaydiumLaunchpad => protocol_configs.raydium_launchpad.as_ref(),
        DexProtocol::RaydiumAmmV4 => protocol_configs.raydium_amm_v4.as_ref(),
        DexProtocol::MeteoraDlmmV2 => protocol_configs.meteora_dlmm.as_ref(),
        DexProtocol::Unknown => None,
    };

    // If no config for this protocol, skip execution
    let config = match protocol_config {
        Some(c) => c,
        None => {
            tracing::debug!(?dex, "no trade config for protocol, skipping execution");
            return Err(ExecutorError::PoolNotFound(format!(
                "no trade config for {:?}",
                dex
            )));
        }
    };

    // For sells, check if copy_sells is enabled
    if action == TradeAction::Sell && !config.copy_sells {
        tracing::debug!(?dex, "copy_sells disabled for protocol, skipping sell");
        return Err(ExecutorError::PoolNotFound(format!(
            "copy_sells disabled for {:?}",
            dex
        )));
    }

    let params = TradeParams {
        mint: mint_pubkey,
        action,
        amount: match action {
            TradeAction::Buy => (config.buy_sol_amount * 1_000_000_000.0) as u64,
            TradeAction::Sell => token_amount,
        },
        slippage_bps: config.slippage_bps,
        priority_fee_microlamports,
        compute_unit_limit,
        ix_accounts: ix_accounts
            .and_then(|v| Some(v.iter().filter_map(|s| s.parse::<Pubkey>().ok()).collect())),
    };

    // Try the correct executor first, then fall back to others
    let executors: Vec<Box<dyn DexExecutor>> = match dex {
        DexProtocol::PumpFun => vec![Box::new(pumpfun::PumpFunExecutor::new())],
        DexProtocol::PumpSwap => vec![Box::new(pumpswap::PumpSwapExecutor::new())],
        DexProtocol::RaydiumLaunchpad => {
            vec![Box::new(raydium_launchpad::RaydiumLaunchpadExecutor::new())]
        }
        DexProtocol::RaydiumAmmV4 => vec![Box::new(raydium_amm_v4::RaydiumAmmV4Executor::new())],
        DexProtocol::MeteoraDlmmV2 => vec![Box::new(meteora_dlmm::MeteoraDlmmExecutor::new())],
        DexProtocol::Unknown => return Err(ExecutorError::PoolNotFound("unknown DEX".to_string())),
    };

    let payer = keypair.pubkey();

    // For sells, check if bot has any tokens before attempting
    if action == TradeAction::Sell {
        let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(rpc_url.to_string());
        let mint_pk: solana_sdk::pubkey::Pubkey = mint
            .parse()
            .map_err(|_| ExecutorError::InvalidAccount(format!("invalid mint: {mint}")))?;

        // Check balance using Token-2022 ATA (PumpFun/PumpSwap use Token-2022)
        let ata_2022 = spl_associated_token_account::get_associated_token_address_with_program_id(
            &payer,
            &mint_pk,
            &tx_utils::token_2022_program_id(),
        );
        let ata_spl = spl_associated_token_account::get_associated_token_address(&payer, &mint_pk);

        // Try Token-2022 ATA first, then SPL Token ATA
        let has_tokens = match rpc.get_token_account_balance(&ata_2022).await {
            Ok(balance) => balance.amount.parse::<u64>().unwrap_or(0) > 0,
            Err(_) => match rpc.get_token_account_balance(&ata_spl).await {
                Ok(balance) => balance.amount.parse::<u64>().unwrap_or(0) > 0,
                Err(_) => false,
            },
        };

        if !has_tokens {
            tracing::info!(
                mint = %mint,
                "skipping sell: bot has no tokens of this mint"
            );
            return Err(ExecutorError::InsufficientTokenBalance {
                needed: 1,
                available: 0,
            });
        }
    }

    for executor in &executors {
        let ixs = match action {
            TradeAction::Buy => match executor.build_buy_ixs(&payer, &params) {
                Ok((ixs, _min_out)) => ixs,
                Err(_) => continue,
            },
            TradeAction::Sell => match executor.build_sell_ixs(&payer, &params) {
                Ok((ixs, _min_out)) => ixs,
                Err(_) => continue,
            },
        };

        if ixs.is_empty() {
            continue;
        }

        tracing::info!(
            protocol = executor.protocol_name(),
            action = ?action,
            mint = %mint,
            amount = if action == TradeAction::Buy { config.buy_sol_amount } else { 0.0 },
            slippage_bps = config.slippage_bps,
            jito = use_jito,
            "submitting {} transaction",
            if action == TradeAction::Buy { "buy" } else { "sell" }
        );

        if use_jito {
            return tx_utils::submit_jito_bundle(
                rpc_url,
                keypair,
                &ixs,
                priority_fee_microlamports,
                compute_unit_limit,
                jito_tip_lamports,
                blockhash_cache,
            )
            .await;
        } else {
            return tx_utils::submit_transaction(
                rpc_url,
                keypair,
                &ixs,
                priority_fee_microlamports,
                compute_unit_limit,
                blockhash_cache,
            )
            .await;
        }
    }

    Err(ExecutorError::PoolNotFound(format!(
        "{:?} executor failed for mint {}",
        dex, mint
    )))
}
