use super::*;
use crate::sync::{Action, Answer, AudioJob, SyncView};
use er_api::{CaptureAuditV1, CaptureAuditView, ConsentAudit, SegmentAudit};
use er_store::audio::ChunkState;
use er_store::sync::{RemoteEvent, SyncBinding, SyncLedger};

impl DeviceCore {
    pub(crate) fn scheduled_sync_due(&self) -> Result<bool> {
        let l = self.repo.sync_ledger()?;
        let now = Utc::now().timestamp_millis();
        if l.discovery_at_ms <= now {
            return Ok(true);
        }
        for s in &self.state.shifts {
            if s.server_shift_id.is_none()
                || s.ended_at_ms.is_some() && !l.shifts.get(&s.id).is_some_and(|x| x.ended)
            {
                return Ok(true);
            }
            for v in &s.visits {
                let p = l.patients.get(&v.patient.id);
                let count = self.repo.delivery_counts(v.patient.id)?;
                if count.conflicts == 0
                    && (count.queued > 0
                        || count.awaiting_download > 0
                        || self.repo.audio_work_pending(v.patient.id)?
                        || p.is_none_or(|p| {
                            !p.registered
                                || !p.audit_loaded
                                || p.push_next
                                || p.next_poll_ms <= now
                                || p.audit_json.as_deref()
                                    != Some(&serde_json::to_string(&audit(v)).unwrap_or_default())
                        }))
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    pub(crate) fn request_sync_refresh(&self) -> Result<()> {
        let mut l = self.repo.sync_ledger()?;
        l.discovery_at_ms = 0;
        for p in l.patients.values_mut() {
            p.next_poll_ms = 0;
        }
        self.repo.save_sync_ledger(&l)?;
        Ok(())
    }
    pub(crate) fn sync_can_connect(&self, binding: &SyncBinding) -> Result<bool> {
        let l = self.repo.sync_ledger()?;
        if l.binding.as_ref().is_some_and(|b| b != binding) {
            return Err(Error::Scope);
        }
        if self.pending.is_some()
            || self.state.shifts.iter().flat_map(|s| &s.visits).any(|v| {
                matches!(
                    v.recorder,
                    RecorderStatus::Recording | RecorderStatus::PendingStop
                )
            })
        {
            return Err(Error::Busy);
        }
        Ok(Utc::now().timestamp_millis() >= l.retry_at_ms)
    }
    pub(crate) fn bind_sync(&mut self, binding: SyncBinding) -> Result<()> {
        let mut l = self.repo.sync_ledger()?;
        if l.binding.as_ref().is_some_and(|b| b != &binding) {
            return Err(Error::Scope);
        }
        l.binding = Some(binding);
        l.status = "pending".into();
        self.repo.save_sync_ledger(&l)?;
        self.rebuild_synchronized_notes()
    }
    pub(crate) fn reserve_request(&self) -> Result<bool> {
        let now = Utc::now().timestamp_millis();
        let mut l = self.repo.sync_ledger()?;
        l.requests_ms.retain(|t| *t > now - 60_000);
        if now < l.retry_at_ms {
            return Ok(false);
        }
        if l.requests_ms.len() >= 48 {
            l.status = "budget_wait".into();
            l.retry_at_ms = l.requests_ms[0] + 60_001;
            self.repo.save_sync_ledger(&l)?;
            return Ok(false);
        }
        l.requests_ms.push(now);
        self.repo.save_sync_ledger(&l)?;
        Ok(true)
    }
    pub(crate) fn sync_failure(&self, status: &str, minimum: i64) -> Result<()> {
        let mut l = self.repo.sync_ledger()?;
        l.failures = l.failures.saturating_add(1);
        l.status = status.into();
        let delay = if minimum > 0 {
            minimum.max((1000i64 << l.failures.min(6)).min(60_000))
        } else {
            (1000i64 << l.failures.min(6)).min(60_000)
        };
        l.retry_at_ms = Utc::now().timestamp_millis().saturating_add(delay);
        // An uncertain start always goes back through active-shift discovery.
        for s in l.shifts.values_mut() {
            s.create_ready = false;
        }
        if status == "conflict" {
            for p in l.patients.values_mut() {
                p.audit_loaded = false;
            }
        }
        self.repo.save_sync_ledger(&l)?;
        Ok(())
    }
    pub(crate) fn sync_view(&self) -> Result<SyncView> {
        let l = self.repo.sync_ledger()?;
        let mut queued = 0;
        let mut awaiting = 0;
        let mut conflicts = 0;
        let mut pending = false;
        for s in &self.state.shifts {
            pending |= s.server_shift_id.is_none()
                || (s.ended_at_ms.is_some() && !l.shifts.get(&s.id).is_some_and(|x| x.ended));
            for v in &s.visits {
                let counts = self.repo.delivery_counts(v.patient.id)?;
                queued += counts.queued;
                awaiting += counts.awaiting_download;
                conflicts += counts.conflicts;
                let p = l.patients.get(&v.patient.id);
                pending |= self.repo.audio_work_pending(v.patient.id)?;
                pending |= p.is_none_or(|p| {
                    !p.registered
                        || !p.audit_loaded
                        || p.audit_json.as_deref()
                            != Some(&serde_json::to_string(&audit(v)).unwrap_or_default())
                        || p.cursor == 0 && p.next_poll_ms == 0
                });
            }
        }
        let status = if conflicts > 0 {
            "conflict"
        } else if !l.status.is_empty() && !["pending", "up_to_date"].contains(&l.status.as_str()) {
            &l.status
        } else if l.binding.is_none() {
            "not_connected"
        } else if queued > 0 || awaiting > 0 || pending {
            "pending"
        } else {
            "up_to_date"
        };
        Ok(SyncView {
            status: status.into(),
            queued_events: queued,
            awaiting_download: awaiting,
            conflicts,
            retry_at_ms: (l.retry_at_ms > Utc::now().timestamp_millis()).then_some(l.retry_at_ms),
            local_only_preferences: true,
            local_only_context: true,
        })
    }
    pub(crate) fn next_sync_action(&mut self) -> Result<Option<Action>> {
        let now = Utc::now().timestamp_millis();
        let mut l = self.repo.sync_ledger()?;
        if now < l.retry_at_ms {
            return Ok(None);
        }
        if let Some(s) = self
            .state
            .shifts
            .iter()
            .find(|s| !l.shifts.get(&s.id).is_some_and(|x| x.ended))
        {
            if s.server_shift_id.is_none() {
                let control = l.shifts.entry(s.id).or_default();
                if control.create_ready {
                    control.create_ready = false;
                    self.repo.save_sync_ledger(&l)?;
                    return Ok(Some(Action::Start { local: s.id }));
                }
                return Ok(Some(Action::Discover { local: Some(s.id) }));
            }
            let remote = s.server_shift_id.unwrap();
            if now >= l.discovery_at_ms {
                return Ok(Some(Action::Roster {
                    local: s.id,
                    remote,
                }));
            }
            for v in &s.visits {
                if !l.patients.get(&v.patient.id).is_some_and(|p| p.registered) {
                    return Ok(Some(Action::Register {
                        patient: v.patient.id,
                        complaint: v.patient.chief_complaint.clone(),
                        room: v.patient.room.clone(),
                    }));
                }
            }
        } else if self.state.shifts.is_empty() && now >= l.discovery_at_ms {
            return Ok(Some(Action::Discover { local: None }));
        }
        let mut visits: Vec<_> = self.state.shifts.iter().flat_map(|s| &s.visits).collect();
        // A rotating starting point prevents the selected visit from starving the roster.
        if let Some(last) = l
            .last_patient
            .and_then(|id| visits.iter().position(|v| v.patient.id == id))
        {
            let n = visits.len();
            if n > 0 {
                visits.rotate_left((last + 1) % n);
            }
        }

        for v in visits {
            let id = v.patient.id;
            let p = l.patients.entry(id).or_default();
            if !p.registered {
                continue;
            }
            if self.repo.delivery_counts(id)?.conflicts > 0 {
                continue;
            }
            let action = if !p.history_loaded {
                Some(Action::Pull {
                    patient: id,
                    cursor: p.cursor,
                })
            } else if !p.audit_loaded {
                Some(Action::ReadAudit { patient: id })
            } else if p.push_next {
                let pending = self.repo.pending_events(id)?;
                if !pending.is_empty() {
                    Some(Action::Push {
                        patient: id,
                        events: crate::sync::bounded_event_batch(pending)?,
                    })
                } else if self.repo.delivery_counts(id)?.awaiting_download > 0 {
                    Some(Action::Pull {
                        patient: id,
                        cursor: p.cursor,
                    })
                } else if p.audit_json.as_deref()
                    != Some(&serde_json::to_string(&audit(v)).map_err(|_| Error::Internal)?)
                {
                    Some(Action::WriteAudit {
                        patient: id,
                        body: er_api::CaptureAuditWrite {
                            expected_version: p.audit_version,
                            audit: audit(v),
                        },
                    })
                } else if let Some(action) = self.next_audio_action(id)? {
                    // Reached only with this patient's event queue empty, so the
                    // segment's AudioStarted/AudioStopped are already on the
                    // server and its TranscriptReady can never precede them.
                    Some(action)
                } else {
                    p.push_next = false;
                    p.next_poll_ms = now
                        + if self
                            .state
                            .shifts
                            .iter()
                            .any(|s| s.selected_patient_id == Some(id))
                        {
                            5_000
                        } else {
                            60_000
                        };
                    None
                }
            } else if now >= p.next_poll_ms || self.repo.delivery_counts(id)?.queued > 0 {
                Some(Action::Pull {
                    patient: id,
                    cursor: p.cursor,
                })
            } else {
                None
            };
            if let Some(a) = action {
                l.last_patient = Some(id);
                self.repo.save_sync_ledger(&l)?;
                return Ok(Some(a));
            }
        }
        for s in &self.state.shifts {
            if s.ended_at_ms.is_some()
                && !l.shifts.get(&s.id).is_some_and(|x| x.ended)
                && s.visits.iter().all(|v| {
                    l.patients
                        .get(&v.patient.id)
                        .is_some_and(|p| p.registered && p.audit_loaded && p.next_poll_ms > 0)
                })
                && let Some(remote) = s.server_shift_id
            {
                self.repo.save_sync_ledger(&l)?;
                return Ok(Some(Action::End {
                    local: s.id,
                    remote,
                }));
            }
        }
        l.status = "up_to_date".into();
        self.repo.save_sync_ledger(&l)?;
        Ok(None)
    }
    pub(crate) fn accept_sync_answer(&mut self, answer: Answer) -> Result<()> {
        if self.pending.is_some()
            || self
                .state
                .shifts
                .iter()
                .flat_map(|s| &s.visits)
                .any(|v| v.recorder == RecorderStatus::Recording)
        {
            return Err(Error::Busy);
        }
        let mut l = self.repo.sync_ledger()?;
        let now = Utc::now().timestamp_millis();
        l.failures = 0;
        l.retry_at_ms = 0;
        l.status = "pending".into();
        match answer {
            Answer::Discovered(local, remote) => match remote {
                Some(remote) => {
                    let local = local.unwrap_or_else(Uuid::now_v7);
                    self.import_roster(local, remote, &mut l)?;
                }
                None => {
                    if let Some(local) = local {
                        l.shifts.entry(local).or_default().create_ready = true;
                    } else {
                        l.discovery_at_ms = now + 60_000;
                    }
                }
            },
            Answer::Started(local, remote) | Answer::Roster(local, remote) => {
                self.import_roster(local, remote, &mut l)?;
            }
            Answer::Registered(patient, response) => {
                if patient != response.patient_id {
                    return Err(Error::Recovery);
                }
                l.patients.entry(patient).or_default().registered = true;
            }
            Answer::Page(patient, cursor, page) => {
                if page
                    .events
                    .iter()
                    .any(|e| !same_server_instant(e.occurred_at, e.event.clinical_occurred_at()))
                {
                    return Err(Error::Recovery);
                }
                let rows: Vec<_> = page
                    .events
                    .into_iter()
                    .map(|e| RemoteEvent {
                        sequence: e.sequence_id,
                        id: e.event_id,
                        event: e.event,
                    })
                    .collect();
                self.repo
                    .receive_page(patient, cursor, &rows, page.has_more, l)?;
                self.rebuild_synchronized(&[patient])?;
                // A page may carry a segment's TranscriptReady.
                self.release_transcribed_audio()?;
                return Ok(());
            }
            Answer::AudioUploaded(chunk) => {
                self.repo.set_audio_state(
                    chunk,
                    &[ChunkState::Queued],
                    ChunkState::Uploaded,
                    None,
                )?;
            }
            Answer::AudioReupload(chunk) => {
                self.repo.set_audio_state(
                    chunk,
                    &[ChunkState::Uploaded],
                    ChunkState::Queued,
                    Some("object_missing_on_server"),
                )?;
            }
            Answer::AudioCompleted(job, view) => {
                let agrees = view.chunk_id == job.chunk_id
                    && view.encounter_id == job.segment
                    && view.chunk_index == job.index
                    && view.is_final == job.is_final;
                let (to, reason) = if agrees {
                    (ChunkState::Completed, None)
                } else {
                    (ChunkState::Rejected, Some("server_disagrees"))
                };
                self.repo
                    .set_audio_state(job.chunk_id, &[ChunkState::Uploaded], to, reason)?;
            }
            Answer::AudioRetry(chunk, reason) => {
                self.repo.note_audio_retry(chunk, reason)?;
                return self.sync_failure("upload_retrying", 2_000);
            }
            Answer::AudioRejected(chunk, reason) => {
                self.repo.set_audio_state(
                    chunk,
                    &[ChunkState::Queued, ChunkState::Uploaded],
                    ChunkState::Rejected,
                    Some(reason),
                )?;
            }
            Answer::Acknowledged(patient, sent, response) => {
                if response.results.is_empty() {
                    return self.sync_failure("retrying", 2_000);
                }
                l.patients.entry(patient).or_default().push_next = false;
                let acks: Vec<_> = response
                    .results
                    .into_iter()
                    .map(|a| (a.event_id, a.sequence_id))
                    .collect();
                self.repo.acknowledge_events(patient, &sent, &acks, &l)?;
                self.rebuild_synchronized(&[patient])?;
                return Ok(());
            }
            Answer::AuditRead(patient, response) => {
                self.import_audit(patient, &response, &mut l)?;
            }
            Answer::AuditSaved(patient, sent, response) => {
                if response.audit != sent
                    || Some(response.clinician_id) != l.binding.as_ref().map(|b| b.clinician)
                {
                    return Err(Error::Recovery);
                }
                let p = l.patients.entry(patient).or_default();
                p.audit_version = response.version;
                p.audit_json = Some(serde_json::to_string(&sent).map_err(|_| Error::Internal)?);
            }
            Answer::Ended(local, response) => {
                if response.ended_at.is_none()
                    || self
                        .state
                        .shifts
                        .iter()
                        .find(|s| s.id == local)
                        .and_then(|s| s.server_shift_id)
                        != Some(response.shift_id)
                {
                    return Err(Error::Recovery);
                }
                l.shifts.entry(local).or_default().ended = true;
            }
        }
        self.repo.save_sync_ledger(&l)?;
        Ok(())
    }
    fn import_roster(
        &mut self,
        local: Uuid,
        remote: er_api::ShiftView,
        l: &mut SyncLedger,
    ) -> Result<()> {
        if remote.shift_id.is_nil()
            || !matches!(remote.status.as_str(), "active" | "ended")
            || self
                .state
                .shifts
                .iter()
                .any(|s| s.id != local && s.server_shift_id == Some(remote.shift_id))
        {
            return Err(Error::Conflict);
        }
        let mut staged = self.state.clone();
        let index = if let Some(i) = staged.shifts.iter().position(|s| s.id == local) {
            i
        } else {
            if staged.shifts.iter().any(|s| s.ended_at_ms.is_none()) {
                return Err(Error::Conflict);
            }
            staged.shifts.push(DeviceShift {
                id: local,
                server_shift_id: None,
                started_at_ms: remote.started_at.timestamp_millis(),
                ended_at_ms: remote.ended_at.map(|t| t.timestamp_millis()),
                selected_patient_id: None,
                visits: vec![],
            });
            staged.shifts.len() - 1
        };
        if staged.shifts[index]
            .server_shift_id
            .is_some_and(|id| id != remote.shift_id)
        {
            return Err(Error::Conflict);
        }
        staged.shifts[index].server_shift_id = Some(remote.shift_id);
        if remote.ended_at.is_some() && staged.shifts[index].ended_at_ms.is_none() {
            staged.shifts[index].ended_at_ms = remote.ended_at.map(|t| t.timestamp_millis());
            staged.shifts[index].selected_patient_id = None;
            l.shifts.entry(local).or_default().ended = true;
        }
        let mut ordered = vec![];
        for p in remote.patients {
            if p.patient_id.is_nil()
                || ordered.contains(&p.patient_id)
                || staged.shifts.iter().enumerate().any(|(i, s)| {
                    i != index && s.visits.iter().any(|v| v.patient.id == p.patient_id)
                })
            {
                return Err(Error::Recovery);
            }
            if !staged.shifts[index]
                .visits
                .iter()
                .any(|v| v.patient.id == p.patient_id)
            {
                let mut patient = PatientRecord::new(p.chief_complaint);
                patient.id = p.patient_id;
                patient.room = p.display_label;
                patient.created_at = p.created_at;
                staged.shifts[index].visits.push(Visit {
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
            }
            ordered.push(p.patient_id);
            l.patients.entry(p.patient_id).or_default().registered = true;
        }
        staged.shifts[index].visits.sort_by_key(|v| {
            ordered
                .iter()
                .position(|id| *id == v.patient.id)
                .unwrap_or(usize::MAX)
        });
        self.commit(staged, None)?;
        for p in &self.state.shifts[index].visits {
            self.notes
                .entry(p.patient.id)
                .or_insert_with(|| PatientNote::new(p.patient.id));
        }
        l.discovery_at_ms = Utc::now().timestamp_millis() + 30_000;
        Ok(())
    }
    fn import_audit(
        &mut self,
        patient: Uuid,
        response: &CaptureAuditView,
        l: &mut SyncLedger,
    ) -> Result<()> {
        if Some(response.clinician_id) != l.binding.as_ref().map(|b| b.clinician)
            || !response.audit.valid(Utc::now())
        {
            return Err(Error::Recovery);
        }
        let note = self.note(patient)?;
        if response.audit.segments.iter().any(|s| {
            !note.encounters.iter().any(|e| {
                e.id == s.id
                    && e.started_at.timestamp_millis() == s.started_at_ms
                    && e.ended_at.map(|t| t.timestamp_millis()) == s.stopped_at_ms
            })
        }) {
            let p = l.patients.entry(patient).or_default();
            p.history_loaded = false;
            p.audit_loaded = false;
            return Ok(());
        }
        let current = audit(self.visit(patient)?);
        if !current.extends(&response.audit) && !response.audit.extends(&current) {
            l.status = "conflict".into();
            self.repo.save_sync_ledger(l)?;
            return Err(Error::Conflict);
        }
        if response.audit.extends(&current) {
            let (s, v) = self.locate(patient)?;
            let mut staged = self.state.clone();
            let visit = &mut staged.shifts[s].visits[v];
            visit.consents = response
                .audit
                .consents
                .iter()
                .map(|c| Consent {
                    status: match c.status {
                        er_api::ConsentChoice::Unknown => ConsentStatus::Unknown,
                        er_api::ConsentChoice::Granted => ConsentStatus::Granted,
                        er_api::ConsentChoice::Declined => ConsentStatus::Declined,
                        er_api::ConsentChoice::Revoked => ConsentStatus::Revoked,
                    },
                    recorded_at_ms: c.recorded_at_ms,
                    actor_scope: self.scope.clone(),
                    policy_version: c.policy_version.clone(),
                    revision: c.revision,
                })
                .collect();
            visit.segments = response
                .audit
                .segments
                .iter()
                .map(|s| Segment {
                    id: s.id,
                    mode: s.mode.clone(),
                    source: s.source.clone(),
                    consent_revision: s.consent_revision,
                    started_at_ms: s.started_at_ms,
                    stopped_at_ms: s.stopped_at_ms,
                })
                .collect();
            if let Some(s) = visit.segments.last() {
                visit.mode = s.mode.clone();
                visit.boundary_at_ms = s.stopped_at_ms.or(Some(s.started_at_ms));
                visit.recorder = if s.stopped_at_ms.is_some() {
                    RecorderStatus::Paused
                } else {
                    RecorderStatus::Interrupted
                };
            }
            self.commit(staged, None)?;
        }
        let p = l.patients.entry(patient).or_default();
        p.audit_loaded = true;
        p.audit_version = response.version;
        p.audit_json = Some(serde_json::to_string(&response.audit).map_err(|_| Error::Internal)?);
        Ok(())
    }

    /// Everyone, from scratch. For when the whole picture may have changed:
    /// opening the store, binding an account.
    pub(crate) fn rebuild_synchronized_notes(&mut self) -> Result<()> {
        self.checkpoints.clear();
        let all: Vec<Uuid> = self
            .state
            .shifts
            .iter()
            .flat_map(|shift| &shift.visits)
            .map(|visit| visit.patient.id)
            .collect();
        self.rebuild_synchronized(&all)
    }

    /// Only the visits whose stored history changed.
    pub(crate) fn rebuild_synchronized(&mut self, patients: &[Uuid]) -> Result<()> {
        let ledger = self.repo.sync_ledger()?;
        if ledger.binding.is_none() {
            return Ok(());
        }
        let mut staged: Option<DeviceState> = None; // cloned only if a boundary is adopted
        let mut recovered = false;
        for &patient in patients {
            let cursor = ledger.patients.get(&patient).map_or(0, |p| p.cursor);
            let note = self.replay_synchronized(patient, cursor)?;
            #[cfg(any(test, feature = "test-support"))]
            self.assert_matches_full_replay(patient, &note)?;
            if find_visit(&self.state, patient).is_some_and(|visit| needs_boundary(visit, &note)) {
                let s = staged.get_or_insert_with(|| self.state.clone());
                if let Some(v) = find_visit_mut(s, patient) {
                    recovered |= adopt_boundary(v, &note);
                }
            }
            self.notes.insert(patient, note);
        }
        if let (true, Some(s)) = (recovered, staged) {
            self.commit(s, None)?;
        }
        Ok(())
    }

    /// The patient's note: a checkpoint of the history already read back from
    /// the server, plus everything newer. Only rows past the checkpoint are
    /// read and applied, so a sync reply costs what it brought, not the whole
    /// history.
    ///
    /// The checkpoint covers positions at or below the download bookmark
    /// (`cursor`) and nothing else. Every event there is stored, because a page
    /// is saved together with its bookmark. No new event can land there either:
    /// a page must advance past the bookmark, and `acknowledge_events` refuses a
    /// receipt that points at or below it. A receipted event above the bookmark
    /// stays in the tail, because a later page can still bring a lower server
    /// position ahead of it.
    fn replay_synchronized(&mut self, patient: Uuid, cursor: u64) -> Result<PatientNote> {
        if self
            .checkpoints
            .get(&patient)
            .is_some_and(|cp| cp.cursor > cursor)
        {
            // The bookmark moved backward; what the checkpoint holds no longer matches.
            self.checkpoints.remove(&patient);
        }
        let cp = self
            .checkpoints
            .entry(patient)
            .or_insert_with(|| Checkpoint {
                cursor: 0,
                note: PatientNote::new(patient),
            });
        let rows = self.repo.ordered_delivery_after(patient, cp.cursor)?;
        // Rows come in replay order, so "read back, at or below the bookmark"
        // is true for a prefix and false after it: a binary search finds the end.
        let split = rows.partition_point(|row| row.server_sequence.is_some_and(|p| p <= cursor));
        let (confirmed, tail) = rows.split_at(split);
        for row in confirmed {
            er_domain::replay::apply_event(&mut cp.note, &row.stored.event);
        }
        cp.cursor = cursor;
        let mut note = copy_note(&cp.note)?;
        for row in tail {
            if row.pending && !valid_pending(&note, &row.stored.event) {
                self.repo
                    .mark_sync_conflict(row.stored.event_id, "remote_state_changed")?;
                continue;
            }
            er_domain::replay::apply_event(&mut note, &row.stored.event);
        }
        Ok(note)
    }

    /// Test builds only: the incremental note must equal a replay from zero,
    /// the simple version this optimization replaced.
    #[cfg(any(test, feature = "test-support"))]
    fn assert_matches_full_replay(&self, patient: Uuid, note: &PatientNote) -> Result<()> {
        let mut full = PatientNote::new(patient);
        // `new()` stamps a fresh ID and clock time; take the checkpoint's.
        full.id = note.id;
        full.created_at = note.created_at;
        full.updated_at = note.updated_at;
        for (row, pending) in self.repo.ordered_delivery(patient)? {
            assert!(
                !pending || valid_pending(&full, &row.event),
                "incremental rebuild left an invalid pending event unmarked"
            );
            er_domain::replay::apply_event(&mut full, &row.event);
        }
        let json = |n: &PatientNote| -> Result<serde_json::Value> {
            let mut value = serde_json::to_value(n).map_err(|_| Error::Internal)?;
            // Replay gives every audit entry a fresh random ID
            // (`AuditEntry::with_timestamp`), so no two replays agree on those.
            if let Some(entries) = value.get_mut("audit_log").and_then(|a| a.as_array_mut()) {
                for entry in entries.iter_mut().filter_map(|e| e.as_object_mut()) {
                    entry.remove("id");
                }
            }
            Ok(value)
        };
        assert_eq!(
            json(note)?,
            json(&full)?,
            "incremental rebuild diverged from a full replay"
        );
        Ok(())
    }

    /// One chunk's next network step, in index order within sealed segments.
    fn next_audio_action(&self, patient: Uuid) -> Result<Option<Action>> {
        let Some(chunk) = self.repo.next_audio_work(patient)? else {
            return Ok(None);
        };
        let job = AudioJob {
            chunk_id: chunk.chunk_id,
            segment: chunk.segment_id,
            index: chunk.chunk_index.ok_or(Error::Recovery)?,
            is_final: chunk.is_final,
            content_type: chunk.content_type,
            path: self.audio_dir.join(&chunk.file_name),
            byte_len: chunk.byte_len,
            sha256: chunk.sha256,
        };
        Ok(Some(match chunk.state {
            ChunkState::Queued => Action::UploadAudio {
                patient,
                chunk: job,
            },
            ChunkState::Uploaded => Action::CompleteAudio { chunk: job },
            _ => return Err(Error::Recovery),
        }))
    }
    pub fn sync_conflicts(&self, patient: Uuid) -> Result<Vec<(String, String)>> {
        self.locate(patient)?;
        Ok(self.repo.sync_conflicts(patient)?)
    }
    pub fn discard_sync_conflict(&mut self, patient: Uuid, event: Uuid) -> Result<()> {
        self.locate(patient)?;
        self.repo.discard_sync_conflict(patient, event)?;
        let mut l = self.repo.sync_ledger()?;
        l.status = "pending".into();
        self.repo.save_sync_ledger(&l)?;
        self.rebuild_synchronized(&[patient])
    }
}
fn audit(v: &Visit) -> CaptureAuditV1 {
    CaptureAuditV1 {
        consents: v
            .consents
            .iter()
            .map(|c| ConsentAudit {
                revision: c.revision,
                status: match c.status {
                    ConsentStatus::Unknown => er_api::ConsentChoice::Unknown,
                    ConsentStatus::Granted => er_api::ConsentChoice::Granted,
                    ConsentStatus::Declined => er_api::ConsentChoice::Declined,
                    ConsentStatus::Revoked => er_api::ConsentChoice::Revoked,
                },
                recorded_at_ms: c.recorded_at_ms,
                policy_version: c.policy_version.clone(),
            })
            .collect(),
        segments: v
            .segments
            .iter()
            .map(|s| SegmentAudit {
                id: s.id,
                mode: s.mode.clone(),
                source: s.source.clone(),
                consent_revision: s.consent_revision,
                started_at_ms: s.started_at_ms,
                stopped_at_ms: s.stopped_at_ms,
            })
            .collect(),
    }
}
/// The server's time column is Postgres `timestamptz`, which keeps microseconds
/// (sqlx truncates when binding), while the event body keeps whatever precision
/// its author used. Worker-authored events carry nanoseconds, so compare at the
/// column's precision and no coarser.
fn same_server_instant(column: DateTime<Utc>, event: DateTime<Utc>) -> bool {
    use chrono::SubsecRound;
    column == event || column == event.trunc_subsecs(6)
}
/// A patient's note with every event at or below `cursor` applied. In memory
/// only: reopening starts from zero.
pub(super) struct Checkpoint {
    cursor: u64,
    note: PatientNote,
}

/// `PatientNote` isn't `Clone` (its state object is a trait object), so copy
/// it the way snapshots round-trip, then restore the state object.
fn copy_note(note: &PatientNote) -> Result<PatientNote> {
    let value = serde_json::to_value(note).map_err(|_| Error::Internal)?;
    let mut copy: PatientNote = serde_json::from_value(value).map_err(|_| Error::Internal)?;
    copy.hydrate(); // `state` is #[serde(skip)]; without it transitions fail silently
    Ok(copy)
}

fn find_visit(state: &DeviceState, patient: Uuid) -> Option<&Visit> {
    state
        .shifts
        .iter()
        .flat_map(|shift| &shift.visits)
        .find(|visit| visit.patient.id == patient)
}

fn find_visit_mut(state: &mut DeviceState, patient: Uuid) -> Option<&mut Visit> {
    state
        .shifts
        .iter_mut()
        .flat_map(|shift| &mut shift.visits)
        .find(|visit| visit.patient.id == patient)
}

/// The phone thinks it is still recording (or was cut off) but the server
/// history has every segment closed.
fn needs_boundary(visit: &Visit, note: &PatientNote) -> bool {
    note.encounters.iter().all(|e| e.ended_at.is_some())
        && matches!(
            visit.recorder,
            RecorderStatus::Interrupted | RecorderStatus::Recording
        )
}

/// Adopt the server's confirmed end for the last segment. Returns whether the
/// visit changed.
fn adopt_boundary(visit: &mut Visit, note: &PatientNote) -> bool {
    if let Some(segment) = visit.segments.last_mut() {
        let Some(end) = note
            .encounters
            .iter()
            .find(|e| e.id == segment.id)
            .and_then(|e| e.ended_at)
        else {
            return false;
        };
        segment.stopped_at_ms = Some(end.timestamp_millis());
        visit.boundary_at_ms = segment.stopped_at_ms;
        visit.recorder = RecorderStatus::Paused;
        visit.pause_reason = Some("confirmed_server_boundary".into());
    } else {
        visit.recorder = RecorderStatus::Unavailable;
        visit.pause_reason = None;
    }
    true
}

fn valid_pending(note: &PatientNote, event: &NoteEvent) -> bool {
    if note.phase == NotePhase::Signed {
        return false;
    }
    match event {
        NoteEvent::AudioStarted { .. } => {
            matches!(
                note.phase,
                NotePhase::Registered | NotePhase::Idle | NotePhase::Finalizing
            ) && note.encounters.iter().all(|e| e.ended_at.is_some())
        }
        NoteEvent::AudioStopped { encounter_id, .. } => {
            note.phase == NotePhase::Encountering
                && note
                    .encounters
                    .iter()
                    .any(|e| e.id == *encounter_id && e.ended_at.is_none())
        }
        NoteEvent::ReviewReady { .. } => note.phase == NotePhase::Finalizing,
        NoteEvent::Sign { .. } => note.phase == NotePhase::PendingSignOff,
        NoteEvent::TranscriptReady { .. }
        | NoteEvent::EncounterSummarized { .. }
        | NoteEvent::MdmGenerated { .. }
        | NoteEvent::TriggerMDM { .. } => false,
        _ => true,
    }
}

#[cfg(test)]
impl DeviceCore {
    pub(crate) fn pending_for_test(&self, patient: Uuid) -> Vec<er_store::StoredEvent> {
        self.repo.pending_events(patient).unwrap()
    }
    pub(crate) fn checkpoint_cursor_for_test(&self, patient: Uuid) -> Option<u64> {
        self.checkpoints.get(&patient).map(|cp| cp.cursor)
    }
}

#[cfg(test)]
mod precision_tests {
    use super::same_server_instant;
    use chrono::{DateTime, Utc};

    #[test]
    fn server_time_matches_at_microsecond_precision_only() {
        let event: DateTime<Utc> = "2026-11-18T21:35:50.596424934Z".parse().unwrap();
        let column: DateTime<Utc> = "2026-11-18T21:35:50.596424Z".parse().unwrap();
        assert!(same_server_instant(column, event));
        assert!(same_server_instant(event, event));
        let other: DateTime<Utc> = "2026-11-18T21:35:50.596425Z".parse().unwrap();
        assert!(!same_server_instant(other, event));
        let milli: DateTime<Utc> = "2026-11-18T21:35:50.596Z".parse().unwrap();
        assert!(!same_server_instant(milli, event));
    }
}
