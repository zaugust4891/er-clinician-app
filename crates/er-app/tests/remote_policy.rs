//! Runs with --no-default-features: even offline-before-upload uses server STT.
use er_app::{
    EventBus,
    encounter_driver::{EncounterDriver, TranscriptionOwnership},
    generation::{GenerationOwnership, RemoteGeneration},
    shift_manager::ShiftManager,
};
use er_pipeline::{
    PipelineResult,
    audio::MockAudioCapture,
    stt::{Transcriber, TranscriptResult},
};
use er_store::{Repository, SqliteRepository};
use std::{collections::HashMap, sync::Arc};
struct ForbiddenStt;
impl Transcriber for ForbiddenStt {
    fn transcribe(&self, _: &[u8]) -> PipelineResult<TranscriptResult> {
        panic!("device must not transcribe before upload")
    }
}
#[test]
fn offline_remote_switch_shutdown_and_empty_end_do_not_generate() {
    for solo in [false, true] {
        for shutdown in [false, true] {
            let repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(
                er_store::db::open_memory_db().unwrap(),
            ));
            let notes = er_app::new_store();
            let bus = Arc::new(EventBus::new(64, Some(repo.clone())));
            let driver = EncounterDriver::new(
                MockAudioCapture::new(),
                ForbiddenStt,
                bus.clone(),
                notes.clone(),
            )
            .with_transcription(TranscriptionOwnership::Server);
            let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
            let mut manager = ShiftManager::new(
                er_domain::Shift::new(),
                HashMap::new(),
                driver,
                bus,
                notes,
                repo.clone(),
                RemoteGeneration,
                rt.clone(),
                GenerationOwnership::Remote,
            );
            let a = manager.register_patient("Synthetic A");
            let b = manager.register_patient("Synthetic B");
            manager.set_active_patient(a).unwrap();
            manager
                .start_encounter(if solo {
                    er_domain::CaptureMode::Dictation
                } else {
                    er_domain::CaptureMode::Ambient
                })
                .unwrap();
            if shutdown {
                manager.stop_active_encounter_for_shutdown().unwrap();
            } else {
                manager.set_active_patient(b).unwrap();
            }
            assert!(matches!(
                manager.trigger_mdm(),
                Err(er_app::AppError::Unsupported)
            ));
            manager.end_shift().unwrap();
            manager.end_shift().unwrap();
            assert_eq!(
                rt.block_on(repo.get_events(a))
                    .unwrap()
                    .iter()
                    .map(|e| e.event.event_type())
                    .collect::<Vec<_>>(),
                vec!["AudioStarted", "AudioStopped"]
            );
        }
    }
}
