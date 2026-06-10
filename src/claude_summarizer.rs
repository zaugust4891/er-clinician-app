use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::domain::NoteSections;
use crate::summarizer::Summarizer;

// Request and response structs for Claude API.
#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

/// ClaudeSummarizer is the API client for note summarization.
/// 
///   Why these fields:
/// - client: Client — reuse a single reqwest client across calls (connection
/// pooling)
/// - api_key: String — your ANTHROPIC_API_KEY. Read from env at construction
/// time, not at call time.
/// - model: String — lets you swap between claude-sonnet-4-20250514 (quality)
/// and claude-haiku-4-5-20251001 (speed/cost) without changing code.
pub struct ClaudeSummarizer {
    client: Client,
    api_key: String,
    model: String,
}

impl ClaudeSummarizer {
    pub fn new() -> AppResult<Self> {
        let api_key = std::env::var("ANTHROPIC_KEY")
            .map_err(|_| AppError::Summarization(
                "ANTHROPIC_KEY not set in env file.".into(),
            ))?;
    
        Ok(Self {
            client: Client::new(),
            api_key,
            model: "claude-sonnet-4-6".into(),
        })
    }
}

impl ClaudeSummarizer {
    async fn call_claude(&self, system: &str, user_message: &str) -> AppResult<String> {
        let request = MessagesRequest {
            model: self.model.clone(),
            max_tokens: 2048,
            system: system.into(),
            messages: vec![Message {
                role: "user".into(),
                content: user_message.into(),
            }]
        };

        let response = self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(AppError::Api)?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::Summarization(
                format!("Claude API {status}: {body}")
            ));
        }

        let body: MessagesResponse = response.json().await.map_err(AppError::Api)?;
        let text = body.content
            .first()
            .and_then(|block| block.text.clone())
            .ok_or_else(|| AppError::Summarization("empty response from Claude".into()))?;

        Ok(strip_json_fences(&text).to_string())
    }
}

/// Remove markdown code fences Claude sometimes adds despite being told not to.
/// Handles ```json, ```, and plain whitespace. Safe no-op if fences aren't present.
fn strip_json_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```json").unwrap_or(s);
    let s = s.strip_prefix("```").unwrap_or(s);
    let s = s.strip_suffix("```").unwrap_or(s);
    s.trim()
}

impl Summarizer for ClaudeSummarizer {
    async fn summarize_encounter(
        &self,
        transcript: &str,
        prior_sections: &NoteSections,
        encounter_number: u32,
    ) -> AppResult<NoteSections> {
        let system = r#"You are an emergency medicine documentation assistant.
You extract structured clinical note sections from clinician-patient encounter transcripts.

Output ONLY raw JSON — no markdown, no code fences, no commentary. Just the JSON object.

Schema:
{
  "hpi": "string or null",
  "ros": "string or null",
  "exam": "string or null",
  "mdm": null,
  "disposition_text": null
}

Rules:
- Write in standard ED note prose (third person, present tense, medical abbreviations OK)
- HPI: Chief complaint, onset, duration, quality, severity, associated symptoms, pertinent negatives
- ROS: Only if explicitly reviewed systems beyond HPI
- Exam: Physical findings in organ-system format (HEENT, CV, Pulm, Abd, MSK, Neuro, Skin)
- Leave mdm and disposition_text as null (generated separately)
- If this is encounter 2+, MERGE new findings into prior sections — don't replace, augment
- Omit sections with no data (return null for that field)"#;

        let prior_json = serde_json::to_string(prior_sections)
            .unwrap_or_else(|_| "{}".into());

        let user_message = format!(
            "Encounter #{encounter_number} transcript: transcript:\n\
            ---\n\
            {transcript}\n\
            ---\n\
            \n\
            Prior note sections (merge into these):\n\
            {prior_json}"
        );

        let response_text = self.call_claude(&system, &user_message).await?;

        // Parse the JSON response into NoteSections
        serde_json::from_str::<NoteSections>(&response_text)
            .map_err(|e| AppError::Summarization(format!("Failed to parse Claude response as NoteSections: {e}\nRaw: {response_text}")))
    }

    async fn generate_mdm(
        &self,
        sections: &NoteSections,
        results_summary: &str,
        disposition: &str,
    ) -> AppResult<String> {
        let system = r#"You are an emergency medicine documentation assistant.
        Generate the Medical Decision Making (MDM) section of an ED note.

        MDM has three elements (per 2021 E/M guidelines):
        1. Number and complexity of problems addressed
        2. Amount and complexity of data reviewed (labs, imaging, records)
        3. Risk of complications, morbidity, or mortality

        Write in standard ED MDM prose. Be specific about:
        - Differential diagnoses considered and why
        - Data reviewed and how it influenced decisions
        - Risk assessment tied to the clinical situation
        - Treatment plan with rationale

        Output ONLY the MDM text - no JSON wrapper, no section headers, just the prose."#;

        let sections_json = serde_json::to_string(sections)
            .unwrap_or_else(|_| "{}".into());

        let user_message = format!(
            "Note sections so far:\n{sections_json}\n\n\
            Results reviewed:\n{results_summary}\n\n\
            Disposition: {disposition}\n\n\
            Generate the MDM section."
        );

        self.call_claude(&system, &user_message).await
    }
}

/*
Why the 2021 E/M guidelines reference? 
MDM complexity level (straightforward/ low / moderate / high) directly determines billing code (99281-99285).
Claude knowing this framework produces MDM that maps to billing levels
correctly. This is a major pain point for ED docs — MDM that doesn't support
the billing level they select.

*/