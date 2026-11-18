import Foundation

public enum ConsentStatus: String, Sendable {
    case unknown, granted, declined, revoked
}

public enum CaptureMode: String, Sendable {
    case ambient
    case soloDictation
}

/// Recorder presentation is separate from the Rust note phase.
public enum RecorderStatus: String, Sendable {
    case unavailable, paused, interrupted, waiting, pendingStop
}

public enum ServiceAvailability: Sendable {
    case syntheticPreview
    case coreUnavailable
    case localCore
}

public enum ContextCategory: String, CaseIterable, Sendable {
    case history, medication, vital, lab, imaging, treatment, diagnosis
}

public struct ContextItem: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let category: ContextCategory
    public let text: String
    public let source: String
    public let observedAt: Date?
    public let enteredAt: Date
    public let version: UInt64
    public let needsReview: Bool
    public let isStale: Bool

    public init(
        id: UUID = UUID(), category: ContextCategory, text: String,
        source: String, observedAt: Date?, enteredAt: Date = Date(), version: UInt64 = 0,
        needsReview: Bool = false, isStale: Bool = false
    ) {
        self.id = id
        self.category = category
        self.text = text
        self.source = source
        self.observedAt = observedAt
        self.enteredAt = enteredAt
        self.version = version
        self.needsReview = needsReview
        self.isStale = isStale
    }
}

public struct PatientView: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let displayLabel: String
    public let chiefComplaint: String
    public var consent: ConsentStatus
    public var captureMode: CaptureMode
    public var recorder: RecorderStatus
    public var context: [ContextItem]
    public var note: String
    public var mdmIsStale: Bool
    public var separateDraft: String?
    public var generatedSections: [String]
    public var objectiveLines: [String]

    public init(
        id: UUID, displayLabel: String, chiefComplaint: String,
        consent: ConsentStatus = .unknown,
        captureMode: CaptureMode = .ambient,
        recorder: RecorderStatus = .unavailable,
        context: [ContextItem] = [], note: String = "",
        mdmIsStale: Bool = false, separateDraft: String? = nil, generatedSections: [String] = [], objectiveLines: [String] = []
    ) {
        self.id = id
        self.displayLabel = displayLabel
        self.chiefComplaint = chiefComplaint
        self.consent = consent
        self.captureMode = captureMode
        self.recorder = recorder
        self.context = context
        self.note = note
        self.mdmIsStale = mdmIsStale
        self.separateDraft = separateDraft
        self.generatedSections = generatedSections
        self.objectiveLines = objectiveLines
    }
}

public enum PreferenceKind: String, CaseIterable, Sendable { case template, admissionMessage, dischargeInstructions, custom }

public struct SavedPreference: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let section: String
    public let instructions: String
    public let isDefault: Bool
    public let version: UInt64
    public let kind: PreferenceKind
    public let archived: Bool

    public init(
        id: UUID = UUID(), section: String, instructions: String,
        isDefault: Bool = false, version: UInt64 = 0, kind: PreferenceKind = .template, archived: Bool = false
    ) {
        self.id = id
        self.section = section
        self.instructions = instructions
        self.isDefault = isDefault
        self.version = version
        self.kind = kind
        self.archived = archived
    }
}

public struct SyncPresentation: Equatable, Sendable {
    public var status: String
    public var queued: UInt64
    public var awaitingConfirmation: UInt64
    public var conflicts: UInt64
    public init(status: String = "not_connected", queued: UInt64 = 0, awaitingConfirmation: UInt64 = 0, conflicts: UInt64 = 0) {
        self.status = status; self.queued = queued; self.awaitingConfirmation = awaitingConfirmation; self.conflicts = conflicts
    }
    public var message: String {
        switch status {
        case "up_to_date": return "Clinical records are up to date"
        case "not_connected": return "Cloud connection not configured"
        case "authentication": return "Sign in to continue synchronization"
        case "conflict": return "A pending change needs review"
        case "offline": return "Waiting for a connection"
        case "budget_wait", "rate_limited": return "Waiting before the next connection attempt"
        case "missing_on_server": return "A record could not be found on the server"
        case "rejected": return "The server could not accept a pending change"
        default: return "Changes are waiting for confirmation"
        }
    }
}

public struct DeviceSnapshot: Equatable, Sendable {
    public var sync: SyncPresentation
    public var shiftID: UUID?
    public var selectedPatientID: UUID?
    public var patients: [PatientView]
    public var preferences: [SavedPreference]
    public var revision: UInt64

    public init(
        sync: SyncPresentation = SyncPresentation(), shiftID: UUID? = nil, selectedPatientID: UUID? = nil,
        patients: [PatientView] = [], preferences: [SavedPreference] = [], revision: UInt64 = 0
    ) {
        self.sync = sync
        self.shiftID = shiftID
        self.selectedPatientID = selectedPatientID
        self.patients = patients
        self.preferences = preferences
        self.revision = revision
    }
}

public enum DeviceServiceError: Error, Equatable {
    case unavailable
    case unknownPatient
}

/// SwiftUI consumes views and commands through this boundary.
/// Step 2 supplies a Rust-backed adapter that owns command validation.
@MainActor
public protocol DeviceService: AnyObject {
    var availability: ServiceAvailability { get }
    var supportsRecording: Bool { get }
    var supportsSync: Bool { get }
    func synchronize() async throws
    var supportsGeneration: Bool { get }
    var snapshot: DeviceSnapshot { get }

    var onChange: (() -> Void)? { get set }
    func load() async throws
    func close() async throws
    func recordObjective(_ input: ObjectiveInput, for id: UUID) async throws
    func endShift() async throws
    func clearSelection() async throws
    func startShift() async throws
    func createPatient(displayLabel: String, chiefComplaint: String) async throws -> UUID
    func selectPatient(_ id: UUID) async throws
    func setConsent(_ consent: ConsentStatus, for id: UUID) async throws
    func setCaptureMode(_ mode: CaptureMode, for id: UUID) async throws
    func replaceContext(_ context: [ContextItem], for id: UUID) async throws
    /// Manual editing remains available independently of recording, consent,
    /// and generation. The future Rust adapter must preserve manual revisions.
    func updateNote(_ text: String, for id: UUID) async throws
    func savePreference(_ preference: SavedPreference) async throws
}

@MainActor
public extension DeviceService {
    var supportsSync: Bool { false }
    func synchronize() async throws { throw DeviceServiceError.unavailable }
    var onChange: (() -> Void)? { get { nil } set {} }
    func load() async throws {}
    func close() async throws {}
    func recordObjective(_ input: ObjectiveInput, for id: UUID) async throws { throw DeviceServiceError.unavailable }
    func endShift() async throws { throw DeviceServiceError.unavailable }
    func clearSelection() async throws { throw DeviceServiceError.unavailable }
}
