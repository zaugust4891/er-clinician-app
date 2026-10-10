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

    /// Exact, reviewed identifiers only.
    ///
    /// Sonnet 4.6 accepts its in-Region model ID and its US geographic
    /// cross-Region inference profile. The profile is the staging decision in
    /// `docs/ops/deployment-contract.md`: Sonnet 4.6 has no in-Region inference in
    /// us-east-2, and a request through `us.` may be processed in us-east-1,
    /// us-east-2 or us-west-2. The global profile, other geographies, and ARNs
    /// stay rejected until they have their own residency, account-access and
    /// pricing review. Every ID accepted here must also have a row in
    /// `price_for`, or its usage would be metered at zero.
    pub fn set_bedrock_id(&mut self, id: &str) -> bool {
        let approved = match self.direct.as_str() {
            "claude-sonnet-4-6" => matches!(
                id,
                "anthropic.claude-sonnet-4-6" | "us.anthropic.claude-sonnet-4-6"
            ),
            "claude-haiku-4-5" => id == "anthropic.claude-haiku-4-5",
            _ => false,
        };
        if approved {
            self.bedrock = id.to_string();
        }
        approved
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

/// Prices in thousandths of a micro-dollar per token.
///
/// Re-verify quarterly. Last checked 2026-10-09 against Anthropic's pricing page:
/// Sonnet 4.6 $3/$15 and Haiku 4.5 $1/$5 per million input/output tokens on the
/// first-party API, which is global by default. On Bedrock, Claude 4.5 and later
/// models cost 10% more through regional endpoints than through the global
/// profile, and every Bedrock ID `set_bedrock_id` accepts is regional (an
/// in-Region model ID or the `us.` geographic profile). Cache reads cost 0.1x
/// input and 5-minute cache writes 1.25x input. Thousandths keep the regional
/// cache-write rate ($4.125/MTok) an exact integer without floating-point money
/// arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Price {
    input: u128,
    output: u128,
    cache_read: u128,
    cache_write: u128,
}

const SONNET_4_6: Price = Price {
    input: 3_000,
    output: 15_000,
    cache_read: 300,
    cache_write: 3_750,
};

const SONNET_4_6_BEDROCK_REGIONAL: Price = Price {
    input: 3_300,
    output: 16_500,
    cache_read: 330,
    cache_write: 4_125,
};

const HAIKU_4_5: Price = Price {
    input: 1_000,
    output: 5_000,
    cache_read: 100,
    cache_write: 1_250,
};

const HAIKU_4_5_BEDROCK_REGIONAL: Price = Price {
    input: 1_100,
    output: 5_500,
    cache_read: 110,
    cache_write: 1_375,
};

fn price_for(model: &str) -> Option<Price> {
    match model {
        "claude-sonnet-4-6" => Some(SONNET_4_6),
        "anthropic.claude-sonnet-4-6" | "us.anthropic.claude-sonnet-4-6" => {
            Some(SONNET_4_6_BEDROCK_REGIONAL)
        }
        "claude-haiku-4-5" => Some(HAIKU_4_5),
        "anthropic.claude-haiku-4-5" => Some(HAIKU_4_5_BEDROCK_REGIONAL),
        _ => None,
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

    let thousandths = u128::from(usage.input_tokens)
        .saturating_mul(price.input)
        .saturating_add(u128::from(usage.output_tokens).saturating_mul(price.output))
        .saturating_add(u128::from(usage.cache_read_input_tokens).saturating_mul(price.cache_read))
        .saturating_add(
            u128::from(usage.cache_creation_input_tokens).saturating_mul(price.cache_write),
        );
    let rounded_micros = thousandths.saturating_add(500) / 1_000;
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
        // Bedrock regional endpoints: 2,000 × $3.30/MTok + 500 × $16.50/MTok = $0.01485.
        assert_eq!(cost_micros("anthropic.claude-sonnet-4-6", &usage), 14_850);
        assert_eq!(
            cost_micros("us.anthropic.claude-sonnet-4-6", &usage),
            14_850
        );

        let cached = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 10,
        };
        // 10 × $0.30/MTok + 10 × $3.75/MTok = $0.0000405, rounded half up.
        assert_eq!(cost_micros("claude-sonnet-4-6", &cached), 41);
        // 10 × $0.33/MTok + 10 × $4.125/MTok = $0.00004455, rounded half up.
        assert_eq!(cost_micros("us.anthropic.claude-sonnet-4-6", &cached), 45);
        assert_eq!(cost_micros("mock", &usage), 0);
    }

    #[test]
    fn bedrock_regional_prices_are_ten_percent_over_first_party() {
        for (direct, bedrock) in [
            ("claude-sonnet-4-6", "anthropic.claude-sonnet-4-6"),
            ("claude-sonnet-4-6", "us.anthropic.claude-sonnet-4-6"),
            ("claude-haiku-4-5", "anthropic.claude-haiku-4-5"),
        ] {
            let base = price_for(direct).unwrap();
            let regional = price_for(bedrock).unwrap();
            assert_eq!(regional.input * 10, base.input * 11, "{bedrock} input");
            assert_eq!(regional.output * 10, base.output * 11, "{bedrock} output");
            assert_eq!(
                regional.cache_read * 10,
                base.cache_read * 11,
                "{bedrock} cache read"
            );
            assert_eq!(
                regional.cache_write * 10,
                base.cache_write * 11,
                "{bedrock} cache write"
            );
        }
    }

    #[test]
    fn logical_model_names_resolve_for_both_transports() {
        let sonnet = configured_model("SONNET").unwrap();
        assert_eq!(sonnet.direct, "claude-sonnet-4-6");
        assert_eq!(sonnet.bedrock, "anthropic.claude-sonnet-4-6");
        assert!(configured_model("future-model").is_none());
    }

    #[test]
    fn overrides_cannot_change_family_or_smuggle_an_unpriced_profile() {
        let mut model = ModelId::sonnet_4_6();
        for id in [
            "anthropic.claude-haiku-4-5",
            "unreviewed-sonnet-4-6",
            "global.anthropic.claude-sonnet-4-6",
            "eu.anthropic.claude-sonnet-4-6",
            "us.anthropic.claude-haiku-4-5",
            "US.anthropic.claude-sonnet-4-6",
            " us.anthropic.claude-sonnet-4-6",
            "arn:aws:bedrock:profile",
        ] {
            assert!(!model.set_bedrock_id(id), "accepted {id}");
            assert_eq!(model, ModelId::sonnet_4_6());
            if id != "anthropic.claude-haiku-4-5" {
                assert!(price_for(id).is_none(), "{id} must not be priced");
            }
        }

        let mut haiku = ModelId::haiku_4_5();
        assert!(!haiku.set_bedrock_id("us.anthropic.claude-sonnet-4-6"));
        assert!(!haiku.set_bedrock_id("us.anthropic.claude-haiku-4-5"));
        assert_eq!(haiku, ModelId::haiku_4_5());
    }

    #[test]
    fn the_us_profile_is_accepted_for_sonnet_and_every_accepted_id_is_priced() {
        for id in [
            "anthropic.claude-sonnet-4-6",
            "us.anthropic.claude-sonnet-4-6",
        ] {
            let mut model = ModelId::sonnet_4_6();
            assert!(model.set_bedrock_id(id), "rejected {id}");
            assert_eq!(model.bedrock, id);
            assert_eq!(model.direct, "claude-sonnet-4-6", "family must not change");
            assert!(
                price_for(&model.bedrock).is_some(),
                "{id} would meter at zero"
            );
        }
        let mut haiku = ModelId::haiku_4_5();
        assert!(haiku.set_bedrock_id("anthropic.claude-haiku-4-5"));
        assert!(price_for(&haiku.bedrock).is_some());
    }
}
