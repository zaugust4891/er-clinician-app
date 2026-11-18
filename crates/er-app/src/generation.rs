//! Provider-neutral policy. The remote implementation has no AI dependency.
use crate::{AppError, AppResult};
use er_domain::{NoteSections, PatientRecord};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GenerationOwnership {
    Local,
    Remote,
}

pub struct Generated<T> {
    pub value: T,
}

#[allow(async_fn_in_trait)]
pub trait GenerationPolicy {
    fn deidentify(&self, text: &str, patient: &PatientRecord) -> String;
    async fn summarize_encounter(
        &self,
        text: &str,
        prior: &NoteSections,
        number: u32,
    ) -> AppResult<Generated<NoteSections>>;
    async fn generate_mdm(
        &self,
        sections: &NoteSections,
        objective: &str,
        disposition: &str,
    ) -> AppResult<Generated<String>>;
}

pub struct RemoteGeneration;
impl GenerationPolicy for RemoteGeneration {
    fn deidentify(&self, _: &str, _: &PatientRecord) -> String {
        String::new()
    }
    async fn summarize_encounter(
        &self,
        _: &str,
        _: &NoteSections,
        _: u32,
    ) -> AppResult<Generated<NoteSections>> {
        Err(AppError::Unsupported)
    }
    async fn generate_mdm(
        &self,
        _: &NoteSections,
        _: &str,
        _: &str,
    ) -> AppResult<Generated<String>> {
        Err(AppError::Unsupported)
    }
}

/// Feature-gated adapter preserves existing CLI constructors and provider errors.
#[cfg(feature = "local-generation")]
impl<S: er_ai::summarizer::Summarizer> GenerationPolicy for S {
    fn deidentify(&self, text: &str, patient: &PatientRecord) -> String {
        er_ai::deidentify::Deidentifier::new().deidentify(text, patient)
    }
    async fn summarize_encounter(
        &self,
        text: &str,
        prior: &NoteSections,
        number: u32,
    ) -> AppResult<Generated<NoteSections>> {
        Ok(Generated {
            value: er_ai::summarizer::Summarizer::summarize_encounter(self, text, prior, number)
                .await?
                .value,
        })
    }
    async fn generate_mdm(
        &self,
        sections: &NoteSections,
        objective: &str,
        disposition: &str,
    ) -> AppResult<Generated<String>> {
        Ok(Generated {
            value: er_ai::summarizer::Summarizer::generate_mdm(
                self,
                sections,
                objective,
                disposition,
            )
            .await?
            .value,
        })
    }
}
