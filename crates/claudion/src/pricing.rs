// SPDX-License-Identifier: Apache-2.0

//! Token pricing models.
//!
//! Encapsulates the cost structure of different Claude models so that
//! session metrics can be converted into dollar estimates. The four-rate
//! breakdown (input, output, cache creation, cache read) reflects the
//! actual billing structure where cache reads are significantly cheaper.

use crate::energy::{TokenCost, TokenCount};
use serde::{Deserialize, Serialize};

use crate::types::Turn;

/// Per-model pricing rates in USD per million tokens.
///
/// The cache distinction matters: cache reads are typically 90% cheaper
/// than fresh input, making cache-heavy sessions dramatically cheaper.
/// This struct captures that asymmetry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PricingModel {
    /// Model identifier this pricing applies to.
    pub model: String,
    /// Cost per million fresh input tokens (USD).
    pub input_per_mtok: f64,
    /// Cost per million output tokens (USD).
    pub output_per_mtok: f64,
    /// Cost per million cache-creation input tokens (USD).
    pub cache_creation_per_mtok: f64,
    /// Cost per million cache-read input tokens (USD).
    pub cache_read_per_mtok: f64,
}

impl PricingModel {
    /// Default Opus pricing (as of 2025).
    ///
    /// - Input: $15/MTok
    /// - Output: $75/MTok
    /// - Cache creation: $18.75/MTok (25% more than input)
    /// - Cache read: $1.50/MTok (90% discount on input)
    #[must_use]
    pub fn opus() -> Self {
        Self {
            model: "claude-opus-4-6".to_string(),
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_creation_per_mtok: 18.75,
            cache_read_per_mtok: 1.50,
        }
    }

    /// Default Sonnet pricing.
    ///
    /// - Input: $3/MTok
    /// - Output: $15/MTok
    /// - Cache creation: $3.75/MTok
    /// - Cache read: $0.30/MTok
    #[must_use]
    pub fn sonnet() -> Self {
        Self {
            model: "claude-sonnet-4-6".to_string(),
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_creation_per_mtok: 3.75,
            cache_read_per_mtok: 0.30,
        }
    }

    /// Compute the cost of a single turn.
    #[must_use]
    pub fn cost_of_turn(&self, turn: &Turn) -> TokenCost {
        let input_cost = token_cost(turn.input_tokens, self.input_per_mtok);
        let output_cost = token_cost(turn.output_tokens, self.output_per_mtok);
        let cache_create_cost = token_cost(
            turn.cache_creation_input_tokens,
            self.cache_creation_per_mtok,
        );
        let cache_read_cost = token_cost(turn.cache_read_input_tokens, self.cache_read_per_mtok);

        TokenCost::new(input_cost + output_cost + cache_create_cost + cache_read_cost)
    }
}

/// Convert a token count to a dollar amount given a per-million-token rate.
fn token_cost(tokens: TokenCount, rate_per_mtok: f64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let count = tokens.get() as f64;
    count * rate_per_mtok / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_opus_pricing_exists() {
        let p = PricingModel::opus();
        assert_eq!(p.input_per_mtok, 15.0);
        assert_eq!(p.output_per_mtok, 75.0);
        assert_eq!(p.cache_read_per_mtok, 1.50);
    }

    #[test]
    fn test_cost_of_turn() {
        let pricing = PricingModel::opus();
        let turn = Turn {
            index: 0,
            timestamp: Utc::now(),
            model: Some("claude-opus-4-6".to_string()),
            input_tokens: TokenCount::new(1_000_000), // $15
            cache_creation_input_tokens: TokenCount::new(0),
            cache_read_input_tokens: TokenCount::new(0),
            output_tokens: TokenCount::new(0),
        };
        let cost = pricing.cost_of_turn(&turn);
        // 1M input tokens at $15/MTok = $15.00
        assert!((cost.get() - 15.0).abs() < 0.001);
    }

    #[test]
    fn test_cache_read_is_cheap() {
        let pricing = PricingModel::opus();
        let turn = Turn {
            index: 0,
            timestamp: Utc::now(),
            model: None,
            input_tokens: TokenCount::new(0),
            cache_creation_input_tokens: TokenCount::new(0),
            cache_read_input_tokens: TokenCount::new(1_000_000), // $1.50
            output_tokens: TokenCount::new(0),
        };
        let cost = pricing.cost_of_turn(&turn);
        assert!((cost.get() - 1.50).abs() < 0.001);
    }
}
