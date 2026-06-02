//! Guard 2: minimum SOL threshold.

use crate::models::types::{DecodedTradeEvent, TradeAction};

/// Guard 2: minimum SOL threshold, applied to BUYS only (sells always pass). Mutates the event:
/// sets `passed_threshold` and `guard_skip_reason`. (roadmap §Guards)
pub fn apply_threshold(event: &mut DecodedTradeEvent, min_sol_threshold: f64) {
    if event.action == TradeAction::Buy && event.sol_amount < min_sol_threshold {
        event.passed_threshold = false;
        event.guard_skip_reason = Some(format!(
            "buy {:.4} SOL below threshold {:.4} SOL",
            event.sol_amount, min_sol_threshold
        ));
    } else {
        event.passed_threshold = true;
        event.guard_skip_reason = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::DexProtocol;

    fn event(action: TradeAction, sol_amount: f64) -> DecodedTradeEvent {
        DecodedTradeEvent {
            signature: "sig".to_string(),
            slot: 1,
            timestamp: None,
            whale_address: "Whale".to_string(),
            dex: DexProtocol::PumpFun,
            action,
            mint: "MintA".to_string(),
            sol_amount,
            token_amount: 1_000,
            token_decimals: 6,
            is_bot_whale: false,
            passed_threshold: true,
            guard_skip_reason: None,
            decoded_at: None,
        }
    }

    #[test]
    fn buy_below_threshold_is_skipped() {
        let mut e = event(TradeAction::Buy, 1.0);
        apply_threshold(&mut e, 2.0);

        assert!(!e.passed_threshold);
        let reason = e.guard_skip_reason.expect("skip reason set");
        assert!(reason.contains("below threshold"));
    }

    #[test]
    fn buy_at_or_above_threshold_passes() {
        let mut e = event(TradeAction::Buy, 2.0);
        apply_threshold(&mut e, 2.0);

        assert!(e.passed_threshold);
        assert!(e.guard_skip_reason.is_none());
    }

    #[test]
    fn sell_below_threshold_always_passes() {
        let mut e = event(TradeAction::Sell, 0.1);
        apply_threshold(&mut e, 2.0);

        assert!(e.passed_threshold);
        assert!(e.guard_skip_reason.is_none());
    }
}
