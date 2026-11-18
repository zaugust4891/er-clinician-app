use crate::{DeviceError as Error, Result, views::*};
use chrono::{DateTime, Utc};
use er_app::{EventBus, PatientNoteStore};
use er_domain::{CaptureMode, NoteEvent, NotePhase, PatientNote, PatientRecord};
use er_store::{Repository, SqliteRepository, device::*};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::Path,
    sync::Arc,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

#[derive(Clone)]
pub struct DeviceConfig {
    pub database_path: String,
    pub account_scope: String,
}

/// Rust-only seam. The production bridge exports no recording commands in Step 2.
pub trait CaptureAdapter: Send {
    fn available(&self) -> bool;
    fn start(&mut self, patient: Uuid, segment: Uuid, solo: bool) -> Result<()>;
    /// Must release capture before returning, including on an error.
    fn stop(&mut self) -> Result<()>;
}
struct UnavailableCapture;
impl CaptureAdapter for UnavailableCapture {
    fn available(&self) -> bool {
        false
    }
    fn start(&mut self, _: Uuid, _: Uuid, _: bool) -> Result<()> {
        Err(Error::InvalidState)
    }
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }
}

pub struct DeviceCore {
    repo: Arc<SqliteRepository>,
    bus: Arc<EventBus>,
    notes: PatientNoteStore,
    state: DeviceState,
    scope: String,
    runtime: Runtime,
    _ownership: File,
    capture: Box<dyn CaptureAdapter>,
    // Hardware is stopped before a possibly failing write. Retain the SAME event
    // ID/body for retry. On process death the persisted unfinished segment wins.
    pending: Option<(Uuid, NoteEvent, DeviceState)>,
    consent_stop_requested: HashSet<Uuid>,
}

pub fn text(value: &str, max: usize, empty: bool) -> Result<()> {
    if value.len() > max || (!empty && value.trim().is_empty()) || value.contains('\0') {
        Err(Error::InvalidInput)
    } else {
        Ok(())
    }
}
pub fn id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value)
        .ok()
        .filter(|u| !u.is_nil())
        .ok_or(Error::InvalidInput)
}
pub fn timestamp(ms: i64) -> Result<DateTime<Utc>> {
    // Device clock tolerance is 5s. No pre-epoch dates or backdated control taps.
    if ms < 0 || ms > Utc::now().timestamp_millis().saturating_add(5000) {
        return Err(Error::InvalidInput);
    }
    DateTime::from_timestamp_millis(ms).ok_or(Error::InvalidInput)
}

impl DeviceCore {
    pub fn open(config: DeviceConfig) -> Result<Self> {
        Self::open_inner(config, Box::new(UnavailableCapture))
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_with_capture(
        config: DeviceConfig,
        capture: Box<dyn CaptureAdapter>,
    ) -> Result<Self> {
        Self::open_inner(config, capture)
    }
    fn open_inner(config: DeviceConfig, capture: Box<dyn CaptureAdapter>) -> Result<Self> {
        text(&config.account_scope, 256, false)?;
        if config.account_scope.chars().any(char::is_control) {
            return Err(Error::InvalidInput);
        }
        text(&config.database_path, 4096, false)?;
        let path = Path::new(&config.database_path);
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(Error::InvalidInput);
        }
        // Canonicalizing the created DB prevents symlink/hardlink aliases from
        // bypassing the OS-held lock. The lock survives SQLite connection turnover.
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| Error::Storage)?;
        ownership.try_lock().map_err(|_| Error::Busy)?;
        let canonical = path.canonicalize().map_err(|_| Error::Storage)?;
        let repo = Arc::new(SqliteRepository::new(er_store::db::open_db(
            canonical.to_str().ok_or(Error::InvalidInput)?,
        )?));
        let state = repo.bind_device_scope(&config.account_scope)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Internal)?;
        let notes = er_app::new_store();
        let bus = Arc::new(EventBus::new(64, Some(repo.clone())));
        let mut core = Self {
            repo,
            bus,
            notes,
            state,
            scope: config.account_scope,
            runtime,
            _ownership: ownership,
            capture,
            pending: None,
            consent_stop_requested: HashSet::new(),
        };
        core.validate_and_load()?;
        Ok(core)
    }

    fn validate_and_load(&mut self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut open = 0;
        let mut unfinished_total = 0;
        for shift in &mut self.state.shifts {
            if !ids.insert(shift.id)
                || shift.started_at_ms < 0
                || shift.ended_at_ms.is_some_and(|t| t < shift.started_at_ms)
            {
                return Err(Error::Recovery);
            }
            if shift.ended_at_ms.is_none() {
                open += 1;
            }
            if shift
                .selected_patient_id
                .is_some_and(|p| !shift.visits.iter().any(|v| v.patient.id == p))
            {
                return Err(Error::Recovery);
            }
            for visit in &mut shift.visits {
                let p = visit.patient.id;
                if !ids.insert(p)
                    || visit
                        .consents
                        .iter()
                        .enumerate()
                        .any(|(n, c)| c.revision != n as u64 + 1 || c.actor_scope != self.scope)
                    || visit
                        .context
                        .iter()
                        .any(|c| c.patient_id != p || c.version == 0)
                {
                    return Err(Error::Recovery);
                }
                // Validate authoritative rows even when a valid snapshot exists.
                let events = self.runtime.block_on(self.repo.get_events(p))?;
                if events.iter().any(|e| e.event.extract_patient_id() != p) {
                    return Err(Error::Recovery);
                }
                let note = self
                    .runtime
                    .block_on(er_app::load_patient_note(self.repo.as_ref(), p))?
                    .unwrap_or_else(|| PatientNote::new(p));
                if note.patient_id != p {
                    return Err(Error::Recovery);
                }
                for segment in &visit.segments {
                    if segment.consent_revision > visit.consents.len() as u64
                        || segment
                            .stopped_at_ms
                            .is_some_and(|t| t < segment.started_at_ms)
                        || !note.encounters.iter().any(|e| {
                            e.id == segment.id
                                && e.capture_mode == segment.mode
                                && e.started_at.timestamp_millis() == segment.started_at_ms
                                && e.ended_at.map(|t| t.timestamp_millis()) == segment.stopped_at_ms
                        })
                    {
                        return Err(Error::Recovery);
                    }
                }
                let unfinished = note
                    .encounters
                    .iter()
                    .filter(|e| e.ended_at.is_none())
                    .count();
                unfinished_total += unfinished;
                if unfinished > 1 {
                    return Err(Error::Recovery);
                }
                if unfinished == 1 {
                    visit.recorder = RecorderStatus::Interrupted;
                    visit.pause_reason = Some("unfinished_segment".into());
                } else if matches!(
                    visit.recorder,
                    RecorderStatus::Recording
                        | RecorderStatus::PendingStop
                        | RecorderStatus::Interrupted
                ) {
                    return Err(Error::Recovery);
                }
                self.notes.insert(p, note);
            }
        }
        let mut versions = HashMap::new();
        for definition in &self.state.definitions {
            let previous = versions.entry(definition.id).or_insert(0u64);
            if definition.version != *previous + 1
                || definition.id.is_nil()
                || text(&definition.name, 128, false).is_err()
            {
                return Err(Error::Recovery);
            }
            *previous = definition.version;
        }
        if open > 1 || unfinished_total > 1 {
            return Err(Error::Recovery);
        }
        Ok(())
    }

    fn shift_index(&self) -> Result<usize> {
        self.state
            .shifts
            .iter()
            .position(|s| s.ended_at_ms.is_none())
            .ok_or(Error::InvalidState)
    }
    fn locate(&self, patient: Uuid) -> Result<(usize, usize)> {
        self.state
            .shifts
            .iter()
            .enumerate()
            .find_map(|(s, shift)| {
                shift
                    .visits
                    .iter()
                    .position(|v| v.patient.id == patient)
                    .map(|v| (s, v))
            })
            .ok_or(Error::NotFound)
    }
    fn visit(&self, patient: Uuid) -> Result<&Visit> {
        let (s, v) = self.locate(patient)?;
        Ok(&self.state.shifts[s].visits[v])
    }
    fn note(&self, patient: Uuid) -> Result<PatientNote> {
        self.locate(patient)?;
        self.notes
            .get(&patient)
            .map(|n| n.clone())
            .ok_or(Error::Recovery)
    }
    fn editable(&self, patient: Uuid) -> Result<()> {
        let (s, _) = self.locate(patient)?;
        if self.state.shifts[s].ended_at_ms.is_some() {
            return Err(Error::InvalidState);
        }
        if self.pending.is_some() {
            return Err(Error::Recovery);
        }
        er_app::validation::editable(&self.note(patient)?)?;
        Ok(())
    }
    fn commit(&mut self, mut staged: DeviceState, event: Option<(Uuid, NoteEvent)>) -> Result<()> {
        if staged.revision != self.state.revision {
            return Err(Error::Conflict);
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, _, _)| event.as_ref().is_none_or(|(id, _)| id != pending))
        {
            return Err(Error::Recovery);
        }
        staged.revision = self.state.revision.checked_add(1).ok_or(Error::Recovery)?;
        self.repo.commit_device_change(
            self.state.revision,
            &staged,
            event.as_ref().map(|(id, e)| (*id, e)),
        )?;
        // Nothing after commit is presented as a retryable storage failure.
        self.state = staged;
        if let Some((_, event)) = event {
            let p = event.extract_patient_id();
            let mut note = self.notes.get_mut(&p).expect("registered projection");
            er_domain::replay::apply_event(&mut note, &event);
        }
        Ok(())
    }
    fn publish(&mut self, patient: Uuid, event: NoteEvent) -> Result<()> {
        self.editable(patient)?;
        self.runtime
            .block_on(self.bus.publish(event, &self.notes))?;
        // Read-model revision is independent of note's domain version.
        Ok(())
    }
    fn control_time(&self, ms: i64) -> Result<DateTime<Utc>> {
        let t = timestamp(ms)?;
        if ms < self.state.last_control_at_ms {
            return Err(Error::InvalidInput);
        }
        if self
            .state
            .shifts
            .iter()
            .flat_map(|s| &s.visits)
            .filter_map(|v| v.boundary_at_ms)
            .max()
            .is_some_and(|last| ms < last)
        {
            return Err(Error::InvalidInput);
        }
        Ok(t)
    }

    pub fn start_shift(&mut self, at_ms: i64) -> Result<String> {
        timestamp(at_ms)?;
        if self.shift_index().is_ok() {
            return Err(Error::Conflict);
        }
        if self
            .state
            .shifts
            .last()
            .and_then(|s| s.ended_at_ms)
            .is_some_and(|end| at_ms < end)
        {
            return Err(Error::InvalidInput);
        }
        let shift = DeviceShift {
            id: Uuid::now_v7(),
            server_shift_id: None,
            started_at_ms: at_ms,
            ended_at_ms: None,
            selected_patient_id: None,
            visits: vec![],
        };
        let id = shift.id.to_string();
        let mut staged = self.state.clone();
        staged.shifts.push(shift);
        staged.last_control_at_ms = at_ms;
        self.commit(staged, None)?;
        Ok(id)
    }
    pub fn shifts(&self) -> Vec<ShiftView> {
        self.state.shifts.iter().map(shift_view).collect()
    }
    pub fn add_patient(
        &mut self,
        complaint: String,
        room: Option<String>,
        at_ms: i64,
    ) -> Result<String> {
        let at = timestamp(at_ms)?;
        text(&complaint, 1024, false)?;
        if let Some(r) = &room {
            text(r, 128, false)?;
        }
        let s = self.shift_index()?;
        if at_ms < self.state.shifts[s].started_at_ms {
            return Err(Error::InvalidInput);
        }
        let mut patient = PatientRecord::new(complaint);
        patient.room = room;
        patient.created_at = at;
        let id = patient.id;
        let mut staged = self.state.clone();
        staged.shifts[s].visits.push(Visit {
            patient,
            consents: vec![],
            mode: CaptureMode::Ambient,
            recorder: RecorderStatus::Unavailable,
            boundary_at_ms: None,
            pause_reason: None,
            segments: vec![],
            context: vec![],
            manual_draft: None,
        });
        self.commit(staged, None)?;
        self.notes.insert(id, PatientNote::new(id));
        Ok(id.to_string())
    }
    pub fn select_patient(&mut self, patient: Option<Uuid>, at_ms: i64) -> Result<()> {
        self.control_time(at_ms)?;
        let s = self.shift_index()?;
        if let Some(p) = patient
            && self.locate(p)?.0 != s
        {
            return Err(Error::NotFound);
        }
        if self.state.shifts[s].selected_patient_id == patient {
            return Ok(());
        }
        self.stop_if_active(at_ms, "patient_switch")?;
        let mut staged = self.state.clone();
        staged.shifts[s].selected_patient_id = patient;
        staged.last_control_at_ms = at_ms;
        self.commit(staged, None)
    }
    pub fn end_shift(&mut self, at_ms: i64) -> Result<()> {
        self.control_time(at_ms)?;
        let Ok(s) = self.shift_index() else {
            return Ok(());
        };
        if at_ms < self.state.shifts[s].started_at_ms {
            return Err(Error::InvalidInput);
        }
        self.stop_if_active(at_ms, "shift_ended")?;
        let mut staged = self.state.clone();
        staged.shifts[s].ended_at_ms = Some(at_ms);
        staged.last_control_at_ms = at_ms;
        staged.shifts[s].selected_patient_id = None;
        self.commit(staged, None)
    }
    pub fn set_mode(&mut self, patient: Uuid, solo: bool) -> Result<()> {
        self.editable(patient)?;
        let (s, v) = self.locate(patient)?;
        if self.active_patient().is_some() {
            return Err(Error::InvalidState);
        }
        let mut staged = self.state.clone();
        staged.shifts[s].visits[v].mode = if solo {
            CaptureMode::Dictation
        } else {
            CaptureMode::Ambient
        };
        self.commit(staged, None)
    }
    pub fn set_consent(
        &mut self,
        patient: Uuid,
        status: ConsentStatus,
        policy: String,
        expected: u64,
        at_ms: i64,
    ) -> Result<()> {
        self.locate(patient)?;
        er_app::validation::editable(&self.note(patient)?)?;
        timestamp(at_ms)?;
        text(&policy, 128, false)?;
        if self.state.shifts[self.locate(patient)?.0]
            .ended_at_ms
            .is_some()
        {
            return Err(Error::InvalidState);
        }
        let visit = self.visit(patient)?;
        if visit.consents.last().map_or(0, |c| c.revision) != expected {
            return Err(Error::Conflict);
        }
        if visit
            .consents
            .last()
            .is_some_and(|c| at_ms < c.recorded_at_ms)
        {
            return Err(Error::InvalidInput);
        }
        // Safety precedes storage: an ambient mic stops even if the consent save fails.
        if status != ConsentStatus::Granted && self.active_patient() == Some(patient) {
            self.consent_stop_requested.insert(patient);
            self.pause(patient, at_ms, "consent_changed".into())?;
        }
        let granted = status == ConsentStatus::Granted;
        let (s, v) = self.locate(patient)?;
        let mut staged = self.state.clone();
        staged.shifts[s].visits[v].consents.push(Consent {
            status,
            recorded_at_ms: at_ms,
            actor_scope: self.scope.clone(),
            policy_version: policy,
            revision: expected + 1,
        });
        self.commit(staged, None)?;
        if granted {
            self.consent_stop_requested.remove(&patient);
        }
        Ok(())
    }
    fn active_patient(&self) -> Option<Uuid> {
        self.state
            .shifts
            .iter()
            .flat_map(|s| &s.visits)
            .find(|v| {
                matches!(
                    v.recorder,
                    RecorderStatus::Recording
                        | RecorderStatus::Interrupted
                        | RecorderStatus::PendingStop
                )
            })
            .map(|v| v.patient.id)
    }
    pub fn start_capture(&mut self, patient: Uuid, at_ms: i64) -> Result<String> {
        let at = self.control_time(at_ms)?;
        self.editable(patient)?;
        let s = self.shift_index()?;
        if self.state.shifts[s].selected_patient_id != Some(patient)
            || self.active_patient().is_some()
            || !self.capture.available()
        {
            return Err(Error::InvalidState);
        }
        er_app::validation::capture_phase(&self.note(patient)?)?;
        let visit = self.visit(patient)?;
        let solo = visit.mode == CaptureMode::Dictation;
        if !solo
            && (self.consent_stop_requested.contains(&patient)
                || visit
                    .consents
                    .last()
                    .is_none_or(|c| c.status != ConsentStatus::Granted))
        {
            return Err(Error::InvalidState);
        }
        if at_ms < visit.patient.created_at.timestamp_millis() {
            return Err(Error::InvalidInput);
        }
        let consent_revision = visit.consents.last().map_or(0, |c| c.revision);
        let mode = visit.mode.clone();
        let segment = Uuid::now_v7();
        self.capture.start(patient, segment, solo)?;
        let (_, v) = self.locate(patient)?;
        let mut staged = self.state.clone();
        staged.last_control_at_ms = at_ms;
        let visit = &mut staged.shifts[s].visits[v];
        visit.recorder = RecorderStatus::Recording;
        visit.boundary_at_ms = Some(at_ms);
        visit.pause_reason = None;
        visit.segments.push(Segment {
            id: segment,
            mode: mode.clone(),
            source: "phone".into(),
            consent_revision,
            started_at_ms: at_ms,
            stopped_at_ms: None,
        });
        let event = NoteEvent::AudioStarted {
            patient_id: patient,
            encounter_id: segment,
            capture_mode: mode,
            clinical_occurred_at: at,
        };
        if let Err(e) = self.commit(staged, Some((Uuid::now_v7(), event))) {
            let _ = self.capture.stop();
            return Err(e);
        }
        Ok(segment.to_string())
    }
    pub fn pause(&mut self, patient: Uuid, at_ms: i64, reason: String) -> Result<()> {
        let at = self.control_time(at_ms)?;
        text(&reason, 128, false)?;
        if self.active_patient() != Some(patient) {
            return Err(Error::InvalidState);
        }
        if self.visit(patient)?.recorder == RecorderStatus::Interrupted {
            return Err(Error::Recovery);
        }
        if self.pending.is_none() {
            // This is a known boundary supplied by the adapter, not relaunch time.
            let stop_result = self.capture.stop();
            let (s, v) = self.locate(patient)?;
            let mut staged = self.state.clone();
            staged.last_control_at_ms = at_ms;
            let visit = &mut staged.shifts[s].visits[v];
            let segment = visit.segments.last_mut().ok_or(Error::Recovery)?;
            segment.stopped_at_ms = Some(at_ms);
            let event = NoteEvent::AudioStopped {
                patient_id: patient,
                encounter_id: segment.id,
                clinical_occurred_at: at,
            };
            visit.recorder = RecorderStatus::Paused;
            visit.boundary_at_ms = Some(at_ms);
            visit.pause_reason = Some(reason);
            self.pending = Some((Uuid::now_v7(), event, staged));
            self.state.shifts[s].visits[v].recorder = RecorderStatus::PendingStop;
            stop_result?;
        }
        let (id, event, staged) = self.pending.clone().ok_or(Error::Recovery)?;
        self.commit(staged, Some((id, event)))?;
        self.pending = None;
        Ok(())
    }
    /// Recovery requires an evidence-backed stop boundary, supplied by the Rust
    /// test harness now and the hardware manifest policy in Step 4.
    pub fn reconcile_interrupted(&mut self, patient: Uuid, at_ms: i64) -> Result<()> {
        timestamp(at_ms)?;
        if self.visit(patient)?.recorder != RecorderStatus::Interrupted {
            return Err(Error::InvalidState);
        }
        let (s, v) = self.locate(patient)?;
        let mut staged = self.state.clone();
        let visit = &mut staged.shifts[s].visits[v];
        let segment = visit.segments.last_mut().ok_or(Error::Recovery)?;
        if at_ms < segment.started_at_ms {
            return Err(Error::InvalidInput);
        }
        segment.stopped_at_ms = Some(at_ms);
        let encounter = segment.id;
        visit.recorder = RecorderStatus::Paused;
        visit.boundary_at_ms = Some(at_ms);
        visit.pause_reason = Some("reconciled_interruption".into());
        self.commit(
            staged,
            Some((
                Uuid::now_v7(),
                NoteEvent::AudioStopped {
                    patient_id: patient,
                    encounter_id: encounter,
                    clinical_occurred_at: timestamp(at_ms)?,
                },
            )),
        )
    }
    fn stop_if_active(&mut self, at_ms: i64, reason: &str) -> Result<()> {
        if let Some(p) = self.active_patient() {
            self.pause(p, at_ms, reason.into())?;
        }
        Ok(())
    }
    pub fn close(&mut self) -> Result<()> {
        self.stop_if_active(Utc::now().timestamp_millis(), "closed")
    }

    pub fn save_context(&mut self, input: ContextView, expected: u64) -> Result<String> {
        let p = id(&input.patient_id)?;
        self.editable(p)?;
        text(&input.text, 16384, false)?;
        text(&input.source, 256, false)?;
        timestamp(input.entered_at_ms)?;
        if let Some(t) = input.clinical_at_ms {
            timestamp(t)?;
            if t > input.entered_at_ms {
                return Err(Error::InvalidInput);
            }
        }
        let cid = if input.id.is_empty() {
            Uuid::now_v7()
        } else {
            id(&input.id)?
        };
        let (s, v) = self.locate(p)?;
        if self
            .state
            .shifts
            .iter()
            .flat_map(|s| &s.visits)
            .filter(|v| v.patient.id != p)
            .any(|v| v.context.iter().any(|c| c.id == cid))
        {
            return Err(Error::NotFound);
        }
        let mut staged = self.state.clone();
        let context = &mut staged.shifts[s].visits[v].context;
        let old = context.iter().position(|c| c.id == cid);
        if old.map_or(0, |n| context[n].version) != expected {
            return Err(Error::Conflict);
        }
        let item = ChartContext {
            id: cid,
            patient_id: p,
            version: expected + 1,
            category: input.category,
            text: input.text,
            source: input.source,
            clinical_at_ms: input.clinical_at_ms,
            entered_at_ms: input.entered_at_ms,
            reviewed: input.reviewed,
        };
        if let Some(n) = old {
            context[n] = item;
        } else {
            if context.len() >= 512 {
                return Err(Error::InvalidInput);
            }
            context.push(item);
        }
        self.commit(staged, None)?;
        Ok(cid.to_string())
    }
    pub fn contexts(&self, patient: Uuid) -> Result<Vec<ContextView>> {
        Ok(self
            .visit(patient)?
            .context
            .iter()
            .map(|c| ContextView {
                id: c.id.to_string(),
                patient_id: patient.to_string(),
                version: c.version,
                category: c.category.clone(),
                text: c.text.clone(),
                source: c.source.clone(),
                clinical_at_ms: c.clinical_at_ms,
                entered_at_ms: c.entered_at_ms,
                reviewed: c.reviewed,
            })
            .collect())
    }
    pub fn save_manual_draft(
        &mut self,
        patient: Uuid,
        draft: String,
        expected_revision: u64,
    ) -> Result<()> {
        self.editable(patient)?;
        text(&draft, 65536, true)?;
        if self.state.revision != expected_revision {
            return Err(Error::Conflict);
        }
        let (s, v) = self.locate(patient)?;
        let mut staged = self.state.clone();
        staged.shifts[s].visits[v].manual_draft = Some(draft);
        self.commit(staged, None)
    }

    pub fn save_definition(&mut self, input: DefinitionView, expected: u64) -> Result<String> {
        text(&input.name, 128, false)?;
        text(&input.instructions, 8192, true)?;
        if input.sections.len() > 5 {
            return Err(Error::InvalidInput);
        }
        let mut seen = HashSet::new();
        for section in &input.sections {
            if !["hpi", "ros", "exam", "mdm", "disposition"].contains(&section.section.as_str())
                || !seen.insert(&section.section)
            {
                return Err(Error::InvalidInput);
            }
            text(&section.instructions, 8192, true)?;
        }
        if input.kind == DefinitionKind::Template {
            if input.sections.is_empty() {
                return Err(Error::InvalidInput);
            }
        } else {
            text(&input.instructions, 8192, false)?;
            if !input.sections.is_empty() {
                return Err(Error::InvalidInput);
            }
        }
        let did = if input.id.is_empty() {
            Uuid::now_v7()
        } else {
            id(&input.id)?
        };
        let old = self.state.definitions.iter().rev().find(|d| d.id == did);
        if old.map_or(0, |d| d.version) != expected || old.is_some_and(|d| d.kind != input.kind) {
            return Err(Error::Conflict);
        }
        let mut staged = self.state.clone();
        // Defaults are selected by immutable new versions of other definitions too.
        if input.is_default && !input.archived {
            let latest = latest_definitions(&staged.definitions);
            for mut d in latest {
                if d.id != did && d.kind == input.kind && d.is_default {
                    d.version += 1;
                    d.is_default = false;
                    staged.definitions.push(d);
                }
            }
        }
        staged.definitions.push(Definition {
            id: did,
            version: expected + 1,
            name: input.name,
            kind: input.kind,
            instructions: input.instructions,
            sections: input
                .sections
                .into_iter()
                .map(|s| SectionPreference {
                    section: s.section,
                    instructions: s.instructions,
                })
                .collect(),
            archived: input.archived,
            is_default: input.is_default && !input.archived,
        });
        self.commit(staged, None)?;
        Ok(did.to_string())
    }
    pub fn definition_versions(&self, definition: Uuid) -> Vec<DefinitionView> {
        self.state
            .definitions
            .iter()
            .filter(|d| d.id == definition)
            .map(definition_view)
            .collect()
    }
    /// Duplicate/reset/default/archive all save an explicit version; old bodies remain intact.
    pub fn duplicate_definition(&mut self, definition: Uuid, name: String) -> Result<String> {
        let mut d = self
            .definition_versions(definition)
            .pop()
            .ok_or(Error::NotFound)?;
        d.id = String::new();
        d.name = name;
        d.is_default = false;
        d.archived = false;
        self.save_definition(d, 0)
    }
    pub fn restore_definition(
        &mut self,
        definition: Uuid,
        version: u64,
        expected: u64,
    ) -> Result<()> {
        let d = self
            .definition_versions(definition)
            .into_iter()
            .find(|d| d.version == version)
            .ok_or(Error::NotFound)?;
        self.save_definition(d, expected)?;
        Ok(())
    }

    pub fn record_result(
        &mut self,
        patient: Uuid,
        result: er_domain::ClinicalResult,
        at_ms: i64,
    ) -> Result<()> {
        if result.patient_id != patient || result.source != er_domain::ResultSource::Manual {
            return Err(Error::InvalidInput);
        }
        timestamp(result.timestamp.timestamp_millis())?;
        use er_domain::ResultType;
        match &result.result_type {
            ResultType::Lab(l) => {
                text(&l.display_name, 256, false)?;
                text(&l.unit, 64, false)?;
                if !l.value.is_finite()
                    || l.fhir_id.is_some()
                    || l.reference_range
                        .is_some_and(|(a, b)| !a.is_finite() || !b.is_finite() || a > b)
                {
                    return Err(Error::InvalidInput);
                }
            }
            ResultType::Imaging(i) => {
                text(&i.modality, 64, false)?;
                text(&i.body_part, 256, false)?;
                text(&i.impression, 16384, false)?;
            }
            ResultType::Vital(v) => {
                text(&v.unit, 64, false)?;
                if !v.value.is_finite() {
                    return Err(Error::InvalidInput);
                }
            }
        }
        self.publish(
            patient,
            NoteEvent::result_received(patient, result, timestamp(at_ms)?),
        )
    }
    pub fn record_medication(
        &mut self,
        patient: Uuid,
        medication: er_domain::Medication,
        at_ms: i64,
    ) -> Result<()> {
        if medication.patient_id != patient {
            return Err(Error::InvalidInput);
        }
        text(&medication.name, 256, false)?;
        text(&medication.dose, 256, true)?;
        text(&medication.frequency, 256, true)?;
        if let Some(r) = &medication.route {
            text(r, 128, false)?;
        }
        timestamp(medication.administered_at.timestamp_millis())?;
        self.publish(
            patient,
            NoteEvent::MedicationRecorded {
                patient_id: patient,
                medication,
                clinical_occurred_at: timestamp(at_ms)?,
            },
        )
    }
    pub fn record_diagnosis(
        &mut self,
        patient: Uuid,
        diagnosis: er_domain::Diagnosis,
        at_ms: i64,
    ) -> Result<()> {
        if diagnosis.patient_id != patient {
            return Err(Error::InvalidInput);
        }
        text(&diagnosis.text, 2048, false)?;
        if let Some(c) = &diagnosis.icd10_code {
            text(c, 64, false)?;
        }
        self.publish(
            patient,
            NoteEvent::DiagnosisAssigned {
                patient_id: patient,
                diagnosis,
                clinical_occurred_at: timestamp(at_ms)?,
            },
        )
    }
    pub fn record_data_reviewed(
        &mut self,
        patient: Uuid,
        review: String,
        at_ms: i64,
    ) -> Result<()> {
        text(&review, 8192, false)?;
        self.publish(
            patient,
            NoteEvent::DataReviewed {
                patient_id: patient,
                physician_review: review,
                clinical_occurred_at: timestamp(at_ms)?,
            },
        )
    }
    pub fn set_disposition(
        &mut self,
        patient: Uuid,
        disposition: er_domain::Disposition,
        at_ms: i64,
    ) -> Result<()> {
        use er_domain::Disposition::*;
        match &disposition {
            Discharge {
                instructions,
                follow_up,
            } => {
                text(instructions, 8192, false)?;
                if let Some(f) = follow_up {
                    text(f, 2048, false)?;
                }
            }
            Admit { service, bed } => {
                text(service, 256, false)?;
                if let Some(b) = bed {
                    text(b, 256, false)?;
                }
            }
            Transfer {
                destination,
                reason,
            } => {
                text(destination, 256, false)?;
                if let Some(r) = reason {
                    text(r, 2048, false)?;
                }
            }
            AMA | LWBS => {}
        }
        self.publish(
            patient,
            NoteEvent::DispositionSet {
                patient_id: patient,
                disposition,
                clinical_occurred_at: timestamp(at_ms)?,
            },
        )
    }
    pub fn review_ready(&mut self, patient: Uuid, at_ms: i64) -> Result<()> {
        let note = self.note(patient)?;
        if note.phase != NotePhase::Finalizing
            || note
                .note_sections
                .mdm
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
            || note
                .encounters
                .iter()
                .any(|e| e.ended_at.is_none() || e.transcript.is_none())
        {
            return Err(Error::InvalidState);
        }
        self.publish(
            patient,
            NoteEvent::ReviewReady {
                patient_id: patient,
                clinical_occurred_at: timestamp(at_ms)?,
            },
        )
    }
    pub fn sign(&mut self, patient: Uuid, at_ms: i64) -> Result<()> {
        if self.note(patient)?.phase != NotePhase::PendingSignOff {
            return Err(Error::InvalidState);
        }
        self.publish(
            patient,
            NoteEvent::Sign {
                patient_id: patient,
                clinical_occurred_at: timestamp(at_ms)?,
            },
        )
    }
    pub fn note_view(&self, patient: Uuid) -> Result<NoteView> {
        let n = self.note(patient)?;
        let v = self.visit(patient)?;
        Ok(NoteView {
            patient_id: patient.to_string(),
            phase: (&n.phase).into(),
            hpi: n.note_sections.hpi,
            ros: n.note_sections.ros,
            exam: n.note_sections.exam,
            mdm: n.note_sections.mdm,
            disposition_text: n.note_sections.disposition_text,
            objective_items: n
                .results
                .iter()
                .map(|r| ObjectiveView {
                    id: r.id.to_string(),
                    category: match r.result_type {
                        er_domain::ResultType::Lab(_) => ContextCategory::Lab,
                        er_domain::ResultType::Imaging(_) => ContextCategory::Imaging,
                        er_domain::ResultType::Vital(_) => ContextCategory::Vital,
                    },
                    summary: er_domain::note::format_result_line(r),
                    source: match r.source {
                        er_domain::ResultSource::Manual => "manual",
                        er_domain::ResultSource::FhirPull => "fhir_pull",
                        er_domain::ResultSource::Hl7Feed => "hl7_feed",
                    }
                    .into(),
                    clinical_at_ms: Some(r.timestamp.timestamp_millis()),
                })
                .chain(n.medication.iter().map(|m| ObjectiveView {
                    id: m.id.to_string(),
                    category: ContextCategory::Medication,
                    summary: m.summary_line(),
                    source: "unspecified".into(),
                    clinical_at_ms: Some(m.administered_at.timestamp_millis()),
                }))
                .chain(n.diagnoses.iter().map(|d| ObjectiveView {
                    id: d.id.to_string(),
                    category: ContextCategory::Diagnosis,
                    summary: d.summary_line(),
                    source: "unspecified".into(),
                    clinical_at_ms: None,
                }))
                .collect(),
            disposition: n
                .disposition
                .as_ref()
                .map(er_app::conversions::disposition::format_disposition_for_mdm),
            results: n
                .results
                .iter()
                .map(er_domain::note::format_result_line)
                .collect(),
            medications: n.medication.iter().map(|m| m.summary_line()).collect(),
            diagnoses: n.diagnoses.iter().map(|d| d.summary_line()).collect(),
            data_reviewed: n.data_reviewed,
            encounter_count: n.encounter_count,
            audit_count: n.audit_log.len() as u64,
            projection_version: n.version,
            recovery_required: matches!(
                v.recorder,
                RecorderStatus::Interrupted | RecorderStatus::PendingStop
            ),
            manual_draft: v.manual_draft.clone(),
        })
    }
    pub fn export_note(&self, patient: Uuid) -> Result<String> {
        let n = self.note(patient)?;
        let visit = self.visit(patient)?;
        let mut sections = n.note_sections.clone();
        if sections.hpi.is_none()
            && sections.ros.is_none()
            && sections.exam.is_none()
            && sections.mdm.is_none()
            && sections.disposition_text.is_none()
        {
            let fallback = n
                .encounters
                .iter()
                .filter_map(|e| {
                    e.transcript
                        .as_ref()
                        .map(|t| format!("Encounter {}: {}", e.encounter_number, t))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !fallback.is_empty() {
                sections.hpi = Some(fallback);
            }
        }
        let results = n
            .results
            .iter()
            .map(|r| format!("{:?}", r.result_type))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(er_app::export::format_note(
            &visit.patient,
            &sections,
            n.disposition.as_ref(),
            if results.is_empty() {
                None
            } else {
                Some(&results)
            },
            &n.medication,
            &n.diagnoses,
            &n.data_reviewed,
        ))
    }
    pub fn view(&self) -> DeviceView {
        let shift = self.state.shifts.iter().find(|s| s.ended_at_ms.is_none());
        let patients = shift
            .map(|s| {
                s.visits
                    .iter()
                    .enumerate()
                    .map(|(position, v)| {
                        let phase = self
                            .notes
                            .get(&v.patient.id)
                            .map(|n| ClinicalPhase::from(&n.phase))
                            .unwrap_or(ClinicalPhase::Registered);
                        let consent = v.consents.last();
                        let phase_ok = matches!(
                            phase,
                            ClinicalPhase::Registered
                                | ClinicalPhase::Idle
                                | ClinicalPhase::Finalizing
                        );
                        let eligible = phase_ok
                            && self.active_patient().is_none()
                            && self.capture.available()
                            && s.selected_patient_id == Some(v.patient.id);
                        PatientView {
                            id: v.patient.id.to_string(),
                            complaint: v.patient.chief_complaint.clone(),
                            room: v.patient.room.clone(),
                            phase,
                            position: position as u32,
                            ambient_eligible: eligible
                                && !self.consent_stop_requested.contains(&v.patient.id)
                                && consent.is_some_and(|c| c.status == ConsentStatus::Granted),
                            solo_eligible: eligible,
                            consent: ConsentView {
                                patient_id: v.patient.id.to_string(),
                                status: consent
                                    .map_or(ConsentStatus::Unknown, |c| c.status.clone()),
                                recorded_at_ms: consent.map(|c| c.recorded_at_ms),
                                policy_version: consent.map(|c| c.policy_version.clone()),
                                revision: consent.map_or(0, |c| c.revision),
                            },
                            recording: RecordingView {
                                patient_id: v.patient.id.to_string(),
                                solo: v.mode == CaptureMode::Dictation,
                                source: "phone".into(),
                                segment_id: v.segments.last().map(|s| s.id.to_string()),
                                status: v.recorder.clone(),
                                boundary_at_ms: v.boundary_at_ms,
                                reason: v.pause_reason.clone(),
                                hardware_available: self.capture.available(),
                            },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        DeviceView {
            revision: self.state.revision,
            shift: shift.map(shift_view),
            patients,
            definitions: latest_definitions(&self.state.definitions)
                .iter()
                .map(definition_view)
                .collect(),
        }
    }
}
impl Drop for DeviceCore {
    fn drop(&mut self) {
        let _ = self.capture.stop();
    }
}
fn shift_view(s: &DeviceShift) -> ShiftView {
    ShiftView {
        id: s.id.to_string(),
        started_at_ms: s.started_at_ms,
        ended_at_ms: s.ended_at_ms,
        selected_patient_id: s.selected_patient_id.map(|p| p.to_string()),
        is_open: s.ended_at_ms.is_none(),
    }
}
fn latest_definitions(definitions: &[Definition]) -> Vec<Definition> {
    let mut latest = HashMap::new();
    for d in definitions {
        latest.insert(d.id, d.clone());
    }
    let mut result: Vec<_> = latest.into_values().collect();
    result.sort_by_key(|d| d.id);
    result
}
fn definition_view(d: &Definition) -> DefinitionView {
    DefinitionView {
        id: d.id.to_string(),
        version: d.version,
        name: d.name.clone(),
        kind: d.kind.clone(),
        instructions: d.instructions.clone(),
        sections: d
            .sections
            .iter()
            .map(|s| SectionView {
                section: s.section.clone(),
                instructions: s.instructions.clone(),
            })
            .collect(),
        archived: d.archived,
        is_default: d.is_default,
    }
}
