use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use er_domain::{PatientId, PatientRecord};

/// Strips PHI from text before sending to external LLM APIs.
///
/// This is a regex-based approach sufficient for MVP testing with clinician consent.
/// For production, use a certified NLP de-identification service.
pub struct Deidentifier {
    patient_map: HashMap<PatientId, String>,
    counter: u8,
    mrn_re: Regex,
    phone_re: Regex,
    date_re: Regex,
    ssn_re: Regex,
}

impl Deidentifier {
    pub fn new() -> Self {
        Self {
            patient_map: HashMap::new(),
            counter: 0,
            mrn_re: Regex::new(r"\b\d{7,10}\b").expect("valid regex"),
            phone_re: Regex::new(r"\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}").expect("valid regex"),
            date_re: Regex::new(r"\b\d{1,2}/\d{1,2}/\d{2,4}\b").expect("valid regex"),
            ssn_re: Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").expect("valid regex"),
        }
    }

    /// Replace PHI in text with safe placeholders.
    /// Maintains stable patient aliases across calls (Patient_A, Patient_B, ...).
    pub fn deidentify(&mut self, text: &str, patient: &PatientRecord) -> String {
        let counter = &mut self.counter;
        let alias = self
            .patient_map
            .entry(patient.id)
            .or_insert_with(|| {
                let label = format!("Patient_{}", (b'A' + *counter) as char);
                *counter = counter.saturating_add(1);
                label
            })
            .clone();

        let mut result = text.to_string();

        // Replace patient name variants.
        if let Some(ref name) = patient.last_name {
            result = result.replace(name, &alias);
        }
        if let Some(ref name) = patient.first_name {
            result = result.replace(name, &alias);
        }

        // Strip structured identifiers.
        result = self.ssn_re.replace_all(&result, "[SSN]").to_string();
        result = self.phone_re.replace_all(&result, "[PHONE]").to_string();
        result = self.mrn_re.replace_all(&result, "[MRN]").to_string();
        result = self.date_re.replace_all(&result, "[DATE]").to_string();

        result
    }

    /// Stateless structured-identifier scrub for a headless worker.
    ///
    /// The server intentionally stores no patient name, so it cannot provide the
    /// alias replacement used by `deidentify`. Spoken names may remain; the
    /// pilot relies on the provider BAA for that residual risk.
    pub fn scrub(text: &str) -> String {
        static SSN: OnceLock<Regex> = OnceLock::new();
        static PHONE: OnceLock<Regex> = OnceLock::new();
        static MRN: OnceLock<Regex> = OnceLock::new();
        static DATE: OnceLock<Regex> = OnceLock::new();

        let ssn = SSN.get_or_init(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap());
        let phone =
            PHONE.get_or_init(|| Regex::new(r"\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}").unwrap());
        let mrn = MRN.get_or_init(|| Regex::new(r"\b\d{7,10}\b").unwrap());
        let date = DATE.get_or_init(|| Regex::new(r"\b\d{1,2}/\d{1,2}/\d{2,4}\b").unwrap());

        let result = ssn.replace_all(text, "[SSN]");
        let result = phone.replace_all(&result, "[PHONE]");
        let result = mrn.replace_all(&result, "[MRN]");
        date.replace_all(&result, "[DATE]").into_owned()
    }

    /// Reset the deidentifier state (e.g., at shift end).
    pub fn reset(&mut self) {
        self.patient_map.clear();
        self.counter = 0;
    }
}

impl Default for Deidentifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_patient_name() {
        let mut d = Deidentifier::new();
        let mut patient = PatientRecord::new("chest pain");
        patient.first_name = Some("John".into());
        patient.last_name = Some("Smith".into());

        let text = "John Smith presents with chest pain";
        let result = d.deidentify(text, &patient);

        assert!(!result.contains("John"));
        assert!(!result.contains("Smith"));
        assert!(result.contains("Patient_A"));
    }

    #[test]
    fn strips_phone_numbers() {
        let mut d = Deidentifier::new();
        let patient = PatientRecord::new("chest pain");

        let text = "Call back at 510-555-1234";
        let result = d.deidentify(text, &patient);

        assert!(!result.contains("510-555-1234"));
        assert!(result.contains("[PHONE]"));
    }

    #[test]
    fn stable_aliases_across_calls() {
        let mut d = Deidentifier::new();
        let patient_a = PatientRecord::new("headache");
        let patient_b = PatientRecord::new("lac repair");

        let _ = d.deidentify("text", &patient_a);
        let _ = d.deidentify("text", &patient_b);
        let result = d.deidentify("text", &patient_a);

        // Patient A should still be Patient_A on the third call.
        assert!(d.patient_map.get(&patient_a.id).unwrap() == "Patient_A");
        assert!(d.patient_map.get(&patient_b.id).unwrap() == "Patient_B");
        let _ = result;
    }

    #[test]
    fn stateless_scrub_removes_structured_identifiers_but_is_honest_about_names() {
        let scrubbed = Deidentifier::scrub(
            "Mrs. Alvarez MRN 12345678, SSN 111-22-3333, phone 510-555-1234, DOB 1/2/1980",
        );
        assert_eq!(
            scrubbed,
            "Mrs. Alvarez MRN [MRN], SSN [SSN], phone [PHONE], DOB [DATE]"
        );
        // The server has no patient-name field to match. The BAA and clinician
        // consent mitigate this documented residual; a certified NLP scrubber
        // remains a future production hardening option.
        assert!(scrubbed.contains("Mrs. Alvarez"));
    }
}
