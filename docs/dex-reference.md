# DEX & Helius Decode Reference (Verified) — 2026-06-02

Single source of truth for Phase 1 decoders. **Every fact here was verified against a
primary source** (official IDL / official docs / crates.io) on 2026-06-02. Items that could
not be confirmed from a primary source are marked **`UNVERIFIED`** and MUST be validated
against a live on-chain transaction before being trusted.

> Per project rule: do NOT hardcode anything from memory. Use the values in this file.
> If a value is `UNVERIFIED`, leave a `// TODO: VERIFY against on-chain data` comment with
> the source link in the decoder.

---

## 0. Cross-cutting decode strategy (READ FIRST)

All four programs share the same two pitfalls, so all decoders follow the same strategy:

1. **Identify the DEX by program ID, NOT by discriminator.** PumpFun & PumpSwap share the
   exact same `buy`/`sell` discriminators; DLMM & DAMM v2 share the same `swap` discriminator.
   The router matches `instruction.program_id` first, then the decoder confirms the
   discriminator (first 8 bytes of the instruction data).

2. **Amounts come from transaction `meta` balance deltas, NOT from instruction args.**
   Instruction args (`max_sol_cost`, `min_amount_out`, etc.) are slippage *limits*, not actual
   fills. The robust, uniform method across every DEX:
   - **`sol_amount`** = `|postBalances[trader_idx] - preBalances[trader_idx]|` (lamports → SOL by
     `/ 1e9`). For a buy, subtract the tx `fee` attributed to the fee payer to avoid counting it;
     acceptable in Phase 1 to report gross delta and note it.
   - **`token_amount`** = `post - pre` of the trader's token balance for the traded mint, read
     from `meta.preTokenBalances` / `meta.postTokenBalances` entries where
     `owner == trader_wallet` and `mint == traded_mint`.
   - **`token_decimals`** = `uiTokenAmount.decimals` from that same token-balance entry
     (on-chain truth — never hardcode decimals).
   - **`mint`** = the non-WSOL mint involved in the swap.
   - **`action`** = from the discriminator where it is explicit (buy vs sell). For Meteora
     `swap` (direction not in the name), derive from WSOL flow: WSOL leaves trader ⇒ **Buy**;
     WSOL returns to trader ⇒ **Sell**.

3. **Trader wallet** = the tracked whale address (it is the signer in `accountInclude`). When an
   account-index table below names a `user`/`payer`, that index is the trader and should equal
   the whale; cross-check against the whale set rather than blindly trusting the index, because
   optional/lookup-table accounts can shift positions in v0 transactions.

4. **Known constants (string form):**
   - WSOL mint: `So11111111111111111111111111111111111111112`
   - System program: `11111111111111111111111111111111`
   - SPL Token: `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`
   - Token-2022: `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`

5. **Account ordering in v0 txns:** effective order is static `message.accountKeys`, then
   `meta.loadedAddresses.writable`, then `meta.loadedAddresses.readonly`. Resolve account
   pubkeys through this combined list. **`UNVERIFIED`** byte-exact ordering — validate on a live
   notification.

---

## 1. Helius `transactionSubscribe` (Geyser Enhanced WebSocket)

Source: https://www.helius.dev/docs/enhanced-websockets/transaction-subscribe
Business+ Helius plan required.

**Endpoint:** Docs show `wss://mainnet.helius-rpc.com/?api-key=<KEY>`. The roadmap specifies
`wss://atlas-mainnet.helius-rpc.com/?api-key=<KEY>` (the historical Atlas host). These differ.
**`UNVERIFIED` which host is live for your account** → the WSS URL is config/env-driven
(`helius_wss_url` in whales.json, or built from the key). Log the endpoint on connect and adjust.

**Subscribe request** (we use `jsonParsed` encoding — see encoding note):
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "transactionSubscribe",
  "params": [
    { "accountInclude": ["<whale1>", "<whale2>"], "vote": false, "failed": false },
    { "commitment": "confirmed", "encoding": "jsonParsed",
      "transactionDetails": "full", "showRewards": false,
      "maxSupportedTransactionVersion": 0 }
  ]
}
```
- Filter: `accountInclude` (OR logic, ≤50k), `accountExclude`, `accountRequired` (AND), `vote`,
  `failed`, `signature`.
- Options: `commitment` (`processed|confirmed|finalized`), `encoding`
  (`base58|base64|jsonParsed`), `transactionDetails` (`full|signatures|accounts|none`),
  `showRewards`, `maxSupportedTransactionVersion` (set `0`).
- **Encoding choice:** `jsonParsed` gives structured per-instruction
  `{ programId, accounts:[pubkey], data:<base58> }` which is easiest to traverse without a heavy
  decode dependency; unknown-program instruction data still arrives base58-encoded (decode with
  `bs58` to read the discriminator). `base64`/`base58` return the whole tx raw. **For Phase 1 we
  use `jsonParsed`** — simplest path to program_id + accounts + meta.
- Ack: `{ "jsonrpc":"2.0", "result":<subId>, "id":<id> }`. Unsubscribe: `transactionUnsubscribe`,
  `params:[<subId>]`.

**Notification:**
```json
{ "jsonrpc":"2.0", "method":"transactionNotification",
  "params": { "subscription":<num>, "result": {
    "signature":"<str>", "slot":<num>, "transactionIndex":<num>,
    "transaction": {
      "transaction": { "message": { "accountKeys":[{"pubkey":"..","signer":bool,"writable":bool}],
                                     "instructions":[{"programId":"..","accounts":["..."],"data":"<base58>"}] },
                       "signatures":["..."] },
      "meta": {
        "err": null, "fee": <num>,
        "preBalances":[<num>], "postBalances":[<num>],
        "preTokenBalances":[{"accountIndex":<num>,"mint":"..","owner":"..",
                             "uiTokenAmount":{"amount":"<str>","decimals":<num>,"uiAmount":<num|null>}}],
        "postTokenBalances":[ ...same shape... ],
        "innerInstructions":[{"index":<num>,"instructions":[{"programId":"..","accounts":[..],"data":"<base58>"}]}],
        "logMessages":["..."],
        "loadedAddresses": { "writable":["..."], "readonly":["..."] }
      } } } } }
```
Note: with `jsonParsed`, instructions are `UiInstruction` — either `parsed` (known programs) or
`{ programId, accounts, data }` (partially-decoded, our DEX programs). Handle both; we only act on
the partially-decoded ones whose `programId` matches a known DEX. `innerInstructions` carry CPIs
(e.g. token transfers, program self-CPI events) and should also be scanned for the DEX programs,
because aggregators/routers often invoke the DEX via CPI rather than top-level.

---

## 2. PumpFun (bonding curve)

Source: official IDL `github.com/pump-fun/pump-public-docs/idl/pump.json`.

- **Program ID:** `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`
- **Discriminators** (= `sha256("global:<name>")[..8]`):
  - `buy`  = `[102, 6, 61, 18, 1, 218, 235, 234]`
  - `sell` = `[51, 230, 133, 164, 1, 127, 131, 173]`
- **Instruction data after disc:** `buy`: `amount:u64`(tokens out), `max_sol_cost:u64`,
  `track_volume:Option<bool>`(1 byte, newer). `sell`: `amount:u64`(tokens in), `min_sol_output:u64`.
- **Accounts** (buy/sell differ at idx 8/9 — `creator_vault` swaps with `token_program`):

  | idx | buy | sell |
  |----|-----|------|
  | 0 | global | global |
  | 1 | fee_recipient | fee_recipient |
  | 2 | **mint** | **mint** |
  | 3 | bonding_curve | bonding_curve |
  | 4 | associated_bonding_curve | associated_bonding_curve |
  | 5 | associated_user | associated_user |
  | 6 | **user (trader)** | **user (trader)** |
  | 7 | system_program | system_program |
  | 8 | token_program | creator_vault |
  | 9 | creator_vault | token_program |
  | 10 | event_authority | event_authority |
  | 11 | program | program |
  | 12+ | volume accumulators / fee_config / fee_program (newer; **`UNVERIFIED` for historical txs — count, don't hard-index the tail**) | |

  Trader = idx 6. Mint = idx 2.
- **Amounts (most exact):** `TradeEvent` (disc `[189,219,127,211,78,230,97,238]`) emitted via
  self-CPI/log: `{ mint, sol_amount:u64, token_amount:u64, is_buy:bool, user, ... }`. Use `is_buy`
  for direction and the two amounts for exact fills (fees included). Fallback = balance deltas (§0).

---

## 3. PumpSwap (`pump_amm`)

Source: official IDL `github.com/pump-fun/pump-public-docs/idl/pump_amm.json`.

- **Program ID:** `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`
- **Discriminators:** `buy` = `[102,6,61,18,1,218,235,234]`, `sell` = `[51,230,133,164,1,127,131,173]`
  — **identical to PumpFun; disambiguate by program ID.**
- **Instruction data:** `buy`: `base_amount_out:u64`, `max_quote_amount_in:u64`, `track_volume:Option<bool>`.
  `sell`: `base_amount_in:u64`, `min_quote_amount_out:u64`.
- **Accounts:**

  | idx | account |
  |----|---------|
  | 0 | pool |
  | 1 | **user (trader)** |
  | 2 | global_config |
  | 3 | **base_mint** (the token) |
  | 4 | **quote_mint** (typically WSOL) |
  | 5 | user_base_token_account |
  | 6 | user_quote_token_account |
  | 7 | pool_base_token_account |
  | 8 | pool_quote_token_account |
  | 9 | protocol_fee_recipient |
  | 10 | protocol_fee_recipient_token_account |
  | 11 | base_token_program |
  | 12 | quote_token_program |
  | 13 | system_program |
  | 14 | associated_token_program |
  | 15 | event_authority |
  | 16 | program |
  | 17 | coin_creator_vault_ata |
  | 18 | coin_creator_vault_authority |
  | 19/20 | global/user volume accumulators (buy only) |

  Trader = idx 1. Token mint = `base_mint` (idx 3). SOL side = `quote_mint` (idx 4) — **confirm
  `quote_mint == WSOL` per-pool; non-SOL quote pools exist (`UNVERIFIED` otherwise).**
- **Direction:** buy = quote(SOL)→base(token); sell = base(token)→quote(SOL).
- **Exact amounts:** `BuyEvent` disc `[103,244,82,31,44,245,119,119]`
  (`base_amount_out`, `quote_amount_in`, `user_quote_amount_in`=incl fees);
  `SellEvent` disc `[62,47,55,10,165,3,220,42]`
  (`base_amount_in`, `quote_amount_out`, `user_quote_amount_out`). Fallback = balance deltas.

---

## 4. Raydium Launchpad (LaunchLab)

Source: official IDL `github.com/raydium-io/raydium-idl/raydium_launchpad/raydium_launchpad.json`
(`raydium_launchpad` v0.2.0). This is the token-launch/bonding-curve program — NOT AMM v4 / CLMM / CPMM.

- **Program ID:** `LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj`
- **Distinct (avoid confusing) — `UNVERIFIED` here, well-known constants, cross-check Raydium docs:**
  - AMM v4: `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`
  - CLMM: `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK`
  - CPMM: `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`
- **Discriminators** (IDL == `sha256("global:<name>")[..8]`):
  - `buy_exact_in`  = `[250,234,13,123,213,156,19,236]` — `amount_in:u64, minimum_amount_out:u64, share_fee_rate:u64`
  - `buy_exact_out` = `[24,211,116,40,105,3,153,56]`   — `amount_out:u64, maximum_amount_in:u64, share_fee_rate:u64`
  - `sell_exact_in` = `[149,39,222,155,211,124,152,26]` — `amount_in:u64, minimum_amount_out:u64, share_fee_rate:u64`
  - `sell_exact_out`= `[95,200,71,34,8,9,11,166]`       — `amount_out:u64, maximum_amount_in:u64, share_fee_rate:u64`
- **Accounts (identical order for all 4 variants):**

  | idx | account |
  |----|---------|
  | 0 | **payer (trader)** |
  | 1 | authority (PDA) |
  | 2 | global_config |
  | 3 | platform_config |
  | 4 | pool_state |
  | 5 | user_base_token |
  | 6 | user_quote_token |
  | 7 | base_vault |
  | 8 | quote_vault |
  | 9 | **base_token_mint** (launched token) |
  | 10 | **quote_token_mint** (WSOL side) |
  | 11 | base_token_program |
  | 12 | quote_token_program |
  | 13 | event_authority |
  | 14 | program |

  Trader = idx 0. Token mint = base (idx 9). SOL side = quote (idx 10) — confirm `== WSOL`;
  some pools may use USDC (`UNVERIFIED`).
- **Direction:** `buy_*` = quote(SOL)→base(token); `sell_*` = base(token)→quote(SOL).
- **Amounts:** parse `TradeEvent` CPI log or use balance deltas (§0); args are intent/limits only.

---

## 5. Meteora DLMM (roadmap "DLMM v2" = current on-chain DLMM)

Source: official IDL `github.com/MeteoraAg/dlmm-sdk/idls/dlmm.json` (`lb_clmm` v0.12.0).

- **Program ID (DLMM):** `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`
- **DAMM v2 (separate constant-product AMM — different program, DON'T confuse):**
  `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` (IDL `cp_amm` v0.2.0,
  `github.com/MeteoraAg/damm-v2-sdk`). There is no third "DLMM v2" program ID.
- **SWAP discriminators (decode these):**
  - `swap` = `[248,198,158,145,225,117,135,200]` — `amount_in:u64, min_amount_out:u64`
  - `swap2` = `[65,75,63,76,235,91,91,136]` — adds `remaining_accounts_info`, inserts `memo_program`
    before `event_authority`
  - `swap_exact_out` `[250,73,101,33,38,207,75,184]`, `swap_exact_out2` `[43,215,247,132,137,60,243,81]`
  - `swap_with_price_impact` `[56,173,230,208,173,228,156,205]`,
    `swap_with_price_impact2` `[74,98,192,214,177,51,75,51]`
  - **NOTE:** DAMM v2 `swap` shares bytes `[248,198,158,145,225,117,135,200]` — **disambiguate by program ID.**
- **EXCLUDE (liquidity ops, not trades):** `add_liquidity` `[181,157,89,67,143,182,52,72]`,
  `add_liquidity2` `[228,162,78,28,70,219,116,115]`, `add_liquidity_by_strategy` `[7,3,150,127,148,40,61,200]`,
  `remove_liquidity` `[80,85,209,72,24,206,177,108]`, `remove_all_liquidity` `[10,51,61,35,112,105,24,85]`,
  `remove_liquidity_by_range` `[26,82,102,152,240,74,105,26]`, `rebalance_liquidity` `[92,4,176,193,119,185,83,9]`
  (+ `*2` variants).
- **DLMM `swap` accounts:**

  | idx | account |
  |----|---------|
  | 0 | lb_pair (pool) |
  | 1 | bin_array_bitmap_extension (optional) |
  | 2 | reserve_x |
  | 3 | reserve_y |
  | 4 | user_token_in |
  | 5 | user_token_out |
  | 6 | **token_x_mint** |
  | 7 | **token_y_mint** |
  | 8 | oracle |
  | 9 | host_fee_in (optional) |
  | 10 | **user (trader, signer)** |
  | 11 | token_x_program |
  | 12 | token_y_program |
  | 13 | event_authority |
  | 14 | program |

  `swap2` inserts `memo_program` → shifts event_authority/program to 14/15. Optional accounts shift
  indices — resolve by relation, not fixed offset.
- **Direction:** name has no buy/sell. Use WSOL pre/post token balance on the trader:
  WSOL decreases (spent on input) ⇒ **Buy**; WSOL increases (received on output) ⇒ **Sell**.
  The non-WSOL mint (of token_x_mint / token_y_mint) is the traded token. DAMM v2 swap args are an
  opaque `SwapParameters` struct, so balance-delta direction is required there.

---

## 6. Crate versions (latest stable on crates.io, 2026-06-02)

We keep dependencies minimal and **string-based** for addresses (Phase 1 only decodes — no tx
building, no ATA derivation), so the heavy `solana-sdk` / `solana-transaction-status` crates are
**intentionally omitted** (documented deviation from roadmap's dependency list, which targeted an
older SDK and execution work). We decode the jsonParsed notification with our own serde structs +
`bs58` for instruction data.

| crate | version | crate | version |
|---|---|---|---|
| tokio | 1.52 (`features=["full"]`) | serde | 1 (`features=["derive"]`) |
| tokio-tungstenite | 0.29 (`features=["native-tls"]`) | serde_json | 1 |
| futures-util | 0.3 | bs58 | 0.5 |
| tracing | 0.1 | sha2 | 0.11 (derive/verify discriminators in tests) |
| tracing-subscriber | 0.3 (`features=["env-filter"]`) | chrono | 0.4 (`features=["serde"]`) |
| dotenvy | 0.15 | anyhow | 1 |
| thiserror | 2 | | |

If solana types are later needed, `solana-sdk` is 4.0.1 / `solana-transaction-status` 4.0.0 (verified).
