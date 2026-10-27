//! Estimated cost of token usage at Anthropic's published list prices.
//!
//! Prices are USD per million tokens, copied from the pricing page of the
//! Claude Platform documentation (see `PRICES_SOURCE`). They are an estimate:
//! the Batch discount, fast mode, the US-only inference multiplier and the tool
//! definitions counted as input are not applied. Claude Code's own recorded cost
//! is shown separately when it exists.

use crate::usage::Tokens;

pub const PRICES_SOURCE: &str = "https://platform.claude.com/docs/en/about-claude/pricing";
pub const PRICES_CHECKED: &str = "2026-10-27";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

const fn price(input: f64, output: f64, cache_write_5m: f64, cache_write_1h: f64, cache_read: f64) -> Price {
    Price { input, output, cache_write_5m, cache_write_1h, cache_read }
}

/// Canonical model ids (no date suffix) with their standard prices.
const TABLE: &[(&str, Price)] = &[
    ("claude-opus-5-5", price(4.0, 20.0, 5.0, 8.0, 0.20)),
    ("claude-opus-5", price(5.0, 25.0, 6.25, 10.0, 0.50)),
    ("claude-opus-4-8", price(5.0, 25.0, 6.25, 10.0, 0.50)),
    ("claude-opus-4-7", price(5.0, 25.0, 6.25, 10.0, 0.50)),
    ("claude-opus-4-6", price(5.0, 25.0, 6.25, 10.0, 0.50)),
    ("claude-opus-4-5", price(5.0, 25.0, 6.25, 10.0, 0.50)),
    ("claude-opus-4-1", price(15.0, 75.0, 18.75, 30.0, 1.50)),
    ("claude-opus-4", price(15.0, 75.0, 18.75, 30.0, 1.50)),
    ("claude-sonnet-5-5", price(2.0, 10.0, 2.50, 4.0, 0.10)),
    ("claude-sonnet-5", price(2.0, 10.0, 2.50, 4.0, 0.20)),
    ("claude-sonnet-4-6", price(3.0, 15.0, 3.75, 6.0, 0.30)),
    ("claude-sonnet-4-5", price(3.0, 15.0, 3.75, 6.0, 0.30)),
    ("claude-sonnet-4", price(3.0, 15.0, 3.75, 6.0, 0.30)),
    ("claude-haiku-5-5", price(0.10, 0.50, 0.125, 0.20, 0.01)),
    ("claude-haiku-4-5", price(1.0, 5.0, 1.25, 2.0, 0.10)),
    ("claude-haiku-3-5", price(0.80, 4.0, 1.0, 1.60, 0.08)),
    ("claude-fable-5-1", price(10.0, 50.0, 12.5, 20.0, 0.25)),
    ("claude-fable-5", price(10.0, 50.0, 12.5, 20.0, 1.0)),
];

/// Haiku 5.5 charges more for each request whose prompt is over 100,000 tokens.
const HAIKU_55_LONG_PROMPT_TOKENS: u64 = 100_000;
const HAIKU_55_LONG_PROMPT: Price = price(0.50, 2.50, 0.625, 1.0, 0.05);

/// Model id without the "-YYYYMMDD" release suffix and without a "[...]" context marker.
pub fn canonical_model(model: &str) -> &str {
    let base = model.split('[').next().unwrap_or(model);
    match base.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => base,
    }
}

/// Standard price of a model for a request with `prompt_tokens` of input.
pub fn price_for(model: &str, prompt_tokens: u64) -> Option<Price> {
    let id = canonical_model(model);
    if id == "claude-haiku-5-5" && prompt_tokens > HAIKU_55_LONG_PROMPT_TOKENS {
        return Some(HAIKU_55_LONG_PROMPT);
    }
    TABLE.iter().find(|(name, _)| *name == id).map(|(_, p)| *p)
}

/// Estimated cost in USD of one response. None if the model has no known price.
pub fn response_cost(model: &str, tokens: &Tokens) -> Option<f64> {
    // The prompt is every input token, including cache reads and writes.
    let prompt = tokens.input + tokens.cache_read + tokens.cache_write;
    let price = price_for(model, prompt)?;
    let write_1h = tokens.cache_write_1h.min(tokens.cache_write);
    let write_5m = tokens.cache_write - write_1h;
    let usd = tokens.input as f64 * price.input
        + tokens.output as f64 * price.output
        + write_5m as f64 * price.cache_write_5m
        + write_1h as f64 * price.cache_write_1h
        + tokens.cache_read as f64 * price.cache_read;
    Some(usd / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_suffix_and_context_marker_are_ignored() {
        assert_eq!(canonical_model("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(canonical_model("claude-opus-4-6[1m]"), "claude-opus-4-6");
        assert_eq!(canonical_model("claude-opus-5-5"), "claude-opus-5-5");
    }

    #[test]
    fn haiku_55_has_a_second_tier_for_long_prompts() {
        assert_eq!(price_for("claude-haiku-5-5", 100_000).unwrap().input, 0.10);
        assert_eq!(price_for("claude-haiku-5-5", 100_001).unwrap().input, 0.50);
    }

    #[test]
    fn unknown_model_has_no_price() {
        assert!(price_for("<synthetic>", 10).is_none());
    }

    #[test]
    fn response_cost_applies_each_category() {
        let tokens = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 0, cache_write: 1_000_000, cache_write_1h: 0 };
        // Opus 5: 5 + 25 + 6.25 (5-minute cache write).
        let usd = response_cost("claude-opus-5", &tokens).unwrap();
        assert!((usd - 36.25).abs() < 1e-9, "{usd}");
    }
}
