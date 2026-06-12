use crate::domain::audit::{AuditAction, AuditEntry};
use crate::domain::events::NoteEvent;
use crate::domain::note::NoteSectionVersion;
use crate::domain::patient::PatientId;
use crate::domain::state::ingest_result;
use crate::domain::{Encounter, NoteSections, PatientNote};

/// Reconstruct a PatientNote from a sequence of events, starting from
/// a fresh Registered state. Events must be in order (caller guarantees
/// this by querying with ORDER BY sequence_id ASC).
pub fn replay(patient_id: PatientId, events: &[NoteEvent]) -> PatientNote {
    let mut note = PatientNote::new(patient_id);
    for event in events {
        apply_event(&mut note, event);
    }
    note
}

pub fn apply_event(note: &mut PatientNote, event: &NoteEvent) {
    match event {
        NoteEvent::AudioStarted {
            encounter_id,
            capture_mode,
            clinical_occurred_at,
            ..
        } => {
            // Drive the state machine first so the encounter_count bumps
            // handle_audio_started calls self.apply_transition()
            // which takes the state out, appl
            note.handle_audio_started(*clinical_occurred_at);

            note.encounters.push(Encounter {
                id: *encounter_id,
                patient_id: note.patient_id,
                capture_mode: capture_mode.clone(),
                started_at: *clinical_occurred_at,
                ended_at: None,
                transcript: None,
                encounter_number: note.encounter_count,
            });
        }

        NoteEvent::AudioStopped {
            encounter_id,
            clinical_occurred_at,
            ..
        } => {
            note.handle_audio_stopped(*clinical_occurred_at);
            if let Some(enc) = note.encounters.iter_mut().find(|e| e.id == *encounter_id) {
                enc.ended_at = Some(*clinical_occurred_at);
            }
            // If no matching encounter: silently ignore. Replay tolerates
            // events referencing missing encounters (should never happen
            // in a healthy stream, but panicking on bad data is worse).
        }

        NoteEvent::TranscriptReady {
            encounter_id, text, ..
        } => {
            if let Some(enc) = note.encounters.iter_mut().find(|e| e.id == *encounter_id) {
                enc.transcript = Some(text.clone());
            }
        }

        NoteEvent::EncounterSummarized {
            sections,
            source_encounter_ids,
            clinical_occurred_at,
            ..
        } => {
            merge_sections(&mut note.note_sections, sections);
            note.record_section_versions(sections, source_encounter_ids, *clinical_occurred_at);
            note.version += 1;
            note.updated_at = *clinical_occurred_at;
            note.audit_log.push(AuditEntry::with_timestamp(
                AuditAction::NoteSectionGenerated,
                "",
                *clinical_occurred_at,
            ));
        }

        NoteEvent::MedicationRecorded {
            medication,
            clinical_occurred_at,
            ..
        } => {
            let can_mutate = note
                .state
                .as_ref()
                .map(|s| s.can_ingest_results())
                .unwrap_or(false);
            if can_mutate {
                note.medication.push(medication.clone());
                note.version += 1;
                note.updated_at = *clinical_occurred_at;
                note.audit_log.push(AuditEntry::with_timestamp(
                    AuditAction::MedicationRecorded,
                    "",
                    *clinical_occurred_at,
                ));
            }
        }

        NoteEvent::DataReviewed {
            physician_review,
            clinical_occurred_at,
            ..
        } => {
            let can_mutate = note
                .state
                .as_ref()
                .map(|s| s.can_ingest_results())
                .unwrap_or(false);

            if can_mutate {
                note.data_reviewed.push(physician_review.clone());
                note.version += 1;
                note.updated_at = *clinical_occurred_at;
                note.audit_log.push(AuditEntry::with_timestamp(
                    AuditAction::DataReviewed,
                    "",
                    *clinical_occurred_at,
                ));
            }
        }

        NoteEvent::DiagnosisAssigned {
            diagnosis,
            clinical_occurred_at,
            ..
        } => {
            let can_mutate = note
                .state
                .as_ref()
                .map(|s| s.can_ingest_results())
                .unwrap_or(false);

            if can_mutate {
                note.diagnoses.push(diagnosis.clone());
                note.version += 1;
                note.updated_at = *clinical_occurred_at;
                note.audit_log.push(AuditEntry::with_timestamp(
                    AuditAction::DiagnosisAssigned,
                    "",
                    *clinical_occurred_at,
                ));
            }
        }
        

        NoteEvent::MdmGenerated {
            mdm_text,
            source_encounter_ids,
            clinical_occurred_at,
            ..
        } => {
            note.note_sections.mdm = Some(mdm_text.clone());
            note.section_history.push(NoteSectionVersion {
                section: "mdm".into(),
                content: mdm_text.clone(),
                version: note.version + 1,
                generated_at: *clinical_occurred_at,
                source_encounter_ids: source_encounter_ids.clone(),
            });
            note.version += 1;
            note.updated_at = *clinical_occurred_at;
            note.audit_log.push(AuditEntry::with_timestamp(
                AuditAction::MdmGenerated,
                "",
                *clinical_occurred_at,
            ));
        }

        NoteEvent::DispositionSet {
            disposition,
            clinical_occurred_at,
            ..
        } => {
            // Mirror ingest_result's immutability guard:
            //    - Signed notes reject mutation.
            let can_mutate = note
                .state
                .as_ref()
                .map(|s| s.can_ingest_results())
                .unwrap_or(false);
            if can_mutate {
                note.disposition = Some(disposition.clone());
                note.version += 1;
                note.updated_at = *clinical_occurred_at;
                note.audit_log.push(AuditEntry::with_timestamp(
                    AuditAction::DispositionSet,
                    "",
                    *clinical_occurred_at,
                ));
            }
        }

        NoteEvent::ResultReceived {
            result,
            clinical_occurred_at,
            ..
        } => {
            let _ = ingest_result(note, result.clone(), *clinical_occurred_at);
        }

        NoteEvent::TriggerMDM {
            clinical_occurred_at,
            ..
        } => note.handle_trigger_mdm(*clinical_occurred_at),
        NoteEvent::ReviewReady {
            clinical_occurred_at,
            ..
        } => note.handle_review_ready(*clinical_occurred_at),
        NoteEvent::Sign {
            clinical_occurred_at,
            ..
        } => note.handle_sign(*clinical_occurred_at),
    }
}

/// Fold an EncounterSummarized delta into the note's sections.
/// Only HPI/ROS/Exam are touched, and only when the incoming field is Some.
/// MDM and disposition_text are NEVER modified here - they are owned by
/// MdmGenerated / DispositionSet respectively.
fn merge_sections(dest: &mut NoteSections, source: &NoteSections) {
    if source.hpi.is_some() {
        dest.hpi = source.hpi.clone();
    }

    if source.ros.is_some() {
        dest.ros = source.ros.clone();
    }
    if source.exam.is_some() {
        dest.exam = source.exam.clone();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use crate::domain::CaptureMode;
    use crate::domain::note::NotePhase;
    use crate::domain::result::ClinicalResult;
    use crate::domain::{LabResult, ResultSource, ResultType};

    use super::*;

    fn build_test_lab_result(patient_id: PatientId) -> ClinicalResult {
        ClinicalResult {
            id: Uuid::now_v7(),
            patient_id,
            source: ResultSource::Manual,
            timestamp: Utc::now(),
            result_type: ResultType::Lab(LabResult {
                fhir_id: None,
                loinc_code: None,
                display_name: "Troponin".into(),
                value: 0.04,
                unit: "ng/mL".into(),
                reference_range: Some((0.0, 0.04)),
                is_abnormal: false,
            }),
        }
    }

    #[test]
    fn apply_event_audio_started_creates_encounter() {
        let mut note = PatientNote::new(Uuid::now_v7());
        let encounter_id = Uuid::now_v7();
        let event = NoteEvent::audio_started(note.patient_id, encounter_id, CaptureMode::Ambient);

        apply_event(&mut note, &event);

        assert_eq!(note.phase, NotePhase::Encountering);
        assert_eq!(note.encounters.len(), 1);
        assert_eq!(note.encounters[0].id, encounter_id);
        assert_eq!(note.encounter_count, 1);
    }

    #[test]
    fn apply_event_audio_stopped_closes_encounter() {
        let mut note = PatientNote::new(Uuid::now_v7());
        let patient_id = note.patient_id;
        let encounter_id = Uuid::now_v7();
        apply_event(
            &mut note,
            &NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
        );
        apply_event(
            &mut note,
            &NoteEvent::audio_stopped(patient_id, encounter_id),
        );

        assert_eq!(note.phase, NotePhase::Idle);
        assert!(note.encounters[0].ended_at.is_some());
    }

    #[test]
    fn apply_event_transcript_attaches_to_encounter() {
        let mut note = PatientNote::new(Uuid::now_v7());
        let patient_id = note.patient_id;
        let encounter_id = Uuid::now_v7();
        apply_event(
            &mut note,
            &NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
        );
        apply_event(
            &mut note,
            &NoteEvent::transcript_ready(patient_id, encounter_id, "chest pain".into(), Utc::now()),
        );

        assert_eq!(note.encounters[0].transcript.as_deref(), Some("chest pain"));
    }

    #[test]
    fn replay_empty_events_returns_registered() {
        let note = replay(Uuid::now_v7(), &[]);
        assert_eq!(note.phase, NotePhase::Registered);
        assert_eq!(note.encounter_count, 0);
    }

    #[test]
    fn replay_full_lifecycle_reaches_signed() {
        let patient_id = Uuid::now_v7();
        let encounter_id = Uuid::now_v7();
        let events = vec![
            NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
            NoteEvent::transcript_ready(patient_id, encounter_id, "chest pain".into(), Utc::now()),
            NoteEvent::audio_stopped(patient_id, encounter_id),
            NoteEvent::trigger_mdm(patient_id),
            NoteEvent::review_ready(patient_id),
            NoteEvent::sign(patient_id),
        ];

        let note = replay(patient_id, &events);

        assert_eq!(note.phase, NotePhase::Signed);
        assert_eq!(note.encounter_count, 1);
        assert_eq!(note.encounters.len(), 1);
        assert_eq!(note.encounters[0].transcript.as_deref(), Some("chest pain"));
    }

    #[test]
    fn replay_multi_encounter_increments_count() {
        let patient_id = Uuid::now_v7();
        let e1 = Uuid::now_v7();
        let e2 = Uuid::now_v7();
        let events = vec![
            NoteEvent::audio_started(patient_id, e1, CaptureMode::Ambient),
            NoteEvent::audio_stopped(patient_id, e1),
            NoteEvent::audio_started(patient_id, e2, CaptureMode::Ambient),
            NoteEvent::audio_stopped(patient_id, e2),
        ];

        let note = replay(patient_id, &events);

        assert_eq!(note.encounter_count, 2);
        assert_eq!(note.encounters.len(), 2);
        assert_eq!(note.encounters[0].id, e1);
        assert_eq!(note.encounters[1].id, e2);
    }

    #[test]
    fn apply_event_result_received_after_sign_is_noop() {
        let patient_id = Uuid::now_v7();
        let encounter_id = Uuid::now_v7();
        let events = vec![
            NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
            NoteEvent::audio_stopped(patient_id, encounter_id),
            NoteEvent::trigger_mdm(patient_id),
            NoteEvent::review_ready(patient_id),
            NoteEvent::sign(patient_id),
        ];
        let mut note = replay(patient_id, &events);
        let initial_results_len = note.results.len();

        let late_result =
            NoteEvent::result_received(patient_id, build_test_lab_result(patient_id), Utc::now());
        apply_event(&mut note, &late_result);

        // Signed state rejects ingestion — results are unchanged.
        assert_eq!(note.results.len(), initial_results_len);
        assert_eq!(note.phase, NotePhase::Signed);
    }
}
