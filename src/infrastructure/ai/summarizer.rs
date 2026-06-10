use crate::infrastructure::ai::claude::ClaudeSummarizer;
use crate::error::AppResult;
use crate::domain::NoteSections;

/// LLM-based note summarization interface.
///
/// Implementations:
/// - Claude API (Sonnet for quality, Haiku for cost)
/// - Mock (for testing without API calls)
pub trait Summarizer: Send + Sync {
    /// Extract structured note sections from an encounter transcript.
    /// Returns incremental updates (delta) to note sections.
    fn summarize_encounter(
        &self,
        transcript: &str,
        prior_sections: &NoteSections,
        encounter_number: u32,
    ) -> impl std::future::Future<Output = AppResult<NoteSections>> + Send;

    /// Generate the MDM (Medical Decision Making) section from all accumulated data.
    /// Called explicitly by the clinician — never auto-triggered.
    fn generate_mdm(
        &self,
        sections: &NoteSections,
        results_summary: &str,
        disposition: &str,
    ) -> impl std::future::Future<Output = AppResult<String>> + Send;
}

pub enum AppSummarizer {
    Claude(ClaudeSummarizer),
    Mock(MockSummarizer),
}

impl Summarizer for AppSummarizer {
    async fn summarize_encounter(&self, t: &str, p: &NoteSections, n: u32) -> AppResult<NoteSections> {
        match self {
            AppSummarizer::Claude(c) => c.summarize_encounter(t, p, n).await,
            AppSummarizer::Mock(m) => m.summarize_encounter(t, p, n).await,
        }
    }

    async fn generate_mdm(&self, s: &NoteSections, r: &str, d: &str) -> AppResult<String> {
        match self {
            AppSummarizer::Claude(c) => c.generate_mdm(s, r, d).await,
            AppSummarizer::Mock(m) => m.generate_mdm(s, r, d).await,
        }
    }
}

pub struct MockSummarizer;

impl MockSummarizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockSummarizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Summarizer for MockSummarizer {
    async fn summarize_encounter(
        &self,
        _transcript: &str,
        _prior_sections: &NoteSections,
        _encounter_number: u32,
    ) -> AppResult<NoteSections> {
        Ok(NoteSections {
            hpi: Some("[mock HPI from encounter transcript]".into()),
            ros: None,
            exam: Some("[mock exam findings]".into()),
            mdm: None,
            disposition_text: None,
        })
    }

    async fn generate_mdm(
        &self,
        _sections: &NoteSections,
        _results_summary: &str,
        _disposition: &str,
    ) -> AppResult<String> {
        Ok("[mock MDM: moderate complexity, 2 diagnoses considered]".into())
    }
}
