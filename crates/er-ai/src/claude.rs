use er_domain::NoteSections;

use crate::error::{AiError, AiResult};
use crate::model::{ModelId, SummarizerConfig};
use crate::summarizer::{Generated, Summarizer};
use crate::transport::{CacheControl, ClaudeTransport, Message, MessagesRequest, SystemBlock};

pub const SUMMARY_SYSTEM_PROMPT: &str = include_str!("../prompts/summarize_system.md");
pub const MDM_SYSTEM_PROMPT: &str = include_str!("../prompts/mdm_system.md");

/// Builds clinical requests while [`ClaudeTransport`] owns authentication and
/// provider-specific wire differences.
#[derive(Debug)]
pub struct ClaudeSummarizer {
    transport: ClaudeTransport,
    config: SummarizerConfig,
}

impl ClaudeSummarizer {
    /// Local-development constructor retained for the CLI and standalone tool.
    pub fn new() -> AiResult<Self> {
        let api_key = std::env::var("ANTHROPIC_KEY")
            .map_err(|_| AiError::Summarization("ANTHROPIC_KEY not set in env file.".into()))?;
        Ok(Self::from_transport(
            ClaudeTransport::direct(api_key)?,
            SummarizerConfig::default(),
        ))
    }

    pub fn from_transport(transport: ClaudeTransport, config: SummarizerConfig) -> Self {
        Self { transport, config }
    }

    async fn call_claude(
        &self,
        model: &ModelId,
        system: &'static str,
        user_message: String,
    ) -> AiResult<Generated<String>> {
        let resolved_model = model.for_transport(&self.transport).to_string();
        let request = messages_request(system, user_message);
        let response = self.transport.messages(&resolved_model, request).await?;
        let text = response
            .first_text()
            .ok_or_else(|| AiError::Summarization("empty response from Claude".into()))?;

        Ok(Generated {
            value: strip_json_fences(text).to_string(),
            usage: response.usage,
            model: resolved_model,
        })
    }
}

fn messages_request(system: &'static str, user_message: String) -> MessagesRequest {
    MessagesRequest {
        model: None,
        anthropic_version: None,
        max_tokens: 4_096,
        system: vec![SystemBlock {
            kind: "text",
            text: system.to_string(),
            cache_control: Some(CacheControl { kind: "ephemeral" }),
        }],
        messages: vec![Message {
            role: "user",
            content: user_message,
        }],
    }
}

/// Remove markdown code fences Claude sometimes adds despite being told not to.
fn strip_json_fences(value: &str) -> &str {
    let value = value.trim();
    let value = value.strip_prefix("```json").unwrap_or(value);
    let value = value.strip_prefix("```").unwrap_or(value);
    let value = value.strip_suffix("```").unwrap_or(value);
    value.trim()
}

impl Summarizer for ClaudeSummarizer {
    async fn summarize_encounter(
        &self,
        transcript: &str,
        prior_sections: &NoteSections,
        encounter_number: u32,
    ) -> AiResult<Generated<NoteSections>> {
        let prior_json = serde_json::to_string(prior_sections).map_err(|_| {
            AiError::Summarization("could not serialize prior note sections".to_string())
        })?;
        let user_message = format!(
            "Encounter #{encounter_number} transcript:\n---\n{transcript}\n---\n\n\
             Prior note sections (merge into these):\n{prior_json}"
        );

        let generated = self
            .call_claude(
                &self.config.summary_model,
                SUMMARY_SYSTEM_PROMPT,
                user_message,
            )
            .await?;
        let sections = serde_json::from_str::<NoteSections>(&generated.value).map_err(|_| {
            // Never include the raw response: model output may contain clinical
            // content and error chains are logged by the worker.
            AiError::Summarization("Claude response was not valid structured note JSON".to_string())
        })?;

        Ok(Generated {
            value: sections,
            usage: generated.usage,
            model: generated.model,
        })
    }

    async fn generate_mdm(
        &self,
        sections: &NoteSections,
        results_summary: &str,
        disposition: &str,
    ) -> AiResult<Generated<String>> {
        let sections_json = serde_json::to_string(sections)
            .map_err(|_| AiError::Summarization("could not serialize note sections".to_string()))?;
        let user_message = format!(
            "Note sections so far:\n{sections_json}\n\n\
             Results and objective data reviewed:\n{results_summary}\n\n\
             Disposition:\n{disposition}\n\nGenerate the MDM section."
        );

        self.call_claude(&self.config.mdm_model, MDM_SYSTEM_PROMPT, user_message)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_is_serialized_as_a_block_array() {
        let request = messages_request(MDM_SYSTEM_PROMPT, "synthetic input".to_string());
        let json = serde_json::to_value(request).unwrap();
        let blocks = json["system"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        assert!(json.get("model").is_none());
        assert!(json.get("anthropic_version").is_none());
    }

    #[test]
    fn mdm_prompt_clears_the_cache_minimum_conservatively() {
        // Whitespace-delimited words are a conservative lower bound for model
        // tokens in this English/Markdown prompt. Exact provider tokenization and
        // a live cache hit remain the manual gate recorded in the Step 7 guide.
        assert!(MDM_SYSTEM_PROMPT.split_whitespace().count() >= 2_300);
    }

    #[test]
    fn strips_optional_json_fences() {
        assert_eq!(
            strip_json_fences("```json\n{\"ok\":true}\n```"),
            "{\"ok\":true}"
        );
        assert_eq!(strip_json_fences(" plain "), "plain");
    }
}
