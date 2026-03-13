use crate::error::AppResult;
use crate::models::NoteSections;

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

/// Mock summarizer for testing — returns template text.
pub struct MockSummarizer;

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
