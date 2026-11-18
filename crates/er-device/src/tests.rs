use super::*;
use chrono::Utc;
use er_store::{Repository, SqliteRepository};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use uuid::Uuid;

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("er-device-{}.sqlite", Uuid::now_v7()));
        Self(path)
    }
    fn config(&self) -> DeviceConfig {
        DeviceConfig {
            database_path: self.0.to_str().unwrap().into(),
            account_scope: "synthetic-account".into(),
        }
    }
    fn sql(&self, sql: &str) {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
        let _ = std::fs::remove_dir_all(format!("{}.audio", self.0.display()));
    }
}
struct Capture {
    starts: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
}
impl CaptureAdapter for Capture {
    fn available(&self) -> bool {
        true
    }
    fn start(&mut self, _: Uuid, _: Uuid, _: bool, _: &std::path::Path) -> Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn capture() -> (Box<dyn CaptureAdapter>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    (
        Box::new(Capture {
            starts: starts.clone(),
            stops: stops.clone(),
        }),
        starts,
        stops,
    )
}
fn now() -> i64 {
    Utc::now().timestamp_millis()
}
fn patient(core: &mut DeviceCore, room: &str) -> Uuid {
    Uuid::parse_str(
        &core
            .add_patient("Synthetic complaint 🫀".into(), Some(room.into()), now())
            .unwrap(),
    )
    .unwrap()
}
fn definition() -> DefinitionView {
    DefinitionView {
        id: String::new(),
        version: 0,
        name: "Concise note".into(),
        kind: DefinitionKind::Template,
        instructions: String::new(),
        sections: vec![SectionView {
            section: "hpi".into(),
            instructions: "Use brief prose".into(),
        }],
        archived: false,
        is_default: true,
    }
}

#[test]
fn durable_local_slice_scope_order_and_prior_shifts() {
    let db = TestDb::new();
    let mut core = DeviceCore::open(db.config()).unwrap();
    let shift = core.start_shift(now()).unwrap();
    assert_eq!(core.start_shift(now()), Err(DeviceError::Conflict));
    let a = patient(&mut core, "Room 4");
    let b = patient(&mut core, "Hall 3");
    core.select_patient(Some(a), now()).unwrap();
    core.set_consent(a, ConsentStatus::Declined, "synthetic-v1".into(), 0, now())
        .unwrap();
    core.set_mode(a, true).unwrap();
    let context = core
        .save_context(
            ContextView {
                id: "".into(),
                patient_id: a.to_string(),
                version: 0,
                category: ContextCategory::History,
                text: "Manual synthetic history".into(),
                source: "Clinician supplied".into(),
                clinical_at_ms: None,
                entered_at_ms: now(),
                reviewed: false,
            },
            0,
        )
        .unwrap();
    let d = core.save_definition(definition(), 0).unwrap();
    let did = Uuid::parse_str(&d).unwrap();
    let mut update = core.definition_versions(did)[0].clone();
    update.sections[0].instructions = "Unicode preserved — 简洁".into();
    core.save_definition(update.clone(), 1).unwrap();
    assert_eq!(core.save_definition(update, 1), Err(DeviceError::Conflict));
    let duplicate = core
        .duplicate_definition(did, "Second template".into())
        .unwrap();
    core.restore_definition(did, 1, 2).unwrap();
    let mut prompt = definition();
    prompt.kind = DefinitionKind::AdmissionMessage;
    prompt.sections.clear();
    prompt.instructions = "Admission message for review".into();
    let prompt_id = core.save_definition(prompt, 0).unwrap();
    core.save_manual_draft(a, "Independent local draft".into(), core.view().revision)
        .unwrap();
    drop(core);
    let mut core = DeviceCore::open(db.config()).unwrap();
    let view = core.view();
    assert_eq!(view.shift.unwrap().id, shift);
    assert_eq!(
        view.patients
            .iter()
            .map(|p| p.id.clone())
            .collect::<Vec<_>>(),
        vec![a.to_string(), b.to_string()]
    );
    assert_eq!(view.patients[0].consent.status, ConsentStatus::Declined);
    assert!(view.patients[0].recording.solo);
    assert!(!view.patients[0].recording.hardware_available);
    assert_eq!(core.contexts(a).unwrap()[0].id, context);
    assert!(!core.contexts(a).unwrap()[0].reviewed);
    assert_eq!(core.definition_versions(did).len(), 3);
    assert_eq!(
        core.definition_versions(Uuid::parse_str(&duplicate).unwrap())
            .len(),
        1
    );
    assert_eq!(
        core.definition_versions(Uuid::parse_str(&prompt_id).unwrap())[0].kind,
        DefinitionKind::AdmissionMessage
    );
    assert_eq!(
        core.note_view(a).unwrap().manual_draft.as_deref(),
        Some("Independent local draft")
    );
    core.end_shift(now()).unwrap();
    core.end_shift(now()).unwrap();
    let next = core.start_shift(now()).unwrap();
    assert_ne!(next, shift);
    drop(core);
    let core = DeviceCore::open(db.config()).unwrap();
    assert_eq!(core.shifts().len(), 2);
    drop(core);
    let mut wrong = db.config();
    wrong.account_scope = "another-account".into();
    assert!(matches!(DeviceCore::open(wrong), Err(DeviceError::Scope)));
}

#[test]
fn consent_segment_timeline_and_all_remote_exit_paths() {
    for solo in [false, true] {
        for exit in ["pause", "switch", "clear", "revoke", "end", "close"] {
            let db = TestDb::new();
            let (adapter, starts, _) = capture();
            let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
            core.start_shift(now()).unwrap();
            let a = patient(&mut core, "Room 4");
            let b = patient(&mut core, "Room 7");
            core.select_patient(Some(a), now()).unwrap();
            assert!(core.start_capture(a, now()).is_err());
            assert_eq!(starts.load(Ordering::SeqCst), 0);
            core.set_consent(a, ConsentStatus::Granted, "synthetic".into(), 0, now())
                .unwrap();
            core.set_mode(a, solo).unwrap();
            let first = core.start_capture(a, now()).unwrap();
            match exit {
                "pause" => core.pause(a, now(), "waiting_for_results".into()).unwrap(),
                "switch" => core.select_patient(Some(b), now()).unwrap(),
                "clear" => core.select_patient(None, now()).unwrap(),
                "revoke" => core
                    .set_consent(a, ConsentStatus::Revoked, "synthetic".into(), 1, now())
                    .unwrap(),
                "end" => core.end_shift(now()).unwrap(),
                _ => core.close().unwrap(),
            }
            let repo =
                SqliteRepository::new(er_store::db::open_db(db.0.to_str().unwrap()).unwrap());
            let rt = tokio::runtime::Runtime::new().unwrap();
            let events = rt.block_on(repo.get_events(a)).unwrap();
            assert_eq!(
                events
                    .iter()
                    .map(|e| e.event.event_type())
                    .collect::<Vec<_>>(),
                vec!["AudioStarted", "AudioStopped"]
            );
            assert!(core.note_view(a).unwrap().hpi.is_none());
            assert_eq!(core.note_view(a).unwrap().encounter_count, 1);
            if exit == "pause" {
                core.select_patient(Some(b), now()).unwrap();
                core.set_mode(b, true).unwrap();
                core.start_capture(b, now()).unwrap();
                core.pause(b, now(), "done".into()).unwrap();
                core.select_patient(Some(a), now()).unwrap();
                let second = core.start_capture(a, now()).unwrap();
                assert_ne!(first, second);
                core.pause(a, now(), "done".into()).unwrap();
                assert_eq!(core.note_view(a).unwrap().encounter_count, 2);
            }
        }
    }
}

#[test]
fn failed_start_stop_and_selection_are_recoverable() {
    let db = TestDb::new();
    let (adapter, starts, stops) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    core.start_shift(now()).unwrap();
    let a = patient(&mut core, "4");
    let b = patient(&mut core, "7");
    core.select_patient(Some(a), now()).unwrap();
    core.set_mode(a, true).unwrap();
    db.sql("CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(FAIL,'private sentinel'); END;");
    assert_eq!(core.start_capture(a, now()), Err(DeviceError::Storage));
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert_eq!(core.note_view(a).unwrap().encounter_count, 0);
    db.sql("DROP TRIGGER fail_event;");
    core.start_capture(a, now()).unwrap();
    db.sql("CREATE TRIGGER fail_stop BEFORE INSERT ON events WHEN NEW.event_type='AudioStopped' BEGIN SELECT RAISE(FAIL,'private sentinel'); END;");
    assert_eq!(
        core.pause(a, now(), "wait".into()),
        Err(DeviceError::Storage)
    );
    assert_eq!(
        core.view().patients[0].recording.status,
        RecorderStatus::PendingStop
    );
    let stopped = stops.load(Ordering::SeqCst);
    assert!(core.start_capture(b, now()).is_err());
    db.sql("DROP TRIGGER fail_stop;");
    core.pause(a, now(), "retry".into()).unwrap();
    assert_eq!(stops.load(Ordering::SeqCst), stopped);
    core.start_capture(a, now()).unwrap();
    db.sql("CREATE TRIGGER fail_selection BEFORE UPDATE ON device_state WHEN json_extract(NEW.payload,'$.shifts[0].selected_patient_id') != json_extract(OLD.payload,'$.shifts[0].selected_patient_id') BEGIN SELECT RAISE(FAIL,'private sentinel'); END;");
    assert_eq!(
        core.select_patient(Some(b), now()),
        Err(DeviceError::Storage)
    );
    assert_eq!(
        core.view().shift.unwrap().selected_patient_id,
        Some(a.to_string())
    );
    assert_eq!(
        core.view().patients[0].recording.status,
        RecorderStatus::Paused
    );
    drop(core);
    let mut core = DeviceCore::open(db.config()).unwrap();
    assert_eq!(
        core.view().shift.unwrap().selected_patient_id,
        Some(a.to_string())
    );
    db.sql("DROP TRIGGER fail_selection;");
    core.select_patient(Some(b), now()).unwrap();
}

#[test]
fn revoke_stops_before_storage_failure_and_cannot_restart_ambient() {
    let db = TestDb::new();
    let (adapter, _, stops) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    core.start_shift(now()).unwrap();
    let p = patient(&mut core, "4");
    core.select_patient(Some(p), now()).unwrap();
    core.set_consent(p, ConsentStatus::Granted, "synthetic".into(), 0, now())
        .unwrap();
    core.start_capture(p, now()).unwrap();
    db.sql("CREATE TRIGGER fail_meta BEFORE UPDATE ON device_state BEGIN SELECT RAISE(FAIL,'private sentinel'); END;");
    assert_eq!(
        core.set_consent(p, ConsentStatus::Revoked, "synthetic".into(), 1, now()),
        Err(DeviceError::Storage)
    );
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        core.view().patients[0].recording.status,
        RecorderStatus::PendingStop
    );
    db.sql("DROP TRIGGER fail_meta;");
    core.pause(p, now(), "retry".into()).unwrap();
    assert!(core.start_capture(p, now()).is_err());
    core.set_consent(p, ConsentStatus::Revoked, "synthetic".into(), 1, now())
        .unwrap();
    assert!(!core.view().patients[0].ambient_eligible);
}

#[test]
fn interrupted_reopen_requires_evidence_and_never_extends_window() {
    let db = TestDb::new();
    let (adapter, _, _) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    core.start_shift(now()).unwrap();
    let p = patient(&mut core, "4");
    core.select_patient(Some(p), now()).unwrap();
    core.set_mode(p, true).unwrap();
    let start = now();
    core.start_capture(p, start).unwrap();
    drop(core);
    let (adapter, _, _) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    assert_eq!(
        core.view().patients[0].recording.status,
        RecorderStatus::Interrupted
    );
    assert!(core.start_capture(p, now()).is_err());
    assert_eq!(
        core.note_view(p).unwrap().phase,
        ClinicalPhase::Encountering
    );
    assert!(core.reconcile_interrupted(p, start - 1).is_err());
    core.reconcile_interrupted(p, start).unwrap();
    core.start_capture(p, now()).unwrap();
    core.pause(p, now(), "done".into()).unwrap();
    assert_eq!(core.note_view(p).unwrap().encounter_count, 2);
}

#[test]
fn context_stale_edit_wrong_visit_and_capture_phase_guards() {
    use er_domain::{CaptureMode, NoteEvent};
    let db = TestDb::new();
    let mut core = DeviceCore::open(db.config()).unwrap();
    core.start_shift(now()).unwrap();
    let a = patient(&mut core, "4");
    let b = patient(&mut core, "7");
    let item = ContextView {
        id: "".into(),
        patient_id: a.to_string(),
        version: 0,
        category: ContextCategory::Imaging,
        text: "Synthetic impression".into(),
        source: "Clinician pasted".into(),
        clinical_at_ms: None,
        entered_at_ms: now(),
        reviewed: false,
    };
    let cid = core.save_context(item, 0).unwrap();
    let mut edit = core.contexts(a).unwrap()[0].clone();
    edit.text = "Corrected synthetic impression".into();
    edit.reviewed = true;
    core.save_context(edit.clone(), 1).unwrap();
    assert_eq!(
        core.save_context(edit.clone(), 1),
        Err(DeviceError::Conflict)
    );
    edit.patient_id = b.to_string();
    assert_eq!(core.save_context(edit, 0), Err(DeviceError::NotFound));
    assert_eq!(core.contexts(a).unwrap()[0].id, cid);
    assert_eq!(core.contexts(a).unwrap()[0].version, 2);
    drop(core);
    let repo = SqliteRepository::new(er_store::db::open_db(db.0.to_str().unwrap()).unwrap());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let segment = Uuid::now_v7();
    for e in [
        NoteEvent::audio_started(a, segment, CaptureMode::Dictation),
        NoteEvent::audio_stopped(a, segment),
        NoteEvent::mdm_generated(a, "Existing synthetic MDM".into(), vec![segment]),
        NoteEvent::trigger_mdm(a),
    ] {
        rt.block_on(repo.append_event(a, Uuid::now_v7(), e))
            .unwrap();
    }
    let (adapter, starts, _) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    core.select_patient(Some(a), now()).unwrap();
    core.set_mode(a, true).unwrap();
    core.start_capture(a, now()).unwrap();
    assert_eq!(
        core.note_view(a).unwrap().mdm.as_deref(),
        Some("Existing synthetic MDM")
    );
    core.pause(a, now(), "done".into()).unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    drop(core);
    // Current domain disallows resume from PendingSignOff or Signed.
    for e in [NoteEvent::trigger_mdm(a), NoteEvent::review_ready(a)] {
        rt.block_on(repo.append_event(a, Uuid::now_v7(), e))
            .unwrap();
    }
    let (adapter, starts, _) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    assert_eq!(core.start_capture(a, now()), Err(DeviceError::InvalidState));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    drop(core);
    rt.block_on(repo.append_event(a, Uuid::now_v7(), NoteEvent::sign(a)))
        .unwrap();
    let (adapter, starts, _) = capture();
    let mut core = DeviceCore::open_with_capture(db.config(), adapter).unwrap();
    assert_eq!(core.start_capture(a, now()), Err(DeviceError::InvalidState));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
}

#[test]
fn metadata_failure_does_not_publish_and_corrupt_authority_is_rejected() {
    let db = TestDb::new();
    let mut core = DeviceCore::open(db.config()).unwrap();
    core.start_shift(now()).unwrap();
    db.sql("CREATE TRIGGER fail_meta BEFORE UPDATE ON device_state BEGIN SELECT RAISE(FAIL,'private sentinel'); END;");
    assert_eq!(
        core.add_patient("synthetic".into(), None, now()),
        Err(DeviceError::Storage)
    );
    assert!(core.view().patients.is_empty());
    db.sql("DROP TRIGGER fail_meta;");
    let p = patient(&mut core, "4");
    core.record_data_reviewed(p, "Synthetic reviewed results".into(), now())
        .unwrap();
    drop(core);
    db.sql("UPDATE events SET payload='corrupt authoritative event'");
    assert!(matches!(
        DeviceCore::open(db.config()),
        Err(DeviceError::Recovery)
    ));
}

#[test]
fn explicit_patient_commands_review_sign_and_export_server_fixtures() {
    use er_domain::{CaptureMode, NoteEvent, NoteSections};
    let db = TestDb::new();
    let mut core = DeviceCore::open(db.config()).unwrap();
    core.start_shift(now()).unwrap();
    let a = patient(&mut core, "4");
    let b = patient(&mut core, "7");
    core.select_patient(Some(b), now()).unwrap();
    // A late form submits A explicitly while B is selected.
    core.record_data_reviewed(a, "Synthetic ECG reviewed".into(), now())
        .unwrap();
    assert!(core.note_view(b).unwrap().data_reviewed.is_empty());
    assert_eq!(
        core.record_data_reviewed(Uuid::now_v7(), "wrong visit".into(), now()),
        Err(DeviceError::NotFound)
    );
    assert!(core.select_patient(Some(a), i64::MAX).is_err());
    assert!(core.select_patient(Some(a), 0).is_err());
    drop(core);
    // Rust-only server fixture: production FFI has no event injection export.
    let repo = SqliteRepository::new(er_store::db::open_db(db.0.to_str().unwrap()).unwrap());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let segment = Uuid::now_v7();
    for event in [
        NoteEvent::audio_started(a, segment, CaptureMode::Ambient),
        NoteEvent::audio_stopped(a, segment),
        NoteEvent::transcript_ready(a, segment, "Synthetic transcript 🫀".into(), Utc::now()),
        NoteEvent::encounter_summarized(
            a,
            segment,
            NoteSections {
                hpi: Some("Synthetic HPI".into()),
                ..Default::default()
            },
            vec![segment],
        ),
        NoteEvent::mdm_generated(a, "Synthetic MDM".into(), vec![segment]),
        NoteEvent::trigger_mdm(a),
    ] {
        rt.block_on(repo.append_event(a, Uuid::now_v7(), event))
            .unwrap();
    }
    let mut core = DeviceCore::open(db.config()).unwrap();
    assert_eq!(
        core.note_view(a).unwrap().hpi.as_deref(),
        Some("Synthetic HPI")
    );
    assert!(core.note_view(a).unwrap().exam.is_none());
    core.review_ready(a, now()).unwrap();
    core.sign(a, now()).unwrap();
    assert_eq!(
        core.view().shift.unwrap().selected_patient_id,
        Some(b.to_string())
    );
    let export = core.export_note(a).unwrap();
    assert!(export.contains("Synthetic HPI"));
    assert!(export.contains("Synthetic MDM"));
    let count = rt.block_on(repo.event_count()).unwrap();
    assert_eq!(
        core.record_data_reviewed(a, "must fail".into(), now()),
        Err(DeviceError::InvalidState)
    );
    assert_eq!(rt.block_on(repo.event_count()).unwrap(), count);
    assert_eq!(core.note_view(a).unwrap().phase, ClinicalPhase::Signed);
    let before = serde_json::to_string(
        &rt.block_on(er_app::load_patient_note(&repo, a))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    core.save_definition(definition(), 0).unwrap();
    let after = serde_json::to_string(
        &rt.block_on(er_app::load_patient_note(&repo, a))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(before, after);
    // A damaged optimization rebuilds; authoritative event corruption was tested separately.
    drop(core);
    db.sql("UPDATE snapshots SET checksum='corrupt'");
    let core = DeviceCore::open(db.config()).unwrap();
    assert_eq!(core.note_view(a).unwrap().phase, ClinicalPhase::Signed);
}

#[test]
fn bounded_admission_slow_listener_and_close_from_callback() {
    use std::sync::mpsc;
    let db = TestDb::new();
    let h = Arc::new(DeviceHandle::open(db.config()).unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = h.clone();
    let owner = std::thread::spawn(move || {
        running.execute(false, move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
    });
    entered_rx.recv().unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    let calls: Vec<_> = (0..80)
        .map(|_| {
            let h = h.clone();
            let tx = result_tx.clone();
            std::thread::spawn(move || {
                let r = h.execute(false, |c| Ok(c.view()));
                tx.send(r.map(|_| ())).unwrap();
            })
        })
        .collect();
    assert_eq!(
        result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        Err(DeviceError::Busy)
    );
    release_tx.send(()).unwrap();
    owner.join().unwrap().unwrap();
    for call in calls {
        call.join().unwrap();
    }
    struct SlowListener {
        started: mpsc::Sender<()>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
        first: std::sync::atomic::AtomicBool,
    }
    impl DeviceListener for SlowListener {
        fn invalidated(&self, _: u64) {
            if !self.first.swap(true, Ordering::SeqCst) {
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
        }
    }
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    h.set_listener(Some(Arc::new(SlowListener {
        started: started_tx,
        release: std::sync::Mutex::new(release_rx),
        first: std::sync::atomic::AtomicBool::new(false),
    })))
    .unwrap();
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    h.execute(true, |c| c.start_shift(now())).unwrap();
    for n in 0..20 {
        h.execute(true, move |c| {
            c.add_patient(format!("Synthetic {n}"), None, now())
        })
        .unwrap();
    }
    assert_eq!(
        h.execute(false, |c| Ok(c.view().patients.len())).unwrap(),
        20
    );
    h.set_listener(None).unwrap();
    release_tx.send(()).unwrap();
    struct ClosingListener {
        h: std::sync::Weak<DeviceHandle>,
        finished: mpsc::Sender<()>,
    }
    impl DeviceListener for ClosingListener {
        fn invalidated(&self, _: u64) {
            if let Some(h) = self.h.upgrade() {
                h.close().unwrap();
                self.finished.send(()).unwrap();
            }
        }
    }
    let (tx, rx) = mpsc::channel();
    h.set_listener(Some(Arc::new(ClosingListener {
        h: Arc::downgrade(&h),
        finished: tx,
    })))
    .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    h.close().unwrap();
}

#[test]
fn simultaneous_external_and_callback_close_do_not_wait_on_each_other() {
    use std::sync::mpsc;
    let db = TestDb::new();
    let h = Arc::new(DeviceHandle::open(db.config()).unwrap());
    struct Listener {
        h: std::sync::Weak<DeviceHandle>,
        entered: mpsc::Sender<()>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
    }
    impl DeviceListener for Listener {
        fn invalidated(&self, _: u64) {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            if let Some(h) = self.h.upgrade() {
                h.close().unwrap();
            }
        }
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    h.set_listener(Some(Arc::new(Listener {
        h: Arc::downgrade(&h),
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
    })))
    .unwrap();
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let closing = h.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let closer = std::thread::spawn(move || done_tx.send(closing.close()).unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while h.execute(false, |_| Ok(())).is_ok() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    release_tx.send(()).unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
    closer.join().unwrap();
}

#[test]
fn callbacks_reenter_concurrent_calls_close_and_fault_release_ownership() {
    let db = TestDb::new();
    let handle = Arc::new(DeviceHandle::open(db.config()).unwrap());
    assert!(matches!(
        DeviceHandle::open(db.config()),
        Err(DeviceError::Busy)
    ));
    struct Listener {
        handle: std::sync::Weak<DeviceHandle>,
        tx: std::sync::mpsc::Sender<()>,
    }
    impl DeviceListener for Listener {
        fn invalidated(&self, _: u64) {
            if let Some(h) = self.handle.upgrade() {
                h.execute(false, |c| Ok(c.view())).unwrap();
                let _ = self.tx.send(());
            }
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    handle
        .set_listener(Some(Arc::new(Listener {
            handle: Arc::downgrade(&handle),
            tx,
        })))
        .unwrap();
    handle.execute(true, |c| c.start_shift(now())).unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let h = handle.clone();
            std::thread::spawn(move || {
                h.execute(true, move |c| {
                    c.add_patient(format!("Synthetic {n}"), None, now())
                })
                .unwrap()
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(
        handle
            .execute(false, |c| Ok(c.view().patients.len()))
            .unwrap(),
        8
    );
    handle.close().unwrap();
    handle.close().unwrap();
    assert_eq!(handle.execute(false, |_| Ok(())), Err(DeviceError::Closed));
    let handle = DeviceHandle::open(db.config()).unwrap();
    assert_eq!(
        handle.execute::<()>(true, |c| {
            c.add_patient("Acknowledged before fault".into(), None, now())?;
            panic!("private sentinel")
        }),
        Err(DeviceError::Internal)
    );
    assert!(handle.execute(false, |_| Ok(())).is_err());
    handle.close().unwrap();
    let handle = DeviceHandle::open(db.config()).unwrap();
    handle.close().unwrap();
}

#[test]
fn process_kill_after_acknowledged_write_survives() {
    const CHILD: &str = "ER_DEVICE_KILL_CHILD";
    if let Some(path) = std::env::var_os(CHILD) {
        let config = DeviceConfig {
            database_path: path.to_str().unwrap().into(),
            account_scope: "synthetic-account".into(),
        };
        let mut core = DeviceCore::open(config).unwrap();
        core.start_shift(now()).unwrap();
        patient(&mut core, "Room 4");
        std::fs::write(format!("{}.ack", path.to_string_lossy()), "ack").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    let db = TestDb::new();
    let ack = PathBuf::from(format!("{}.ack", db.0.display()));
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::process_kill_after_acknowledged_write_survives",
        ])
        .env(CHILD, &db.0)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ack.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let acknowledged = ack.exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(acknowledged);
    let core = DeviceCore::open(db.config()).unwrap();
    assert_eq!(core.view().patients.len(), 1);
    assert_eq!(core.view().patients[0].room.as_deref(), Some("Room 4"));
    let _ = std::fs::remove_file(ack);
}

#[test]
fn sync_request_budget_and_connection_binding_survive_reopen() {
    let db = TestDb::new();
    let mut c = DeviceCore::open(db.config()).unwrap();
    let binding = er_store::sync::SyncBinding {
        origin: "https://example.invalid".into(),
        organization: Uuid::now_v7(),
        clinician: Uuid::now_v7(),
    };
    c.bind_sync(binding.clone()).unwrap();
    for _ in 0..48 {
        assert!(c.reserve_request().unwrap());
    }
    assert!(!c.reserve_request().unwrap());
    drop(c);
    let c = DeviceCore::open(db.config()).unwrap();
    assert_eq!(c.sync_view().unwrap().status, "budget_wait");
    let mut other = binding;
    other.clinician = Uuid::now_v7();
    assert_eq!(c.sync_can_connect(&other), Err(DeviceError::Scope));
}
#[test]
fn downloaded_signing_blocks_pending_local_edits_before_upload() {
    use crate::sync::Answer;
    use er_api::{EventDown, EventsPage};
    use er_domain::NoteEvent;
    let db = TestDb::new();
    let mut c = DeviceCore::open(db.config()).unwrap();
    c.start_shift(now()).unwrap();
    let p = patient(&mut c, "4");
    c.record_data_reviewed(p, "offline change".into(), now())
        .unwrap();
    c.bind_sync(er_store::sync::SyncBinding {
        origin: "https://example.invalid".into(),
        organization: Uuid::now_v7(),
        clinician: Uuid::now_v7(),
    })
    .unwrap();
    let segment = Uuid::now_v7();
    let events = [
        NoteEvent::audio_started(p, segment, er_domain::CaptureMode::Dictation),
        NoteEvent::audio_stopped(p, segment),
        NoteEvent::mdm_generated(p, "Synthetic MDM".into(), vec![segment]),
        NoteEvent::trigger_mdm(p),
        NoteEvent::review_ready(p),
        NoteEvent::sign(p),
    ];
    let down = events
        .into_iter()
        .enumerate()
        .map(|(i, event)| EventDown {
            sequence_id: i as u64 + 100,
            event_id: Uuid::now_v7(),
            occurred_at: event.clinical_occurred_at(),
            event,
        })
        .collect();
    c.accept_sync_answer(Answer::Page(
        p,
        0,
        EventsPage {
            events: down,
            has_more: false,
        },
    ))
    .unwrap();
    assert_eq!(c.sync_view().unwrap().conflicts, 1);
    assert!(c.note_view(p).unwrap().data_reviewed.is_empty());
    assert_eq!(
        c.record_data_reviewed(p, "must fail".into(), now()),
        Err(DeviceError::Conflict)
    );
}

#[test]
fn reopen_between_pages_can_finish_downloading_a_segment_boundary() {
    use crate::sync::Answer;
    use er_api::{EventDown, EventsPage};
    use er_domain::{CaptureMode, NoteEvent};
    let db = TestDb::new();
    let mut c = DeviceCore::open(db.config()).unwrap();
    c.start_shift(now()).unwrap();
    let p = patient(&mut c, "4");
    let binding = er_store::sync::SyncBinding {
        origin: "https://example.invalid".into(),
        organization: Uuid::now_v7(),
        clinician: Uuid::now_v7(),
    };
    c.bind_sync(binding.clone()).unwrap();
    let segment = Uuid::now_v7();
    let start = NoteEvent::audio_started(p, segment, CaptureMode::Dictation);
    c.accept_sync_answer(Answer::Page(
        p,
        0,
        EventsPage {
            events: vec![EventDown {
                sequence_id: 10,
                event_id: Uuid::now_v7(),
                occurred_at: start.clinical_occurred_at(),
                event: start,
            }],
            has_more: true,
        },
    ))
    .unwrap();
    drop(c);
    let mut c = DeviceCore::open(db.config()).unwrap();
    assert!(c.sync_can_connect(&binding).unwrap());
    let stop = NoteEvent::audio_stopped(p, segment);
    c.accept_sync_answer(Answer::Page(
        p,
        10,
        EventsPage {
            events: vec![EventDown {
                sequence_id: 50,
                event_id: Uuid::now_v7(),
                occurred_at: stop.clinical_occurred_at(),
                event: stop,
            }],
            has_more: false,
        },
    ))
    .unwrap();
    assert_eq!(c.note_view(p).unwrap().phase, ClinicalPhase::Idle);
    assert!(!c.note_view(p).unwrap().recovery_required);
    drop(c);
    DeviceCore::open(db.config()).unwrap();
}

#[test]
fn batches_respect_the_encoded_byte_limit_as_well_as_event_count() {
    let patient = Uuid::now_v7();
    let events = (0..200)
        .map(|n| er_store::StoredEvent {
            sequence_id: n + 1,
            event_id: Uuid::now_v7(),
            event: er_domain::NoteEvent::data_reviewed(patient, "\"".repeat(8192)),
            occurred_at: Utc::now(),
        })
        .collect();
    let batch = crate::sync::bounded_event_batch(events).unwrap();
    assert!(batch.len() < 200 && !batch.is_empty());
    assert!(
        serde_json::to_vec(&er_api::AppendEventsRequest { events: batch })
            .unwrap()
            .len()
            < 2 * 1024 * 1024
    );
}

#[test]
fn first_connection_authentication_failure_remains_visible_without_a_binding() {
    let db = TestDb::new();
    let c = DeviceCore::open(db.config()).unwrap();
    c.sync_failure("authentication", 0).unwrap();
    assert_eq!(c.sync_view().unwrap().status, "authentication");
    assert!(c.sync_view().unwrap().retry_at_ms.is_some());
}
