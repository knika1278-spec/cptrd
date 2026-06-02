//! Shared, DEX-agnostic decode helpers.
//!
//! These pure functions extract the trader, traded mint, and balance deltas
//! from a Helius `transactionNotification` (jsonParsed). Per
//! `docs/dex-reference.md` §0, amounts come from `meta` balance deltas — never
//! from instruction args — and decimals come from on-chain token balances.

use std::collections::HashSet;

use crate::models::types::{TxMessage, TxMeta, TxResult};

/// Wrapped SOL mint (string form, per §0 constants).
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Effective ordered account pubkeys for a (possibly v0) transaction.
///
/// Order: static `message.account_keys`, then `meta.loaded_addresses.writable`,
/// then `.readonly`. This index space aligns with `meta.pre/post_balances` and
/// with `TokenBalance.account_index`.
pub fn resolved_account_keys(result: &TxResult) -> Vec<String> {
    let message = &result.transaction.transaction.message;
    let meta = &result.transaction.meta;

    let mut keys: Vec<String> = message
        .account_keys
        .iter()
        .map(|k| k.pubkey.clone())
        .collect();

    if let Some(loaded) = &meta.loaded_addresses {
        keys.extend(loaded.writable.iter().cloned());
        keys.extend(loaded.readonly.iter().cloned());
    }

    keys
}

/// Find the trading whale: first `account_keys` signer in `whales`. Falls back
/// to the first signer when no signer is a whale (so decoding still works in
/// tests / with an empty whale set). `None` if there are no signers.
pub fn find_trader(msg: &TxMessage, whales: &HashSet<String>) -> Option<(usize, String)> {
    let mut first_signer: Option<(usize, String)> = None;

    for (idx, key) in msg.account_keys.iter().enumerate() {
        if !key.signer {
            continue;
        }
        if first_signer.is_none() {
            first_signer = Some((idx, key.pubkey.clone()));
        }
        if whales.contains(&key.pubkey) {
            return Some((idx, key.pubkey.clone()));
        }
    }

    first_signer
}

/// SOL lamport delta for an account index: `post - pre`. `None` if the index is
/// out of range or the balance arrays are empty.
pub fn sol_lamport_delta(meta: &TxMeta, account_index: usize) -> Option<i64> {
    let pre = meta.pre_balances.get(account_index)?;
    let post = meta.post_balances.get(account_index)?;
    // Subtract in i128 so an out-of-range `pre` can't flip the delta's sign;
    // lamports are bounded well below i64::MAX, so narrowing is safe.
    let delta = (*post as i128) - (*pre as i128);
    Some(delta as i64)
}

/// Net SOL the `owner` (at `account_index` in the resolved key list) moved in a tx, in lamports
/// (signed; negative = net spent). Returns the larger-in-magnitude of the native lamport delta and the
/// WSOL token delta (WSOL has 9 decimals, so its raw token delta is already lamports). AMMs route SOL as
/// wrapped SOL while bonding curves use native SOL, and neither signal alone is reliable, so we pick the
/// dominant one. None if neither signal is available.
pub fn sol_movement_lamports(meta: &TxMeta, account_index: usize, owner: &str) -> Option<i64> {
    let native = sol_lamport_delta(meta, account_index);
    let wsol = wsol_delta_for_owner(meta, owner);
    match (native, wsol) {
        (None, None) => None,
        (Some(n), None) => Some(n),
        (None, Some(w)) => i64::try_from(w).ok(),
        (Some(n), Some(w)) => {
            if w.unsigned_abs() > (n as i128).unsigned_abs() {
                i64::try_from(w).ok()
            } else {
                Some(n)
            }
        }
    }
}

/// Parse a token amount from the decimal `amount` STRING (not the float
/// `ui_amount`, which loses precision).
fn parse_amount(amount: &str) -> Option<i128> {
    amount.parse::<i128>().ok()
}

/// (post - pre) raw token-amount delta and decimals for `owner` + `mint`.
///
/// Matches `TokenBalance` entries with `owner == Some(owner)` AND `mint == mint`
/// across pre/post. A missing side (e.g. a newly created ATA) counts as 0.
/// `None` if neither side has a matching entry.
pub fn token_delta(meta: &TxMeta, owner: &str, mint: &str) -> Option<(i128, u8)> {
    let matches = |b: &&crate::models::types::TokenBalance| {
        b.owner.as_deref() == Some(owner) && b.mint == mint
    };

    let pre = meta.pre_token_balances.iter().find(matches);
    let post = meta.post_token_balances.iter().find(matches);

    if pre.is_none() && post.is_none() {
        return None;
    }

    let decimals = post
        .or(pre)
        .map(|b| b.ui_token_amount.decimals)
        .unwrap_or(0);

    let pre_amount = pre
        .and_then(|b| parse_amount(&b.ui_token_amount.amount))
        .unwrap_or(0);
    let post_amount = post
        .and_then(|b| parse_amount(&b.ui_token_amount.amount))
        .unwrap_or(0);

    Some((post_amount - pre_amount, decimals))
}

/// The non-WSOL mint owned by `owner` with the largest absolute nonzero delta.
/// Identifies the token being bought/sold. Returns `(mint, delta, decimals)`.
/// `None` if there is no non-WSOL token movement for that owner.
pub fn traded_mint_for_owner(meta: &TxMeta, owner: &str) -> Option<(String, i128, u8)> {
    // Collect every non-WSOL mint owned by this owner across pre/post balances.
    let mut mints: Vec<String> = meta
        .pre_token_balances
        .iter()
        .chain(meta.post_token_balances.iter())
        .filter(|b| b.owner.as_deref() == Some(owner) && b.mint != WSOL_MINT)
        .map(|b| b.mint.clone())
        .collect();
    mints.sort();
    mints.dedup();

    let mut best: Option<(String, i128, u8)> = None;

    for mint in mints {
        if let Some((delta, decimals)) = token_delta(meta, owner, &mint) {
            if delta == 0 {
                continue;
            }
            let is_better = match &best {
                Some((_, best_delta, _)) => delta.abs() > best_delta.abs(),
                None => true,
            };
            if is_better {
                best = Some((mint, delta, decimals));
            }
        }
    }

    best
}

/// First 8 bytes of a base58-encoded instruction `data` field (the Anchor-style
/// discriminator). `None` if decode fails or fewer than 8 bytes are present.
pub fn discriminator(data_base58: &str) -> Option<[u8; 8]> {
    let bytes = bs58::decode(data_base58).into_vec().ok()?;
    let disc: [u8; 8] = bytes.get(0..8)?.try_into().ok()?;
    Some(disc)
}

/// (post - pre) WSOL token-balance delta for `owner` (used for Meteora swap
/// direction). `None` if the owner has no WSOL balance entry.
pub fn wsol_delta_for_owner(meta: &TxMeta, owner: &str) -> Option<i128> {
    token_delta(meta, owner, WSOL_MINT).map(|(delta, _)| delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::{
        AccountKey, TokenBalance, TxEnvelope, TxInner, TxMessage, TxMeta, UiTokenAmount,
    };

    fn account_key(pubkey: &str, signer: bool) -> AccountKey {
        AccountKey {
            pubkey: pubkey.to_string(),
            signer,
            writable: true,
        }
    }

    fn token_balance(
        index: u64,
        mint: &str,
        owner: &str,
        amount: &str,
        decimals: u8,
    ) -> TokenBalance {
        TokenBalance {
            account_index: index,
            mint: mint.to_string(),
            owner: Some(owner.to_string()),
            ui_token_amount: UiTokenAmount {
                amount: amount.to_string(),
                decimals,
                ui_amount: None,
            },
        }
    }

    fn empty_meta() -> TxMeta {
        TxMeta {
            err: None,
            fee: 0,
            pre_balances: vec![],
            post_balances: vec![],
            pre_token_balances: vec![],
            post_token_balances: vec![],
            inner_instructions: vec![],
            log_messages: None,
            loaded_addresses: None,
        }
    }

    #[test]
    fn discriminator_decodes_known_base58_to_eight_bytes() {
        let raw = [102u8, 6, 61, 18, 1, 218, 235, 234];
        let encoded = bs58::encode(&raw).into_string();
        assert_eq!(discriminator(&encoded), Some(raw));
    }

    #[test]
    fn discriminator_returns_none_on_too_short_input() {
        let short = bs58::encode(&[1u8, 2, 3]).into_string();
        assert_eq!(discriminator(&short), None);
    }

    #[test]
    fn sol_lamport_delta_computes_post_minus_pre() {
        let mut meta = empty_meta();
        meta.pre_balances = vec![10_000_000_000, 0];
        meta.post_balances = vec![6_500_000_000, 0];
        assert_eq!(sol_lamport_delta(&meta, 0), Some(-3_500_000_000));
        assert_eq!(sol_lamport_delta(&meta, 1), Some(0));
    }

    #[test]
    fn sol_lamport_delta_returns_none_out_of_range() {
        let mut meta = empty_meta();
        meta.pre_balances = vec![100];
        meta.post_balances = vec![200];
        assert_eq!(sol_lamport_delta(&meta, 5), None);
    }

    #[test]
    fn token_delta_computes_a_buy() {
        let owner = "Whale1";
        let mint = "Mint1";
        let mut meta = empty_meta();
        meta.pre_token_balances = vec![token_balance(0, mint, owner, "0", 6)];
        meta.post_token_balances = vec![token_balance(0, mint, owner, "1000000", 6)];
        assert_eq!(token_delta(&meta, owner, mint), Some((1_000_000, 6)));
    }

    #[test]
    fn token_delta_computes_a_sell() {
        let owner = "Whale1";
        let mint = "Mint1";
        let mut meta = empty_meta();
        meta.pre_token_balances = vec![token_balance(0, mint, owner, "5000000", 9)];
        meta.post_token_balances = vec![token_balance(0, mint, owner, "1000000", 9)];
        assert_eq!(token_delta(&meta, owner, mint), Some((-4_000_000, 9)));
    }

    #[test]
    fn token_delta_with_only_post_treats_missing_pre_as_zero() {
        let owner = "Whale1";
        let mint = "Mint1";
        let mut meta = empty_meta();
        // New ATA / first buy: no matching pre entry, only post.
        meta.post_token_balances = vec![token_balance(0, mint, owner, "1000000", 6)];
        assert_eq!(token_delta(&meta, owner, mint), Some((1_000_000, 6)));
    }

    #[test]
    fn traded_mint_picks_non_wsol_and_ignores_wsol_movement() {
        let owner = "Whale1";
        let mint = "Mint1";
        let mut meta = empty_meta();
        meta.pre_token_balances = vec![
            token_balance(0, mint, owner, "0", 6),
            token_balance(1, WSOL_MINT, owner, "9000000000", 9),
        ];
        meta.post_token_balances = vec![
            token_balance(0, mint, owner, "1000000", 6),
            token_balance(1, WSOL_MINT, owner, "5000000000", 9),
        ];
        let (got_mint, delta, decimals) = traded_mint_for_owner(&meta, owner).expect("traded mint");
        assert_eq!(got_mint, mint);
        assert_eq!(delta, 1_000_000);
        assert_eq!(decimals, 6);
    }

    #[test]
    fn wsol_delta_for_owner_reads_wsol_movement() {
        let owner = "Whale1";
        let mut meta = empty_meta();
        meta.pre_token_balances = vec![token_balance(0, WSOL_MINT, owner, "9000000000", 9)];
        meta.post_token_balances = vec![token_balance(0, WSOL_MINT, owner, "5000000000", 9)];
        assert_eq!(wsol_delta_for_owner(&meta, owner), Some(-4_000_000_000));
    }

    #[test]
    fn sol_movement_returns_native_when_no_wsol() {
        let mut meta = empty_meta();
        meta.pre_balances = vec![10_000_000_000];
        meta.post_balances = vec![6_500_000_000];
        // No WSOL balance for the owner → native delta wins by default.
        assert_eq!(
            sol_movement_lamports(&meta, 0, "Whale1"),
            Some(-3_500_000_000)
        );
    }

    #[test]
    fn sol_movement_returns_wsol_when_no_native() {
        let owner = "Whale1";
        let mut meta = empty_meta();
        // Balance arrays empty → native is None; only WSOL movement present.
        meta.pre_token_balances = vec![token_balance(0, WSOL_MINT, owner, "9000000000", 9)];
        meta.post_token_balances = vec![token_balance(0, WSOL_MINT, owner, "5000000000", 9)];
        assert_eq!(
            sol_movement_lamports(&meta, 0, owner),
            Some(-4_000_000_000)
        );
    }

    #[test]
    fn sol_movement_picks_wsol_when_larger_magnitude() {
        let owner = "Whale1";
        let mut meta = empty_meta();
        // Native delta is only a tiny fee; WSOL routes the real SOL (AMM trade).
        meta.pre_balances = vec![1_000_000_000];
        meta.post_balances = vec![999_995_000]; // -5_000 fee
        meta.pre_token_balances = vec![token_balance(0, WSOL_MINT, owner, "3000000000", 9)];
        meta.post_token_balances = vec![token_balance(0, WSOL_MINT, owner, "1000000000", 9)];
        // WSOL delta = -2_000_000_000, magnitude beats -5_000.
        assert_eq!(
            sol_movement_lamports(&meta, 0, owner),
            Some(-2_000_000_000)
        );
    }

    #[test]
    fn sol_movement_picks_native_when_larger_magnitude() {
        let owner = "Whale1";
        let mut meta = empty_meta();
        // Bonding curve: native SOL is the real movement; WSOL barely moves.
        meta.pre_balances = vec![10_000_000_000];
        meta.post_balances = vec![6_500_000_000]; // -3_500_000_000
        meta.pre_token_balances = vec![token_balance(0, WSOL_MINT, owner, "1000", 9)];
        meta.post_token_balances = vec![token_balance(0, WSOL_MINT, owner, "0", 9)];
        // Native magnitude (3.5e9) beats WSOL (-1_000).
        assert_eq!(
            sol_movement_lamports(&meta, 0, owner),
            Some(-3_500_000_000)
        );
    }

    #[test]
    fn sol_movement_preserves_positive_sign_on_sell() {
        let owner = "Whale1";
        let mut meta = empty_meta();
        // Sell side: WSOL received (positive) dominates the small native fee.
        meta.pre_balances = vec![1_000_000_000];
        meta.post_balances = vec![999_995_000]; // -5_000 fee
        meta.pre_token_balances = vec![token_balance(0, WSOL_MINT, owner, "0", 9)];
        meta.post_token_balances = vec![token_balance(0, WSOL_MINT, owner, "2000000000", 9)];
        assert_eq!(
            sol_movement_lamports(&meta, 0, owner),
            Some(2_000_000_000)
        );
    }

    #[test]
    fn find_trader_picks_whale_signer_when_present() {
        let msg = TxMessage {
            account_keys: vec![
                account_key("Payer1", true),
                account_key("Whale1", true),
                account_key("Readonly1", false),
            ],
            instructions: vec![],
        };
        let mut whales = HashSet::new();
        whales.insert("Whale1".to_string());
        assert_eq!(find_trader(&msg, &whales), Some((1, "Whale1".to_string())));
    }

    #[test]
    fn find_trader_falls_back_to_first_signer_with_empty_whale_set() {
        let msg = TxMessage {
            account_keys: vec![
                account_key("Reader", false),
                account_key("Signer1", true),
                account_key("Signer2", true),
            ],
            instructions: vec![],
        };
        let whales: HashSet<String> = HashSet::new();
        assert_eq!(find_trader(&msg, &whales), Some((1, "Signer1".to_string())));
    }

    #[test]
    fn find_trader_returns_none_with_no_signers() {
        let msg = TxMessage {
            account_keys: vec![account_key("Reader", false)],
            instructions: vec![],
        };
        let whales: HashSet<String> = HashSet::new();
        assert_eq!(find_trader(&msg, &whales), None);
    }

    #[test]
    fn resolved_account_keys_appends_loaded_addresses() {
        use crate::models::types::LoadedAddresses;
        let mut meta = empty_meta();
        meta.loaded_addresses = Some(LoadedAddresses {
            writable: vec!["W1".to_string()],
            readonly: vec!["R1".to_string()],
        });
        let result = TxResult {
            signature: "sig".to_string(),
            slot: 1,
            transaction: TxEnvelope {
                transaction: TxInner {
                    message: TxMessage {
                        account_keys: vec![account_key("Static0", true)],
                        instructions: vec![],
                    },
                },
                meta,
            },
        };
        assert_eq!(
            resolved_account_keys(&result),
            vec!["Static0".to_string(), "W1".to_string(), "R1".to_string()]
        );
    }
}
