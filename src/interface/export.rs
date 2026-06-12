use crate::domain::{Diagnosis, Disposition, Medication, NoteSections, PatientRecord};

/// Format a patient note for export (copy to clipboard → paste into Epic).
/// Plain text only — no markdown, no HTML. Epic note fields accept plain text.
pub fn format_note(
    patient: &PatientRecord,
    sections: &NoteSections,
    disposition: Option<&Disposition>,
    results_text: Option<&str>,
    medications: &[Medication],
    diagnoses: &[Diagnosis],
    data_reviewed: &[String],
) -> String {
    let mut out = String::with_capacity(2048);

    out.push_str("ED PROVIDER NOTE\n\n");

    out.push_str(&format!("Chief Complaint: {}\n\n", patient.chief_complaint));

    if let Some(hpi) = &sections.hpi {
        out.push_str(&format!("HPI:\n{hpi}\n\n"));
    }

    if let Some(ros) = &sections.ros {
        out.push_str(&format!("Review of Systems:\n{ros}\n\n"));
    } else {
        out.push_str("Review of Systems:\nSee HPI\n\n");
    }

    if let Some(exam) = &sections.exam {
        out.push_str(&format!("Physical Exam:\n{exam}\n\n"));
    }

    if let Some(results) = results_text {
        out.push_str(&format!("Results Reviewed:\n{results}\n\n"));
    }

    push_lines(&mut out, "Medications", &lines(medications, Medication::summary_line));
    push_lines(&mut out, "Data Reviewed", data_reviewed);

    if let Some(mdm) = &sections.mdm {
        out.push_str(&format!("Medical Decision Making:\n{mdm}\n\n"));
    }

    push_lines(&mut out, "Diagnoses", &lines(diagnoses, Diagnosis::summary_line));

    if let Some(dispo) = disposition {
        out.push_str(&format!("Disposition: {}\n", format_disposition(dispo)));
    }

    out.push_str("\n---\nNote assisted by AI documentation tool. All content reviewed and approved by signing provider.\n");

    out
}

/// Map a slice of items to their one-line summaries.
fn lines<T>(items: &[T], summary: impl Fn(&T) -> String) -> Vec<String> {
    items.iter().map(summary).collect()
}

/// Append a titled section listing one item per line; skip if empty.
fn push_lines(out: &mut String, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("{title}:\n"));
    for line in lines {
        out.push_str(&format!("{line}\n"));
    }
    out.push('\n');
}

fn format_disposition(dispo: &Disposition) -> String {
    match dispo {
        Disposition::Discharge {
            instructions,
            follow_up,
        } => {
            let mut s = format!("Discharge\nInstructions: {instructions}");
            if let Some(fu) = follow_up {
                s.push_str(&format!("\nFollow-up: {fu}"));
            }
            s
        }
        Disposition::Admit { service, bed } => {
            let mut s = format!("Admit to {service}");
            if let Some(b) = bed {
                s.push_str(&format!(", Bed: {b}"));
            }
            s
        }
        Disposition::Transfer {
            destination,
            reason,
        } => {
            let mut s = format!("Transfer to {destination}");
            if let Some(r) = reason {
                s.push_str(&format!(", Reason: {r}"));
            }
            s
        }
        Disposition::AMA => "Left Against Medical Advice (AMA)".into(),
        Disposition::LWBS => "Left Without Being Seen (LWBS)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_note_format() {
        let patient = PatientRecord::new("chest pain");
        let sections = NoteSections {
            hpi: Some("44yo M with acute onset chest pain.".into()),
            ros: None,
            exam: Some("HEENT: normal. CV: RRR.".into()),
            mdm: Some("Low risk chest pain.".into()),
            disposition_text: None,
        };
        let dispo = Disposition::Discharge {
            instructions: "Return if worse.".into(),
            follow_up: Some("PCP in 2 days".into()),
        };

        let output = format_note(&patient, &sections, Some(&dispo), None, &[], &[], &[]);

        assert!(output.contains("ED PROVIDER NOTE"));
        assert!(output.contains("chest pain"));
        assert!(output.contains("44yo M"));
        assert!(output.contains("Low risk chest pain"));
        assert!(output.contains("Discharge"));
        assert!(output.contains("signing provider"));
    }

    #[test]
    fn renders_medications_diagnoses_and_data_reviewed() {
        use crate::domain::{Medication, MedicationKind};
        use chrono::Utc;
        use uuid::Uuid;

        let patient = PatientRecord::new("chest pain");
        let patient_id = Uuid::now_v7();
        let sections = NoteSections {
            hpi: Some("44yo M with chest pain.".into()),
            ros: None,
            exam: None,
            mdm: None,
            disposition_text: None,
        };
        let medications = vec![Medication {
            id: Uuid::now_v7(),
            patient_id,
            name: "Aspirin".into(),
            dose: "324mg".into(),
            route: Some("PO".into()),
            rxnorm_code: None,
            kind: MedicationKind::AdministeredInEd,
            administered_at: Utc::now(),
            frequency: String::new(),
        }];
        let diagnoses = vec![Diagnosis {
            id: Uuid::now_v7(),
            patient_id,
            text: "Acute PE".into(),
            icd10_code: Some("I26.99".into()),
            is_primary: true,
        }];
        let data_reviewed = vec!["CTA chest: PE in RLL".to_string()];

        let output = format_note(
            &patient,
            &sections,
            None,
            None,
            &medications,
            &diagnoses,
            &data_reviewed,
        );

        assert!(output.contains("Medications:\nAspirin 324mg PO (administered in ED)"));
        assert!(output.contains("Diagnoses:\nAcute PE (primary) [I26.99]"));
        assert!(output.contains("Data Reviewed:\nCTA chest: PE in RLL"));
    }
}
