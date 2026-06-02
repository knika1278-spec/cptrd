```markdown
# Solana Copytrade Bot — Phase 1: WSS Tracking + Per-DEX Decode

## ⚠️ CRITICAL RULE — NO HALLUCINATION
Sebelum menulis decoder APAPUN, WAJIB research terlebih dahulu:
- Cari program ID resmi dari on-chain / official docs / GitHub repo masing-masing DEX
- Cari instruction discriminator dan account layout dari SDK terbaru atau IDL terbaru
- JANGAN menggunakan data dari training data lama — Solana program sering di-update
- Jika tidak yakin, tulis placeholder dengan comment `// TODO: VERIFY against on-chain data` dan berikan link sumber yang perlu dicek
- Tunjukkan sumber referensi yang kamu pakai di comment setiap file decoder

## Overview
Bangun bot copytrading Solana dalam bahasa **Rust**. Fase 1 hanya sampai:
1. Load whale addresses dari static JSON
2. Koneksi ke Helius WSS (`transactionSubscribe`)
3. Decode transaksi whale per-DEX protocol
4. Identifikasi: action (buy/sell), mint (token address), SOL amount, token amount
5. Apply basic security guards (bot filter + SOL threshold)
6. Output ke JSON file untuk verifikasi

**TIDAK ADA eksekusi transaksi di fase ini.** Tidak perlu re-derive ATA atau build instruction.

## Architecture

```
solana-copytrade-bot/
├── Cargo.toml
├── config/
│   └── whales.json
├── src/
│   ├── main.rs                    # Entry point, orchestration
│   ├── helius/
│   │   ├── mod.rs
│   │   └── wss.rs                 # WebSocket connection & subscription
│   ├── decoder/
│   │   ├── mod.rs                 # Router: match program_id → decoder
│   │   ├── decode_pumpfun.rs      # PumpFun bonding curve decoder
│   │   ├── decode_pumpswap.rs     # PumpSwap AMM decoder
│   │   ├── decode_raydium_launchpad.rs  # Raydium Launchpad decoder
│   │   └── decode_meteora_dlmm.rs # Meteora DLMM v2 decoder
│   ├── guard/
│   │   ├── mod.rs
│   │   ├── bot_filter.rs          # Guard 1: detect arbitrage bots
│   │   └── threshold_filter.rs    # Guard 2: minimum SOL threshold
│   ├── models/
│   │   ├── mod.rs
│   │   └── types.rs               # Shared types & structs
│   └── output/
│       ├── mod.rs
│       └── json_writer.rs         # Write decoded events to JSON
├── output/
│   └── decoded_events.json        # Generated output file
```

## Data Structures (models/types.rs)

```rust
use serde::{Deserialize, Serialize};

// === Config ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleConfig {
    pub address: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BotConfig {
    pub helius_api_key: String,
    pub helius_wss_url: String,  // wss://atlas-mainnet.helius-rpc.com/?api-key=<KEY>
    pub whales: Vec<WhaleConfig>,
    pub min_sol_threshold: f64,  // default: 2.0
}

// === Decoded Event ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedTradeEvent {
    pub signature: String,
    pub slot: u64,
    pub timestamp: Option<i64>,
    pub whale_address: String,
    pub dex: DexProtocol,
    pub action: TradeAction,
    pub mint: String,              // token mint address
    pub sol_amount: f64,           // SOL lamports converted to SOL
    pub token_amount: u64,         // raw token amount
    pub token_decimals: u8,
    pub is_bot_whale: bool,        // Guard 1 result
    pub passed_threshold: bool,    // Guard 2 result
    pub guard_skip_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DexProtocol {
    PumpFun,
    PumpSwap,
    RaydiumLaunchpad,
    MeteoraDlmmV2,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TradeAction {
    Buy,   // SOL → Token
    Sell,  // Token → SOL
}

// === Helius WSS Response ===

#[derive(Debug, Deserialize)]
pub struct HeliusTransactionNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: HeliusTransactionParams,
}

#[derive(Debug, Deserialize)]
pub struct HeliusTransactionParams {
    pub subscription: u64,
    pub result: HeliusTransactionResult,
}

// NOTE: Struct di bawah harus diverifikasi terhadap response aktual Helius
// Ref: https://docs.helius.dev/solana-apis/websocket-api
#[derive(Debug, Deserialize)]
pub struct HeliusTransactionResult {
    pub signature: String,
    // Field lain tergantung enhanced/non-enhanced mode
    // Lakukan research terhadap format response aktual
}
```

## Component Requirements

### 1. Helius WSS (helius/wss.rs)

- Connect ke Helius WebSocket: `wss://atlas-mainnet.helius-rpc.com/?api-key=<API_KEY>`
- Subscribe menggunakan method `transactionSubscribe`
- `accountInclude`: array semua address dari whales.json
- Include semua whale address dalam satu subscription ATAU per-whale subscription (pilih yang paling efisien)
- Handle reconnect logic (exponential backoff)
- Parse incoming JSON notification menjadi `HeliusTransactionNotification`
- Forward raw transaction data ke decoder router

### 2. Decoder Router (decoder/mod.rs)

- Terima raw transaction data dari Helius
- Iterasi instructions dalam transaction
- Match `program_id` dari setiap instruction terhadap DEX program yang diketahui:
  - PumpFun program ID → `decode_pumpfun::decode()`
  - PumpSwap program ID → `decode_pumpswap::decode()`
  - Raydium Launchpad program ID → `decode_raydium_launchpad::decode()`
  - Meteora DLMM v2 program ID → `decode_meteora_dlmm::decode()`
- Jika tidak match dengan DEX manapun, skip (return None)
- Return `Option<DecodedTradeEvent>`

### 3. Per-DEX Decoders

Setiap file decoder HARUS:
- Research program ID resmi (jangan hardcode dari ingatan)
- Research instruction discriminator terbaru
- Research account layout (akun mana yang merupakan wallet, mint, dll)
- Extract: TradeAction (buy/sell), mint address, SOL amount, token amount
- Handle error gracefully — jika instruction tidak bisa di-decode, log warning dan return None
- Tulis comment dengan link sumber referensi yang dipakai

**decode_pumpfun.rs:**
- Program: PumpFun bonding curve
- Instruction: buy / sell
- Accounts: bonding curve, mint, user, system program, dll
- Kalkulasi SOL amount dari instruction data atau account balance changes

**decode_pumpswap.rs:**
- Program: PumpSwap (AMM dari PumpFun)
- Instruction: buy / sell swap
- Accounts: pool, mint, user, vault, dll

**decode_raydium_launchpad.rs:**
- Program: Raydium Launchpad (BUKAN Raydium AMM biasa)
- Instruction: buy / sell pada launchpad
- Note: pastikan ini launchpad, bukan AMM V4 atau CLMM

**decode_meteora_dlmm.rs:**
- Program: Meteora DLMM v2
- Instruction: swap / addLiquidity / removeLiquidity
- Yang perlu dideteksi: swap (buy/sell), bukan liquidity operations
- Kalkulasi arah trade berdasarkan token input/output

### 4. Guards (guard/)

**bot_filter.rs — Guard 1:**
- Deteksi apakah 1 signature mengandung BOTH buy DAN sell instruction untuk token yang sama
- Jika ya → ini arbitrage bot → flag `is_bot_whale = true`
- Juga bisa deteksi: terlalu banyak instruction dalam 1 tx (>4 swap instructions)
- Bot whale akan di-log ke file terpisah: `output/bot_whales.json` untuk blacklist

**threshold_filter.rs — Guard 2:**
- Cek apakah `sol_amount >= min_sol_threshold` (default 2.0 SOL)
- Jika tidak → `passed_threshold = false`, isi `guard_skip_reason`
- Hanya apply ke BUY action (untuk SELL, selalu passed)

### 5. Output (output/json_writer.rs)

- Setiap `DecodedTradeEvent` yang lolos decode ditulis ke `output/decoded_events.json`
- Format: JSON array (append mode)
- Juga tulis events yang TIDAK passed guard ke `output/skipped_events.json` (untuk debugging)
- Bot whale detections ke `output/bot_whales.json`
- Tambahkan field `decoded_at` (ISO 8601 timestamp) ke setiap entry

### 6. Main (main.rs)

- Load `config/whales.json` → parse ke `Vec<WhaleConfig>`
- Load environment variables (HELIUS_API_KEY) atau dari config
- Inisialisasi semua decoder
- Connect ke Helius WSS
- Loop: receive → decode → guard filter → output
- Graceful shutdown on SIGINT/SIGTERM

## whales.json Format

```json
{
  "helius_api_key": "YOUR_KEY_HERE",
  "min_sol_threshold": 2.0,
  "whales": [
    {
      "address": "AbCd...1234",
      "label": "whale-alpha"
    },
    {
      "address": "EfGh...5678",
      "label": "whale-beta"
    }
  ]
}
```

## Dependencies (Cargo.toml)

```toml
[dependencies]
solana-sdk = "2.2"
solana-transaction-status = "2.2"
spl-token = "7"
spl-associated-token-account = "6"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json"] }
dotenv = "0.15"
tracing = "0.1"
tracing-subscriber = "0.3"
chrono = "0.4"
```

## Execution Order

1. Buat project structure dan Cargo.toml
2. Implement `models/types.rs` dengan semua struct
3. **Research dulu**: Untuk setiap DEX, cari program ID dan instruction layout terbaru
4. Implement decoder satu per satu (mulai dari PumpFun karena paling sering dipakai)
5. Implement `helius/wss.rs` — koneksi + subscription
6. Implement `decoder/mod.rs` — router
7. Implement `guard/` — bot filter + threshold
8. Implement `output/json_writer.rs`
9. Implement `main.rs` — orchestrate semua
10. Test dengan whale address real dan verify output JSON

## Success Criteria

- Bot bisa connect ke Helius WSS dan receive transaction notifications
- Decoder bisa mengenali transaksi dari keempat DEX target
- Setiap decoder menghasilkan `DecodedTradeEvent` yang akurat (verify terhadap Solana Explorer)
- Bot whale terdeteksi dan dipisahkan
- Transaksi < 2 SOL buy di-skip dan logged
- Semua output dalam format JSON yang terstruktur
- Tidak ada hardcode assumption — semua instruction layout dari research aktual
```

---

# Referensi / Dasar Pertimbangan

| Aspek | Dasar |
|-------|-------|
| **Arsitektur per-file decoder** | Sesuai permintaan user: modular, mudah maintain & test per-DEX |
| **Helius WSS transactionSubscribe** | Method resmi Helius untuk subscribe transaksi real-time berdasarkan account. Ref: https://docs.helius.dev |
| **Guards 1 & 2 di fase 1** | Logika filtering pasca-decode yang tidak memerlukan eksekusi transaksi, natural masuk di pipeline decode |
| **Guards 3 & 4 di fase berikutnya** | Guard 3 (copy limit) & Guard 4 (profitability ranking) butuh execution tracking dan time-series analysis — bukan scope fase 1 |
| **NO HALLUCINATION rule** | Instruksi eksplisit user: "wajib research jangan halusinasi, jangan berasumsi". Prompt menekankan research-first dan verifikasi on-chain untuk setiap decoder |
| **JSON output** | User ingin mudah debug dan verifikasi akurasi decode — structured JSON dengan file terpisah (events, skipped, bot_whales) |
| **Solana SDK versions** | Menggunakan versi terbaru solana-sdk 2.x |
| **EVM Token Decimals skill** | Meskipun project ini Solana (bukan EVM), prinsip yang relevan diterapkan: selalu query decimals dari on-chain, jangan hardcode — diterapkan di decoder saat menghitung token_amount |
| **Product Capability skill** | Scope fase 1 didefinisikan dengan jelas: capability boundary antara decode (fase 1) dan execution (fase 2), dengan open questions untuk fase berikutnya dicatat |