//! Shared transaction building and submission utilities.
//!
//! Low-latency optimizations:
//! - Blockhash cache (background refresh every 2s)
//! - Fire-and-forget submission (no waiting for confirmation)
//! - Jito bundle submission for MEV protection

use std::sync::Arc;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use tokio::sync::RwLock;
use base64::Engine;

use super::ExecutorError;
use crate::models::types::ExecutionResult;

// ---------------------------------------------------------------------------
// Blockhash cache — background refresh, zero-latency reads
// ---------------------------------------------------------------------------

/// Cached blockhash with timestamp.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CachedBlockhash {
    blockhash: solana_sdk::hash::Hash,
    fetched_at: std::time::Instant,
}

/// Thread-safe blockhash cache with background refresh.
#[derive(Clone)]
#[allow(dead_code)]
pub struct BlockhashCache {
    inner: Arc<RwLock<CachedBlockhash>>,
    rpc_url: String,
}

impl BlockhashCache {
    /// Create a new cache and start background refresh.
    pub fn new(rpc_url: &str) -> Self {
        let rpc_url = rpc_url.to_string();
        let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

        // Fetch initial blockhash
        let blockhash = rpc.get_latest_blockhash()
            .unwrap_or_else(|e| {
                tracing::warn!("initial blockhash fetch failed: {e}, using default");
                solana_sdk::hash::Hash::default()
            });

        let cache = Self {
            inner: Arc::new(RwLock::new(CachedBlockhash {
                blockhash,
                fetched_at: std::time::Instant::now(),
            })),
            rpc_url: rpc_url.clone(),
        };

        // Spawn background refresh task
        let inner = cache.inner.clone();
        tokio::spawn(async move {
            let rpc = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                match rpc.get_latest_blockhash() {
                    Ok(bh) => {
                        let mut cached = inner.write().await;
                        *cached = CachedBlockhash {
                            blockhash: bh,
                            fetched_at: std::time::Instant::now(),
                        };
                    }
                    Err(e) => {
                        tracing::warn!("blockhash refresh failed: {e}");
                    }
                }
            }
        });

        cache
    }

    /// Get cached blockhash (instant, no RPC call).
    pub async fn get(&self) -> solana_sdk::hash::Hash {
        self.inner.read().await.blockhash
    }
}

// ---------------------------------------------------------------------------
// Fire-and-forget transaction submission
// ---------------------------------------------------------------------------

/// Submit a transaction without waiting for confirmation (fire-and-forget).
///
/// Returns the signature immediately. The caller can poll for confirmation
/// separately or just log it. This is ~4 seconds faster than send_and_confirm.
pub async fn submit_transaction(
    rpc_url: &str,
    keypair: &Keypair,
    instructions: &[Instruction],
    priority_fee_microlamports: u64,
    compute_unit_limit: u32,
    blockhash_cache: &BlockhashCache,
) -> Result<ExecutionResult, ExecutorError> {
    let rpc = RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());

    // Build compute budget instructions
    let mut all_ixs: Vec<Instruction> = Vec::with_capacity(instructions.len() + 2);
    all_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit));
    if priority_fee_microlamports > 0 {
        all_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(priority_fee_microlamports));
    }
    all_ixs.extend_from_slice(instructions);

    // Use cached blockhash (instant, no RPC call)
    let blockhash = blockhash_cache.get().await;

    // Build and sign
    let tx = Transaction::new_signed_with_payer(
        &all_ixs,
        Some(&keypair.pubkey()),
        &[keypair],
        blockhash,
    );

    // Fire-and-forget: send without waiting for confirmation
    let signature = rpc
        .send_transaction(&tx)
        .map_err(|e| ExecutorError::TransactionFailed(format!("tx send failed: {e}")))?;

    let executed_at = chrono::Utc::now().to_rfc3339();

    tracing::info!(
        signature = %signature,
        "transaction submitted (fire-and-forget)"
    );

    Ok(ExecutionResult {
        tx_signature: signature.to_string(),
        confirmed: false, // Not confirmed yet, fire-and-forget
        actual_sol_lamports: 0,
        actual_token_amount: 0,
        error: None,
        executed_at,
    })
}

// ---------------------------------------------------------------------------
// Jito bundle submission
// ---------------------------------------------------------------------------

/// Jito bundle tip accounts (round-robin).
const JITO_TIP_ACCOUNTS: &[&str] = &[
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4bVqkfRtQ7NmXwkiNPLYkNs",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSLJn2HiHrEZeMKNd7R",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// Submit a transaction as a Jito bundle for MEV protection.
///
/// Sends to Jito's block engine endpoint with a tip for priority inclusion.
pub async fn submit_jito_bundle(
    rpc_url: &str,
    keypair: &Keypair,
    instructions: &[Instruction],
    priority_fee_microlamports: u64,
    compute_unit_limit: u32,
    tip_lamports: u64,
    blockhash_cache: &BlockhashCache,
) -> Result<ExecutionResult, ExecutorError> {
    let _rpc = RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());

    // Build compute budget + tip instructions
    let mut all_ixs: Vec<Instruction> = Vec::with_capacity(instructions.len() + 3);
    all_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit));
    if priority_fee_microlamports > 0 {
        all_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(priority_fee_microlamports));
    }

    // Add Jito tip instruction
    if tip_lamports > 0 {
        let tip_account: Pubkey = {
            let idx = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as usize % JITO_TIP_ACCOUNTS.len();
            JITO_TIP_ACCOUNTS[idx].parse().unwrap()
        };
        all_ixs.push(solana_sdk::system_instruction::transfer(
            &keypair.pubkey(),
            &tip_account,
            tip_lamports,
        ));
    }

    all_ixs.extend_from_slice(instructions);

    let blockhash = blockhash_cache.get().await;
    let tx = Transaction::new_signed_with_payer(
        &all_ixs,
        Some(&keypair.pubkey()),
        &[keypair],
        blockhash,
    );

    // Send to Jito endpoint (or fall back to regular RPC)
    // Jito accepts bundles via JSON-RPC at their block engine
    let jito_url = "https://mainnet.block-engine.jito.wtf/api/v1/bundles";
    let serialized = bincode::serialize(&tx)
        .map_err(|e| ExecutorError::TransactionFailed(format!("serialize failed: {e}")))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&serialized);

    let client = reqwest::Client::new();
    let resp = client
        .post(jito_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [[encoded]]
        }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let signature = tx.signatures[0];
            tracing::info!(signature = %signature, "jito bundle submitted");
            Ok(ExecutionResult {
                tx_signature: signature.to_string(),
                confirmed: false,
                actual_sol_lamports: 0,
                actual_token_amount: 0,
                error: None,
                executed_at: chrono::Utc::now().to_rfc3339(),
            })
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "jito bundle failed, falling back to RPC");
            // Fall back to regular RPC
            submit_transaction(rpc_url, keypair, instructions, priority_fee_microlamports, compute_unit_limit, blockhash_cache).await
        }
        Err(e) => {
            tracing::warn!(error = %e, "jito request failed, falling back to RPC");
            submit_transaction(rpc_url, keypair, instructions, priority_fee_microlamports, compute_unit_limit, blockhash_cache).await
        }
    }
}

// ---------------------------------------------------------------------------
// Slippage helpers
// ---------------------------------------------------------------------------

/// Compute the minimum output after slippage.
pub fn min_output_after_slippage(amount: u64, slippage_bps: u16) -> u64 {
    let factor = 10_000u64.saturating_sub(slippage_bps as u64);
    amount.saturating_mul(factor) / 10_000
}

/// Compute the maximum input allowed after slippage.
pub fn max_input_after_slippage(amount: u64, slippage_bps: u16) -> u64 {
    let factor = 10_000u64.saturating_add(slippage_bps as u64);
    amount.saturating_mul(factor) / 10_000
}

// ---------------------------------------------------------------------------
// Idempotent ATA creation
// ---------------------------------------------------------------------------

/// SPL Associated Token Account program ID.
const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// Token-2022 program ID (Token Extensions).
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Build an idempotent create-ATA instruction (no-op if already exists).
///
/// `token_program_id` should be `spl_token::id()` for legacy tokens or
/// `TOKEN_2022_PROGRAM_ID` for Token-2022 tokens. PumpFun/PumpSwap tokens
/// use Token-2022 since late 2024.
pub fn create_ata_idempotent_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program_id: &Pubkey,
) -> Instruction {
    let ata = spl_associated_token_account::get_associated_token_address_with_program_id(
        owner, mint, token_program_id,
    );
    let ata_program: Pubkey = ATA_PROGRAM_ID.parse().unwrap();
    let system_program = solana_sdk::system_program::id();

    Instruction {
        program_id: ata_program,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new_readonly(*token_program_id, false),
        ],
        data: vec![1], // CreateIdempotent
    }
}

/// Get the Token-2022 program ID.
pub fn token_2022_program_id() -> Pubkey {
    TOKEN_2022_PROGRAM_ID.parse().unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_output_zero_slippage() {
        assert_eq!(min_output_after_slippage(1_000_000, 0), 1_000_000);
    }

    #[test]
    fn min_output_5_percent_slippage() {
        assert_eq!(min_output_after_slippage(1_000_000, 500), 950_000);
    }

    #[test]
    fn max_input_zero_slippage() {
        assert_eq!(max_input_after_slippage(1_000_000, 0), 1_000_000);
    }

    #[test]
    fn max_input_5_percent_slippage() {
        assert_eq!(max_input_after_slippage(1_000_000, 500), 1_050_000);
    }
}
