use serde::{Deserialize, Serialize};

/// ED disposition — the most important single field in an ED note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Disposition {
    Discharge {
        instructions: String,
        follow_up: Option<String>,
    },
    Admit {
        service: String,
        bed: Option<String>,
    },
    Transfer {
        destination: String,
        reason: Option<String>,
    },
    /// Against medical advice
    AMA,
    /// Left without being seen
    LWBS,
}
