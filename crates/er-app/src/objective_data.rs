//! Shared rendering of structured note facts for the MDM model.
//!
//! Local mode and the server worker must build the same prompt input. Keeping
//! this renderer in `er-app` prevents either orchestration layer from inventing
//! a second clinical representation.

use er_domain::PatientNote;
use er_domain::note::format_result_line;

pub fn assemble_objective_data(note: &PatientNote) -> String {
    let mut out = String::new();

    if !note.results.is_empty() {
        out.push_str("Results:\n");
        for result in &note.results {
            out.push_str(&format!("- {}\n", format_result_line(result)));
        }
    }

    if !note.medication.is_empty() {
        out.push_str("Medications:\n");
        for medication in &note.medication {
            out.push_str(&format!("- {}\n", medication.summary_line()));
        }
    }

    if !note.diagnoses.is_empty() {
        out.push_str("Diagnoses:\n");
        for diagnosis in &note.diagnoses {
            let primary = if diagnosis.is_primary {
                " (primary)"
            } else {
                ""
            };
            out.push_str(&format!("- {}{}\n", diagnosis.text, primary));
        }
    }

    if !note.data_reviewed.is_empty() {
        out.push_str("Other data reviewed (orders/consults):\n");
        for item in &note.data_reviewed {
            out.push_str(&format!("- {item}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use er_domain::{Diagnosis, PatientNote};
    use uuid::Uuid;

    use super::assemble_objective_data;

    #[test]
    fn renders_all_populated_sections() {
        let mut note = PatientNote::new(Uuid::now_v7());
        let patient_id = note.patient_id;
        note.diagnoses.push(Diagnosis {
            id: Uuid::now_v7(),
            patient_id,
            text: "Acute PE".into(),
            icd10_code: Some("I26.99".into()),
            is_primary: true,
        });
        note.diagnoses.push(Diagnosis {
            id: Uuid::now_v7(),
            patient_id,
            text: "Hypoxia".into(),
            icd10_code: None,
            is_primary: false,
        });
        note.data_reviewed.push("CTA chest: PE in RLL".into());

        let rendered = assemble_objective_data(&note);
        assert!(rendered.contains("- Acute PE (primary)"));
        assert!(rendered.contains("- Hypoxia\n"));
        assert!(rendered.contains("Diagnoses:"));
        assert!(rendered.contains("Other data reviewed (orders/consults):"));
        assert!(rendered.contains("- CTA chest: PE in RLL"));
    }

    #[test]
    fn blank_note_renders_empty_input() {
        assert_eq!(
            assemble_objective_data(&PatientNote::new(Uuid::now_v7())),
            ""
        );
    }
}
