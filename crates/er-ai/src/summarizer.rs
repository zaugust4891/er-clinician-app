use er_domain::NoteSections;
use std::sync::{Arc, Mutex};

use crate::claude::ClaudeSummarizer;
use crate::error::AiResult;
use crate::transport::Usage;

/// A model output together with the provider usage needed for durable metering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated<T> {
    pub value: T,
    pub usage: Usage,
    pub model: String,
}

/// LLM-based note summarization interface.
pub trait Summarizer: Send + Sync {
    fn summarize_encounter(
        &self,
        transcript: &str,
        prior_sections: &NoteSections,
        encounter_number: u32,
    ) -> impl std::future::Future<Output = AiResult<Generated<NoteSections>>> + Send;

    fn generate_mdm(
        &self,
        sections: &NoteSections,
        results_summary: &str,
        disposition: &str,
    ) -> impl std::future::Future<Output = AiResult<Generated<String>>> + Send;
}

pub enum AppSummarizer {
    Claude(ClaudeSummarizer),
    Mock(MockSummarizer),
}

impl Summarizer for AppSummarizer {
    async fn summarize_encounter(
        &self,
        transcript: &str,
        prior: &NoteSections,
        encounter_number: u32,
    ) -> AiResult<Generated<NoteSections>> {
        match self {
            Self::Claude(client) => {
                client
                    .summarize_encounter(transcript, prior, encounter_number)
                    .await
            }
            Self::Mock(mock) => {
                mock.summarize_encounter(transcript, prior, encounter_number)
                    .await
            }
        }
    }

    async fn generate_mdm(
        &self,
        sections: &NoteSections,
        results: &str,
        disposition: &str,
    ) -> AiResult<Generated<String>> {
        match self {
            Self::Claude(client) => client.generate_mdm(sections, results, disposition).await,
            Self::Mock(mock) => mock.generate_mdm(sections, results, disposition).await,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct MockSummarizer {
    captured_transcripts: Arc<Mutex<Vec<String>>>,
}

impl MockSummarizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test observation seam. It exposes only what reached the mock, allowing
    /// integration tests to prove scrubbing happened before the model boundary.
    pub fn captured_transcripts(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.captured_transcripts)
    }

    fn generated<T>(value: T) -> Generated<T> {
        Generated {
            value,
            usage: Usage::default(),
            model: "mock".to_string(),
        }
    }
}

impl Summarizer for MockSummarizer {
    async fn summarize_encounter(
        &self,
        transcript: &str,
        _prior_sections: &NoteSections,
        _encounter_number: u32,
    ) -> AiResult<Generated<NoteSections>> {
        self.captured_transcripts
            .lock()
            .expect("mock transcript capture lock")
            .push(transcript.to_string());
        Ok(Self::generated(NoteSections {
            hpi: Some("[mock HPI from encounter transcript]".into()),
            ros: None,
            exam: Some("[mock exam findings]".into()),
            mdm: None,
            disposition_text: None,
        }))
    }

    async fn generate_mdm(
        &self,
        _sections: &NoteSections,
        _results_summary: &str,
        _disposition: &str,
    ) -> AiResult<Generated<String>> {
        Ok(Self::generated(
            "[mock MDM: moderate complexity, 2 diagnoses considered]".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_summarizer_mock_dispatches_summarize_encounter() {
        let summarizer = AppSummarizer::Mock(MockSummarizer::new());
        let generated = summarizer
            .summarize_encounter("transcript", &NoteSections::default(), 1)
            .await
            .unwrap();

        assert_eq!(
            generated.value.hpi.as_deref(),
            Some("[mock HPI from encounter transcript]")
        );
        assert_eq!(generated.usage, Usage::default());
        assert_eq!(generated.model, "mock");
    }

    #[tokio::test]
    async fn app_summarizer_mock_dispatches_generate_mdm() {
        let summarizer = AppSummarizer::Mock(MockSummarizer::new());
        let generated = summarizer
            .generate_mdm(&NoteSections::default(), "Troponin negative", "discharge")
            .await
            .unwrap();

        assert_eq!(
            generated.value,
            "[mock MDM: moderate complexity, 2 diagnoses considered]"
        );
        assert_eq!(generated.usage, Usage::default());
    }
}
