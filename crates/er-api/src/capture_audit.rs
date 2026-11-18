//! Version 1 records consent actions and capture attribution, separately from notes.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConsentChoice {
    Unknown,
    Granted,
    Declined,
    Revoked,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsentAudit {
    pub revision: u64,
    pub status: ConsentChoice,
    pub recorded_at_ms: i64,
    pub policy_version: String,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentAudit {
    pub id: Uuid,
    pub mode: er_domain::CaptureMode,
    pub source: String,
    pub consent_revision: u64,
    pub started_at_ms: i64,
    pub stopped_at_ms: Option<i64>,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CaptureAuditV1 {
    pub consents: Vec<ConsentAudit>,
    pub segments: Vec<SegmentAudit>,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureAuditWrite {
    pub expected_version: u64,
    pub audit: CaptureAuditV1,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureAuditView {
    pub version: u64,
    pub clinician_id: Uuid,
    pub audit: CaptureAuditV1,
}
impl CaptureAuditV1 {
    pub fn valid(&self, now: DateTime<Utc>) -> bool {
        let valid_time = |t: i64| t >= 0 && t <= now.timestamp_millis() + 5000;
        let mut ids = std::collections::HashSet::new();
        self.consents.len() <= 4096
            && self.segments.len() <= 4096
            && self.consents.iter().enumerate().all(|(i, c)| {
                c.revision == i as u64 + 1
                    && valid_time(c.recorded_at_ms)
                    && !c.policy_version.trim().is_empty()
                    && c.policy_version.len() <= 128
                    && !c.policy_version.contains('\0')
                    && (i == 0 || c.recorded_at_ms >= self.consents[i - 1].recorded_at_ms)
            })
            && self.segments.iter().all(|s| {
                !s.id.is_nil()
                    && ids.insert(s.id)
                    && ["phone", "watch"].contains(&s.source.as_str())
                    && valid_time(s.started_at_ms)
                    && s.stopped_at_ms
                        .is_none_or(|t| valid_time(t) && t >= s.started_at_ms)
                    && s.consent_revision <= self.consents.len() as u64
                    && (s.mode == er_domain::CaptureMode::Dictation
                        || s.consent_revision > 0
                            && self.consents[s.consent_revision as usize - 1].status
                                == ConsentChoice::Granted)
            })
    }
    /// History cannot be rewritten; an unfinished segment may gain one end time.
    pub fn extends(&self, old: &Self) -> bool {
        self.consents.starts_with(&old.consents)
            && self.segments.len() >= old.segments.len()
            && old.segments.iter().zip(&self.segments).all(|(a, b)| {
                let mut expected = a.clone();
                if expected.stopped_at_ms.is_none() {
                    expected.stopped_at_ms = b.stopped_at_ms;
                }
                expected == *b
            })
    }
}
