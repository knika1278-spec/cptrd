//! Guard 1: arbitrage-bot detection.

use crate::models::types::{DecodedTradeEvent, TradeAction};

/// Maximum number of DEX swap instructions in one tx before it is treated as a bot.
const MAX_SWAPS_PER_TX: usize = 4;

/// Guard 1: detect arbitrage-bot transactions. Returns `Some(reason)` if the tx's decoded events
/// look like a bot, else `None`. Signals (roadmap §Guards):
/// (a) the same tx contains BOTH a Buy and a Sell for the SAME mint;
/// (b) more than 4 DEX swap events in one tx.
// TODO: refine same-mint detection if needed.
pub fn detect_bot(events: &[DecodedTradeEvent]) -> Option<String> {
    for event in events {
        if event.action != TradeAction::Buy {
            continue;
        }
        let has_opposing_sell = events
            .iter()
            .any(|other| other.action == TradeAction::Sell && other.mint == event.mint);
        if has_opposing_sell {
            return Some(format!(
                "buy and sell of same mint {} in one transaction",
                event.mint
            ));
        }
    }

    if events.len() > MAX_SWAPS_PER_TX {
        return Some(format!(
            "{} DEX swap instructions in one transaction (likely arbitrage bot)",
            events.len()
        ));
    }

    None
}

/// Apply Guard 1: if `detect_bot` returns `Some(reason)`, set `is_bot_whale = true` on ALL events
/// and return the reason.
pub fn apply_bot_filter(events: &mut [DecodedTradeEvent]) -> Option<String> {
    let reason = detect_bot(events)?;
    for event in events.iter_mut() {
        event.is_bot_whale = true;
    }
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::DexProtocol;

    fn event(action: TradeAction, mint: &str) -> DecodedTradeEvent {
        DecodedTradeEvent {
            signature: "sig".to_string(),
            slot: 1,
            timestamp: None,
            whale_address: "Whale".to_string(),
            dex: DexProtocol::PumpFun,
            action,
            mint: mint.to_string(),
            sol_amount: 3.0,
            token_amount: 1_000,
            token_decimals: 6,
            is_bot_whale: false,
            passed_threshold: true,
            guard_skip_reason: None,
            decoded_at: None,
        }
    }

    #[test]
    fn flags_buy_and_sell_of_same_mint() {
        let mut events = vec![
            event(TradeAction::Buy, "MintA"),
            event(TradeAction::Sell, "MintA"),
        ];

        let reason = detect_bot(&events);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("MintA"));

        let applied = apply_bot_filter(&mut events);
        assert!(applied.is_some());
        assert!(events.iter().all(|e| e.is_bot_whale));
    }

    #[test]
    fn single_buy_is_not_flagged() {
        let mut events = vec![event(TradeAction::Buy, "MintA")];

        assert!(detect_bot(&events).is_none());
        assert!(apply_bot_filter(&mut events).is_none());
        assert!(events.iter().all(|e| !e.is_bot_whale));
    }

    #[test]
    fn flags_more_than_four_swaps() {
        let events = vec![
            event(TradeAction::Buy, "MintA"),
            event(TradeAction::Buy, "MintB"),
            event(TradeAction::Buy, "MintC"),
            event(TradeAction::Buy, "MintD"),
            event(TradeAction::Buy, "MintE"),
        ];

        let reason = detect_bot(&events);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("5 DEX swap instructions"));
    }

    #[test]
    fn buy_and_sell_of_different_mints_is_not_flagged() {
        let events = vec![
            event(TradeAction::Buy, "MintA"),
            event(TradeAction::Sell, "MintB"),
        ];

        assert!(detect_bot(&events).is_none());
    }
}
