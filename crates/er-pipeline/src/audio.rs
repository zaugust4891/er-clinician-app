use crate::error::{PipelineError, PipelineResult};
use er_domain::{CaptureMode, EncounterId, PatientId};

/// Platform-agnostic audio capture interface.
///
/// Implementations:
/// - iOS: AVAudioEngine via Swift, buffers passed to Rust over FFI
/// - Desktop (testing): File-based mock or system mic via cpal
pub trait AudioCapture: Send + Sync {
    /// Start recording audio for a patient encounter.
    fn start(
        &mut self,
        patient_id: PatientId,
        encounter_id: EncounterId,
        mode: CaptureMode,
    ) -> PipelineResult<()>;

    /// Stop recording and return the raw PCM audio bytes.
    fn stop(&mut self) -> PipelineResult<Vec<u8>>;

    /// Whether audio is currently being captured.
    fn is_recording(&self) -> bool;

    /// Get the currently active patient, if recording.
    fn active_patient(&self) -> Option<PatientId>;

    /// Switch attribution to a different patient without stopping capture.
    /// Returns the audio chunk attributed to the previous patient.
    fn switch_patient(&mut self, new_patient_id: PatientId) -> PipelineResult<Option<Vec<u8>>>;
}

/// Mock audio capture for desktop testing — reads from a file or generates silence.
pub struct MockAudioCapture {
    recording: bool,
    active_patient: Option<PatientId>,
    encounter: Option<EncounterId>,
    mode: CaptureMode,
}

impl MockAudioCapture {
    pub fn new() -> Self {
        Self {
            recording: false,
            active_patient: None,
            encounter: None,
            mode: CaptureMode::Ambient,
        }
    }
}

impl Default for MockAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCapture for MockAudioCapture {
    fn start(
        &mut self,
        patient_id: PatientId,
        encounter_id: EncounterId,
        mode: CaptureMode,
    ) -> PipelineResult<()> {
        if self.recording {
            return Err(PipelineError::Audio(
                "audio capture is already recording.".to_string(),
            ));
        }

        self.recording = true;
        self.active_patient = Some(patient_id);
        self.encounter = Some(encounter_id);
        self.mode = mode;
        Ok(())
    }

    fn stop(&mut self) -> PipelineResult<Vec<u8>> {
        self.recording = false;
        self.active_patient = None;
        // Return empty audio for mock.
        Ok(Vec::new())
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn active_patient(&self) -> Option<PatientId> {
        self.active_patient
    }

    fn switch_patient(&mut self, new_patient_id: PatientId) -> PipelineResult<Option<Vec<u8>>> {
        let prev_audio = if self.recording {
            Some(Vec::new()) // Mock: empty chunk for previous patient
        } else {
            None
        };
        self.active_patient = Some(new_patient_id);
        Ok(prev_audio)
    }
}
