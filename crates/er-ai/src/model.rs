//! Claude model routing and immutable-at-call-time pricing.

use crate::transport::{ClaudeTransport, Usage};

/// One logical Claude model with the identifier expected by each transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelId {
    pub direct: String,
    pub bedrock: String,
}

impl ModelId {
    pub fn sonnet_4_6() -> Self {
        Self {
            direct: "claude-sonnet-4-6".to_string(),
            bedrock: "anthropic.claude-sonnet-4-6".to_string(),
        }
    }

    pub fn haiku_4_5() -> Self {
        Self {
            direct: "claude-haiku-4-5".to_string(),
            bedrock: "anthropic.claude-haiku-4-5".to_string(),
        }
    }

    pub fn for_transport<'a>(&'a self, transport: &ClaudeTransport) -> &'a str {
        match transport {
            ClaudeTransport::DirectApi { .. } => &self.direct,
            #[cfg(feature = "bedrock")]
            ClaudeTransport::Bedrock { .. } => &self.bedrock,
        }
    }
}

/// Per-call routing. The pilot defaults to Sonnet for both operations; switching
/// summaries to Haiku is a configuration change after the planned quality A/B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarizerConfig {
    pub summary_model: ModelId,
    pub mdm_model: ModelId,
}

impl Default for SummarizerConfig {
    fn default() -> Self {
        Self {
            summary_model: ModelId::sonnet_4_6(),
            mdm_model: ModelId::sonnet_4_6(),
        }
    }
}

/// Convert a deploy-time logical name into a model pair.
pub fn configured_model(name: &str) -> Option<ModelId> {
    match name.trim().to_ascii_lowercase().as_str() {
        "sonnet" | "sonnet-4.6" | "claude-sonnet-4-6" => Some(ModelId::sonnet_4_6()),
        "haiku" | "haiku-4.5" | "claude-haiku-4-5" => Some(ModelId::haiku_4_5()),
        _ => None,
    }
}

/// Prices in hundredths of a micro-dollar per token.
///
/// Re-verify quarterly. Last checked 2026-09-10 against the Phase 4 cost model:
/// Sonnet $3/$15 and Haiku $1/$5 per million input/output tokens. Cache reads
/// cost 0.1x input and cache writes 1.25x input. Integer hundredths preserve
/// fractional cache rates without floating-point money arithmetic.
#[derive(Clone, Copy)]
struct Price {
    input: u128,
    output: u128,
    cache_read: u128,
    cache_write: u128,
}

fn price_for(model: &str) -> Option<Price> {
    if model.contains("sonnet-4-6") {
        Some(Price {
            input: 300,
            output: 1_500,
            cache_read: 30,
            cache_write: 375,
        })
    } else if model.contains("haiku-4-5") {
        Some(Price {
            input: 100,
            output: 500,
            cache_read: 10,
            cache_write: 125,
        })
    } else {
        None
    }
}

/// USD × 1,000,000, frozen using the price table at call time.
///
/// Unknown models and mocks intentionally meter at zero until their price is
/// reviewed and added. Saturation keeps malformed provider counters from
/// wrapping a signed database column.
pub fn cost_micros(model: &str, usage: &Usage) -> i64 {
    let Some(price) = price_for(model) else {
        return 0;
    };

    let hundredths = u128::from(usage.input_tokens)
        .saturating_mul(price.input)
        .saturating_add(u128::from(usage.output_tokens).saturating_mul(price.output))
        .saturating_add(u128::from(usage.cache_read_input_tokens).saturating_mul(price.cache_read))
        .saturating_add(
            u128::from(usage.cache_creation_input_tokens).saturating_mul(price.cache_write),
        );
    let rounded_micros = hundredths.saturating_add(50) / 100;
    rounded_micros.min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_micros_matches_the_price_table() {
        let usage = Usage {
            input_tokens: 2_000,
            output_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        // 2,000 × $3/MTok + 500 × $15/MTok = $0.0135.
        assert_eq!(cost_micros("claude-sonnet-4-6", &usage), 13_500);

        let cached = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 10,
        };
        assert_eq!(cost_micros("anthropic.claude-sonnet-4-6", &cached), 41);
        assert_eq!(cost_micros("mock", &usage), 0);
    }

    #[test]
    fn logical_model_names_resolve_for_both_transports() {
        let sonnet = configured_model("SONNET").unwrap();
        assert_eq!(sonnet.direct, "claude-sonnet-4-6");
        assert_eq!(sonnet.bedrock, "anthropic.claude-sonnet-4-6");
        assert!(configured_model("future-model").is_none());
    }
}
