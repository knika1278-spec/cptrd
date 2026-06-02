//! Wallet / keypair management.
//!
//! Loads a Solana keypair from:
//! 1. Environment variable `PRIVATE_KEY_BOT` (base58 string) — preferred
//! 2. JSON file (Solana CLI format: `[byte, byte, ...]`)
//! 3. Base58-encoded file content

use anyhow::Context;
use solana_sdk::signature::Keypair;

/// Load a keypair from `PRIVATE_KEY_BOT` env var.
///
/// Returns error if env var is missing or invalid.
pub fn load_keypair() -> anyhow::Result<Keypair> {
    let pk = std::env::var("PRIVATE_KEY_BOT")
        .context("PRIVATE_KEY_BOT env var not set")?;

    if pk.trim().is_empty() || pk.trim() == "REPLACE_WITH_YOUR_PRIVATE_KEY_BASE58" {
        anyhow::bail!("PRIVATE_KEY_BOT env var is empty or placeholder");
    }

    tracing::info!("loading keypair from PRIVATE_KEY_BOT env var");
    keypair_from_base58(pk.trim())
}

/// Load a keypair from a base58-encoded string.
fn keypair_from_base58(b58: &str) -> anyhow::Result<Keypair> {
    let bytes = bs58::decode(b58)
        .into_vec()
        .context("PRIVATE_KEY_BOT is not valid base58")?;
    keypair_from_bytes(&bytes)
}

/// Load a keypair from a file path.
///
/// Supports two formats:
/// 1. JSON array: `[1, 2, 3, ...]` (Solana CLI default)
/// 2. Base58-encoded string
#[allow(dead_code)]
fn load_keypair_from_file(path: &str) -> anyhow::Result<Keypair> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read keypair file: {path}"))?;

    let trimmed = content.trim();

    // Try parsing as a JSON array of bytes
    if trimmed.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(trimmed)
            .with_context(|| format!("failed to parse keypair JSON array from: {path}"))?;
        return keypair_from_bytes(&bytes);
    }

    // Try as base58-encoded string
    let bytes = bs58::decode(trimmed)
        .into_vec()
        .context("keypair is neither valid JSON array nor valid base58")?;
    keypair_from_bytes(&bytes)
}

/// Construct a Keypair from raw bytes.
fn keypair_from_bytes(bytes: &[u8]) -> anyhow::Result<Keypair> {
    if bytes.len() == 64 {
        Keypair::try_from(bytes).context("invalid keypair bytes")
    } else if bytes.len() == 32 {
        anyhow::bail!(
            "32-byte seed format not supported; provide a 64-byte keypair \
             (Solana CLI id.json format or base58-encoded private key)"
        )
    } else {
        anyhow::bail!(
            "keypair must be 64 bytes, got {}",
            bytes.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::Signer;

    #[test]
    fn load_keypair_from_json_array() {
        let kp = Keypair::new();
        let bytes = kp.to_bytes();
        let json = serde_json::to_string(&bytes.to_vec()).unwrap();

        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, json.as_bytes()).unwrap();

        let loaded = load_keypair_from_file(file.path().to_str().unwrap()).expect("loads keypair");
        assert_eq!(loaded.pubkey(), kp.pubkey());
    }

    #[test]
    fn load_keypair_from_base58_file() {
        let kp = Keypair::new();
        let b58 = bs58::encode(kp.to_bytes()).into_string();

        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b58.as_bytes()).unwrap();

        let loaded = load_keypair_from_file(file.path().to_str().unwrap()).expect("loads keypair");
        assert_eq!(loaded.pubkey(), kp.pubkey());
    }

    #[test]
    fn load_keypair_rejects_missing_file() {
        assert!(load_keypair_from_file("/nonexistent/path/id.json").is_err());
    }

    #[test]
    fn load_keypair_rejects_invalid_json() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"not json at all").unwrap();

        assert!(load_keypair_from_file(file.path().to_str().unwrap()).is_err());
    }
}
