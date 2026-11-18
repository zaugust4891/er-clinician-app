//! Explicit phone DTOs and conversions; raw domain events stay private.
use super::{DeviceConfig, FfiError, ffi_guard};
use er_device::{DeviceError, DeviceHandle};
use std::{panic::AssertUnwindSafe, sync::Arc};
use uuid::Uuid;

#[derive(Clone, Debug, uniffi::Enum)]
pub enum MedicationKindDto {
    Home,
    AdministeredInEd,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct MedicationInput {
    pub name: String,
    pub dose: String,
    pub route: Option<String>,
    pub kind: MedicationKindDto,
    pub frequency: String,
    pub administered_at_ms: i64,
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum DispositionInput {
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
    Ama,
    Lwbs,
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum VitalKind {
    HeartRate,
    Systolic,
    Diastolic,
    RespiratoryRate,
    Temperature,
    SpO2,
    Pain,
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum ObjectiveInput {
    Lab {
        name: String,
        value: f64,
        unit: String,
        abnormal: bool,
    },
    Imaging {
        modality: String,
        body_part: String,
        impression: String,
        critical: bool,
    },
    Vital {
        kind: VitalKind,
        value: f64,
        unit: String,
    },
}

#[uniffi::export(callback_interface)]
pub trait DeviceChangeListener: Send + Sync {
    fn invalidated(&self, revision: u64);
}
struct ListenerAdapter(Box<dyn DeviceChangeListener>);
impl er_device::DeviceListener for ListenerAdapter {
    fn invalidated(&self, revision: u64) {
        self.0.invalidated(revision);
    }
}

#[derive(uniffi::Object)]
pub struct ErDevice {
    handle: DeviceHandle,
}
impl ErDevice {
    fn call<T: Send + 'static>(
        &self,
        mutation: bool,
        action: impl FnOnce(&mut er_device::DeviceCore) -> er_device::Result<T> + Send + 'static,
    ) -> Result<T, FfiError> {
        ffi_guard(AssertUnwindSafe(|| {
            self.handle.execute(mutation, action).map_err(Into::into)
        }))
    }
}
#[uniffi::export]
impl ErDevice {
    #[uniffi::constructor]
    pub fn new(config: DeviceConfig) -> Result<Arc<Self>, FfiError> {
        ffi_guard(|| {
            let config = super::validate_config(config)?;
            Ok(Arc::new(Self {
                handle: DeviceHandle::open(er_device::DeviceConfig {
                    database_path: config.database_path,
                    account_scope: config.account_scope,
                })?,
            }))
        })
    }
    pub fn close(&self) -> Result<(), FfiError> {
        ffi_guard(AssertUnwindSafe(|| self.handle.close().map_err(Into::into)))
    }
    pub fn snapshot(&self) -> Result<DeviceSnapshotDto, FfiError> {
        self.call(false, |c| Ok(c.view().into()))
    }
    pub fn shifts(&self) -> Result<Vec<ShiftDto>, FfiError> {
        self.call(false, |c| {
            Ok(c.shifts().into_iter().map(Into::into).collect())
        })
    }
    pub fn start_shift(&self, at_ms: i64) -> Result<String, FfiError> {
        self.call(true, move |c| c.start_shift(at_ms))
    }
    pub fn end_shift(&self, at_ms: i64) -> Result<(), FfiError> {
        self.call(true, move |c| c.end_shift(at_ms))
    }
    pub fn add_patient(
        &self,
        complaint: String,
        room: Option<String>,
        at_ms: i64,
    ) -> Result<String, FfiError> {
        self.call(true, move |c| c.add_patient(complaint, room, at_ms))
    }
    pub fn select_patient(&self, patient_id: Option<String>, at_ms: i64) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.select_patient(
                patient_id.as_deref().map(er_device::core::id).transpose()?,
                at_ms,
            )
        })
    }
    pub fn set_consent(
        &self,
        patient_id: String,
        status: ConsentChoice,
        policy_version: String,
        expected_revision: u64,
        at_ms: i64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.set_consent(
                er_device::core::id(&patient_id)?,
                status.into(),
                policy_version,
                expected_revision,
                at_ms,
            )
        })
    }
    pub fn set_mode(&self, patient_id: String, solo: bool) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.set_mode(er_device::core::id(&patient_id)?, solo)
        })
    }
    pub fn note(&self, patient_id: String) -> Result<NoteDto, FfiError> {
        self.call(false, move |c| {
            Ok(c.note_view(er_device::core::id(&patient_id)?)?.into())
        })
    }
    pub fn chart_context(&self, patient_id: String) -> Result<Vec<ChartContextDto>, FfiError> {
        self.call(false, move |c| {
            Ok(c.contexts(er_device::core::id(&patient_id)?)?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }
    pub fn save_context(
        &self,
        input: ChartContextDto,
        expected_version: u64,
    ) -> Result<String, FfiError> {
        self.call(true, move |c| {
            c.save_context(input.into(), expected_version)
        })
    }
    pub fn save_manual_draft(
        &self,
        patient_id: String,
        text: String,
        expected_revision: u64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.save_manual_draft(er_device::core::id(&patient_id)?, text, expected_revision)
        })
    }
    pub fn save_definition(
        &self,
        input: DefinitionDto,
        expected_version: u64,
    ) -> Result<String, FfiError> {
        self.call(true, move |c| {
            c.save_definition(input.into(), expected_version)
        })
    }
    pub fn definition_versions(
        &self,
        definition_id: String,
    ) -> Result<Vec<DefinitionDto>, FfiError> {
        self.call(false, move |c| {
            Ok(c.definition_versions(er_device::core::id(&definition_id)?)
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }
    pub fn duplicate_definition(
        &self,
        definition_id: String,
        name: String,
    ) -> Result<String, FfiError> {
        self.call(true, move |c| {
            c.duplicate_definition(er_device::core::id(&definition_id)?, name)
        })
    }
    pub fn restore_definition(
        &self,
        definition_id: String,
        version: u64,
        expected_version: u64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.restore_definition(
                er_device::core::id(&definition_id)?,
                version,
                expected_version,
            )
        })
    }
    pub fn record_objective(
        &self,
        patient_id: String,
        input: ObjectiveInput,
        at_ms: i64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            use er_domain::result::{ImagingResult, LabResult, ResultType, VitalSign, VitalType};
            let p = er_device::core::id(&patient_id)?;
            let result_type = match input {
                ObjectiveInput::Lab {
                    name,
                    value,
                    unit,
                    abnormal,
                } => ResultType::Lab(LabResult {
                    fhir_id: None,
                    loinc_code: None,
                    display_name: name,
                    value,
                    unit,
                    reference_range: None,
                    is_abnormal: abnormal,
                }),
                ObjectiveInput::Imaging {
                    modality,
                    body_part,
                    impression,
                    critical,
                } => ResultType::Imaging(ImagingResult {
                    modality,
                    body_part,
                    impression,
                    is_critical: critical,
                }),
                ObjectiveInput::Vital { kind, value, unit } => ResultType::Vital(VitalSign {
                    vital_type: match kind {
                        VitalKind::HeartRate => VitalType::HeartRate,
                        VitalKind::Systolic => VitalType::BloodPressureSystolic,
                        VitalKind::Diastolic => VitalType::BloodPressureDiastolic,
                        VitalKind::RespiratoryRate => VitalType::RespiratoryRate,
                        VitalKind::Temperature => VitalType::Temperature,
                        VitalKind::SpO2 => VitalType::SpO2,
                        VitalKind::Pain => VitalType::Pain,
                    },
                    value,
                    unit,
                }),
            };
            c.record_result(
                p,
                er_domain::ClinicalResult {
                    id: Uuid::now_v7(),
                    patient_id: p,
                    source: er_domain::ResultSource::Manual,
                    timestamp: er_device::core::timestamp(at_ms)?,
                    result_type,
                },
                at_ms,
            )
        })
    }
    pub fn record_medication(
        &self,
        patient_id: String,
        input: MedicationInput,
        at_ms: i64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            let p = er_device::core::id(&patient_id)?;
            c.record_medication(
                p,
                er_domain::Medication {
                    id: Uuid::now_v7(),
                    patient_id: p,
                    name: input.name,
                    dose: input.dose,
                    route: input.route,
                    rxnorm_code: None,
                    kind: match input.kind {
                        MedicationKindDto::Home => er_domain::MedicationKind::Home,
                        MedicationKindDto::AdministeredInEd => {
                            er_domain::MedicationKind::AdministeredInEd
                        }
                    },
                    administered_at: er_device::core::timestamp(input.administered_at_ms)?,
                    frequency: input.frequency,
                },
                at_ms,
            )
        })
    }
    pub fn record_diagnosis(
        &self,
        patient_id: String,
        text: String,
        icd10_code: Option<String>,
        primary: bool,
        at_ms: i64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            let p = er_device::core::id(&patient_id)?;
            c.record_diagnosis(
                p,
                er_domain::Diagnosis {
                    id: Uuid::now_v7(),
                    patient_id: p,
                    text,
                    icd10_code,
                    is_primary: primary,
                },
                at_ms,
            )
        })
    }
    pub fn record_data_reviewed(
        &self,
        patient_id: String,
        text: String,
        at_ms: i64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.record_data_reviewed(er_device::core::id(&patient_id)?, text, at_ms)
        })
    }
    pub fn set_disposition(
        &self,
        patient_id: String,
        input: DispositionInput,
        at_ms: i64,
    ) -> Result<(), FfiError> {
        self.call(true, move |c| {
            use er_domain::Disposition;
            let disposition = match input {
                DispositionInput::Discharge {
                    instructions,
                    follow_up,
                } => Disposition::Discharge {
                    instructions,
                    follow_up,
                },
                DispositionInput::Admit { service, bed } => Disposition::Admit { service, bed },
                DispositionInput::Transfer {
                    destination,
                    reason,
                } => Disposition::Transfer {
                    destination,
                    reason,
                },
                DispositionInput::Ama => Disposition::AMA,
                DispositionInput::Lwbs => Disposition::LWBS,
            };
            c.set_disposition(er_device::core::id(&patient_id)?, disposition, at_ms)
        })
    }
    pub fn review_ready(&self, patient_id: String, at_ms: i64) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.review_ready(er_device::core::id(&patient_id)?, at_ms)
        })
    }
    pub fn sign(&self, patient_id: String, at_ms: i64) -> Result<(), FfiError> {
        self.call(true, move |c| {
            c.sign(er_device::core::id(&patient_id)?, at_ms)
        })
    }
    pub fn export_note(&self, patient_id: String) -> Result<String, FfiError> {
        self.call(false, move |c| {
            c.export_note(er_device::core::id(&patient_id)?)
        })
    }
    pub fn set_listener(
        &self,
        listener: Option<Box<dyn DeviceChangeListener>>,
    ) -> Result<(), FfiError> {
        ffi_guard(AssertUnwindSafe(|| {
            self.handle
                .set_listener(
                    listener.map(|l| {
                        Arc::new(ListenerAdapter(l)) as Arc<dyn er_device::DeviceListener>
                    }),
                )
                .map_err(Into::into)
        }))
    }
}
#[cfg(feature = "ffi-test")]
#[uniffi::export]
impl ErDevice {
    pub fn panic_device_for_test(&self) -> Result<(), FfiError> {
        self.call(true, |_| panic!("ER_FFI_PRIVATE_SENTINEL"))
    }
}

#[uniffi::export]
pub fn device_interface_version() -> Result<u32, FfiError> {
    ffi_guard(|| Ok(2))
}

impl From<DeviceError> for FfiError {
    fn from(e: DeviceError) -> Self {
        match e {
            DeviceError::InvalidInput => Self::InvalidInput,
            DeviceError::InvalidState => Self::InvalidState,
            DeviceError::NotFound => Self::NotFound,
            DeviceError::Conflict => Self::Conflict,
            DeviceError::Scope => Self::Scope,
            DeviceError::Storage => Self::Storage,
            DeviceError::Recovery => Self::Recovery,
            DeviceError::Closed => Self::Closed,
            DeviceError::Busy => Self::Busy,
            DeviceError::Internal => Self::Internal,
        }
    }
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum ConsentChoice {
    Unknown,
    Granted,
    Declined,
    Revoked,
}
impl From<er_device::ConsentStatus> for ConsentChoice {
    fn from(v: er_device::ConsentStatus) -> Self {
        match v {
            er_device::ConsentStatus::Unknown => Self::Unknown,
            er_device::ConsentStatus::Granted => Self::Granted,
            er_device::ConsentStatus::Declined => Self::Declined,
            er_device::ConsentStatus::Revoked => Self::Revoked,
        }
    }
}
impl From<ConsentChoice> for er_device::ConsentStatus {
    fn from(v: ConsentChoice) -> Self {
        match v {
            ConsentChoice::Unknown => Self::Unknown,
            ConsentChoice::Granted => Self::Granted,
            ConsentChoice::Declined => Self::Declined,
            ConsentChoice::Revoked => Self::Revoked,
        }
    }
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum ChartCategory {
    History,
    Medication,
    Vital,
    Lab,
    Imaging,
    Treatment,
    Diagnosis,
}
impl From<er_device::ContextCategory> for ChartCategory {
    fn from(v: er_device::ContextCategory) -> Self {
        match v {
            er_device::ContextCategory::History => Self::History,
            er_device::ContextCategory::Medication => Self::Medication,
            er_device::ContextCategory::Vital => Self::Vital,
            er_device::ContextCategory::Lab => Self::Lab,
            er_device::ContextCategory::Imaging => Self::Imaging,
            er_device::ContextCategory::Treatment => Self::Treatment,
            er_device::ContextCategory::Diagnosis => Self::Diagnosis,
        }
    }
}
impl From<ChartCategory> for er_device::ContextCategory {
    fn from(v: ChartCategory) -> Self {
        match v {
            ChartCategory::History => Self::History,
            ChartCategory::Medication => Self::Medication,
            ChartCategory::Vital => Self::Vital,
            ChartCategory::Lab => Self::Lab,
            ChartCategory::Imaging => Self::Imaging,
            ChartCategory::Treatment => Self::Treatment,
            ChartCategory::Diagnosis => Self::Diagnosis,
        }
    }
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum DefinitionKindDto {
    Template,
    AdmissionMessage,
    DischargeInstructions,
    Custom,
}
impl From<er_device::DefinitionKind> for DefinitionKindDto {
    fn from(v: er_device::DefinitionKind) -> Self {
        match v {
            er_device::DefinitionKind::Template => Self::Template,
            er_device::DefinitionKind::AdmissionMessage => Self::AdmissionMessage,
            er_device::DefinitionKind::DischargeInstructions => Self::DischargeInstructions,
            er_device::DefinitionKind::Custom => Self::Custom,
        }
    }
}
impl From<DefinitionKindDto> for er_device::DefinitionKind {
    fn from(v: DefinitionKindDto) -> Self {
        match v {
            DefinitionKindDto::Template => Self::Template,
            DefinitionKindDto::AdmissionMessage => Self::AdmissionMessage,
            DefinitionKindDto::DischargeInstructions => Self::DischargeInstructions,
            DefinitionKindDto::Custom => Self::Custom,
        }
    }
}
#[derive(Clone, Debug, uniffi::Enum)]
pub enum RecorderPhase {
    Unavailable,
    Ready,
    Recording,
    Paused,
    Interrupted,
    PendingStop,
}
impl From<er_device::RecorderStatus> for RecorderPhase {
    fn from(v: er_device::RecorderStatus) -> Self {
        match v {
            er_device::RecorderStatus::Unavailable => Self::Unavailable,
            er_device::RecorderStatus::Ready => Self::Ready,
            er_device::RecorderStatus::Recording => Self::Recording,
            er_device::RecorderStatus::Paused => Self::Paused,
            er_device::RecorderStatus::Interrupted => Self::Interrupted,
            er_device::RecorderStatus::PendingStop => Self::PendingStop,
        }
    }
}
impl From<RecorderPhase> for er_device::RecorderStatus {
    fn from(v: RecorderPhase) -> Self {
        match v {
            RecorderPhase::Unavailable => Self::Unavailable,
            RecorderPhase::Ready => Self::Ready,
            RecorderPhase::Recording => Self::Recording,
            RecorderPhase::Paused => Self::Paused,
            RecorderPhase::Interrupted => Self::Interrupted,
            RecorderPhase::PendingStop => Self::PendingStop,
        }
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ShiftDto {
    pub id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub selected_patient_id: Option<String>,
    pub is_open: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ConsentDto {
    pub patient_id: String,
    pub status: ConsentChoice,
    pub recorded_at_ms: Option<i64>,
    pub policy_version: Option<String>,
    pub revision: u64,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct RecordingStateDto {
    pub patient_id: String,
    pub solo: bool,
    pub source: String,
    pub segment_id: Option<String>,
    pub status: RecorderPhase,
    pub boundary_at_ms: Option<i64>,
    pub reason: Option<String>,
    pub hardware_available: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct PatientDto {
    pub id: String,
    pub complaint: String,
    pub room: Option<String>,
    pub phase: ClinicalPhaseDto,
    pub position: u32,
    pub ambient_eligible: bool,
    pub solo_eligible: bool,
    pub consent: ConsentDto,
    pub recording: RecordingStateDto,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct NoteDto {
    pub patient_id: String,
    pub phase: ClinicalPhaseDto,
    pub hpi: Option<String>,
    pub ros: Option<String>,
    pub exam: Option<String>,
    pub mdm: Option<String>,
    pub disposition: Option<String>,
    pub disposition_text: Option<String>,
    pub objective_items: Vec<ObjectiveDto>,
    pub results: Vec<String>,
    pub medications: Vec<String>,
    pub diagnoses: Vec<String>,
    pub data_reviewed: Vec<String>,
    pub encounter_count: u32,
    pub audit_count: u64,
    pub projection_version: u64,
    pub recovery_required: bool,
    pub manual_draft: Option<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ChartContextDto {
    pub id: String,
    pub patient_id: String,
    pub version: u64,
    pub category: ChartCategory,
    pub text: String,
    pub source: String,
    pub clinical_at_ms: Option<i64>,
    pub entered_at_ms: i64,
    pub reviewed: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct SectionPreferenceDto {
    pub section: String,
    pub instructions: String,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct DefinitionDto {
    pub id: String,
    pub version: u64,
    pub name: String,
    pub kind: DefinitionKindDto,
    pub instructions: String,
    pub sections: Vec<SectionPreferenceDto>,
    pub archived: bool,
    pub is_default: bool,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct DeviceSnapshotDto {
    pub revision: u64,
    pub shift: Option<ShiftDto>,
    pub patients: Vec<PatientDto>,
    pub definitions: Vec<DefinitionDto>,
}
impl From<er_device::ShiftView> for ShiftDto {
    fn from(v: er_device::ShiftView) -> Self {
        Self {
            id: v.id,
            started_at_ms: v.started_at_ms,
            ended_at_ms: v.ended_at_ms,
            selected_patient_id: v.selected_patient_id,
            is_open: v.is_open,
        }
    }
}
impl From<er_device::ConsentView> for ConsentDto {
    fn from(v: er_device::ConsentView) -> Self {
        Self {
            patient_id: v.patient_id,
            status: v.status.into(),
            recorded_at_ms: v.recorded_at_ms,
            policy_version: v.policy_version,
            revision: v.revision,
        }
    }
}
impl From<er_device::RecordingView> for RecordingStateDto {
    fn from(v: er_device::RecordingView) -> Self {
        Self {
            patient_id: v.patient_id,
            solo: v.solo,
            source: v.source,
            segment_id: v.segment_id,
            status: v.status.into(),
            boundary_at_ms: v.boundary_at_ms,
            reason: v.reason,
            hardware_available: v.hardware_available,
        }
    }
}
impl From<er_device::PatientView> for PatientDto {
    fn from(v: er_device::PatientView) -> Self {
        Self {
            id: v.id,
            complaint: v.complaint,
            room: v.room,
            phase: v.phase.into(),
            position: v.position,
            ambient_eligible: v.ambient_eligible,
            solo_eligible: v.solo_eligible,
            consent: v.consent.into(),
            recording: v.recording.into(),
        }
    }
}
impl From<er_device::NoteView> for NoteDto {
    fn from(v: er_device::NoteView) -> Self {
        Self {
            patient_id: v.patient_id,
            phase: v.phase.into(),
            hpi: v.hpi,
            ros: v.ros,
            exam: v.exam,
            mdm: v.mdm,
            disposition: v.disposition,
            disposition_text: v.disposition_text,
            objective_items: v.objective_items.into_iter().map(Into::into).collect(),
            results: v.results,
            medications: v.medications,
            diagnoses: v.diagnoses,
            data_reviewed: v.data_reviewed,
            encounter_count: v.encounter_count,
            audit_count: v.audit_count,
            projection_version: v.projection_version,
            recovery_required: v.recovery_required,
            manual_draft: v.manual_draft,
        }
    }
}
impl From<er_device::ContextView> for ChartContextDto {
    fn from(v: er_device::ContextView) -> Self {
        Self {
            id: v.id,
            patient_id: v.patient_id,
            version: v.version,
            category: v.category.into(),
            text: v.text,
            source: v.source,
            clinical_at_ms: v.clinical_at_ms,
            entered_at_ms: v.entered_at_ms,
            reviewed: v.reviewed,
        }
    }
}
impl From<ChartContextDto> for er_device::ContextView {
    fn from(v: ChartContextDto) -> Self {
        Self {
            id: v.id,
            patient_id: v.patient_id,
            version: v.version,
            category: v.category.into(),
            text: v.text,
            source: v.source,
            clinical_at_ms: v.clinical_at_ms,
            entered_at_ms: v.entered_at_ms,
            reviewed: v.reviewed,
        }
    }
}
impl From<er_device::SectionView> for SectionPreferenceDto {
    fn from(v: er_device::SectionView) -> Self {
        Self {
            section: v.section,
            instructions: v.instructions,
        }
    }
}
impl From<SectionPreferenceDto> for er_device::SectionView {
    fn from(v: SectionPreferenceDto) -> Self {
        Self {
            section: v.section,
            instructions: v.instructions,
        }
    }
}
impl From<er_device::DefinitionView> for DefinitionDto {
    fn from(v: er_device::DefinitionView) -> Self {
        Self {
            id: v.id,
            version: v.version,
            name: v.name,
            kind: v.kind.into(),
            instructions: v.instructions,
            sections: v.sections.into_iter().map(Into::into).collect(),
            archived: v.archived,
            is_default: v.is_default,
        }
    }
}
impl From<DefinitionDto> for er_device::DefinitionView {
    fn from(v: DefinitionDto) -> Self {
        Self {
            id: v.id,
            version: v.version,
            name: v.name,
            kind: v.kind.into(),
            instructions: v.instructions,
            sections: v.sections.into_iter().map(Into::into).collect(),
            archived: v.archived,
            is_default: v.is_default,
        }
    }
}
impl From<er_device::DeviceView> for DeviceSnapshotDto {
    fn from(v: er_device::DeviceView) -> Self {
        Self {
            revision: v.revision,
            shift: v.shift.map(Into::into),
            patients: v.patients.into_iter().map(Into::into).collect(),
            definitions: v.definitions.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum ClinicalPhaseDto {
    Registered,
    Encountering,
    Idle,
    Finalizing,
    PendingSignOff,
    Signed,
}
impl From<er_device::ClinicalPhase> for ClinicalPhaseDto {
    fn from(p: er_device::ClinicalPhase) -> Self {
        match p {
            er_device::ClinicalPhase::Registered => Self::Registered,
            er_device::ClinicalPhase::Encountering => Self::Encountering,
            er_device::ClinicalPhase::Idle => Self::Idle,
            er_device::ClinicalPhase::Finalizing => Self::Finalizing,
            er_device::ClinicalPhase::PendingSignOff => Self::PendingSignOff,
            er_device::ClinicalPhase::Signed => Self::Signed,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ObjectiveDto {
    pub id: String,
    pub category: ChartCategory,
    pub summary: String,
    pub source: String,
    pub clinical_at_ms: Option<i64>,
}
impl From<er_device::ObjectiveView> for ObjectiveDto {
    fn from(v: er_device::ObjectiveView) -> Self {
        Self {
            id: v.id,
            category: v.category.into(),
            summary: v.summary,
            source: v.source,
            clinical_at_ms: v.clinical_at_ms,
        }
    }
}
