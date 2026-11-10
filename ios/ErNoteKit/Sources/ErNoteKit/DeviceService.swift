import Foundation

public enum ConsentStatus: String, Sendable {
    case unknown, granted, declined
}

public enum CaptureMode: String, Sendable {
    case ambient
    case soloDictation
}

/// Recorder presentation is separate from the Rust note phase.
public enum RecorderStatus: String, Sendable {
    case unavailable, paused, interrupted, waiting
}

public enum ServiceAvailability: Sendable {
    case syntheticPreview
    case coreUnavailable
}

public enum ContextCategory: String, CaseIterable, Sendable {
    case history, medication, vital, lab, imaging, treatment, diagnosis
}

public struct ContextItem: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let category: ContextCategory
    public let text: String
    public let source: String
    public let observedAt: Date
    public let needsReview: Bool
    public let isStale: Bool

    public init(
        id: UUID = UUID(), category: ContextCategory, text: String,
        source: String, observedAt: Date,
        needsReview: Bool = false, isStale: Bool = false
    ) {
        self.id = id
        self.category = category
        self.text = text
        self.source = source
        self.observedAt = observedAt
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

    public init(
        id: UUID, displayLabel: String, chiefComplaint: String,
        consent: ConsentStatus = .unknown,
        captureMode: CaptureMode = .ambient,
        recorder: RecorderStatus = .unavailable,
        context: [ContextItem] = [], note: String = "",
        mdmIsStale: Bool = false, separateDraft: String? = nil
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
    }
}

public struct SavedPreference: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let section: String
    public let instructions: String
    public let isDefault: Bool

    public init(
        id: UUID = UUID(), section: String, instructions: String,
        isDefault: Bool = false
    ) {
        self.id = id
        self.section = section
        self.instructions = instructions
        self.isDefault = isDefault
    }
}

public struct DeviceSnapshot: Equatable, Sendable {
    public var shiftID: UUID?
    public var selectedPatientID: UUID?
    public var patients: [PatientView]
    public var preferences: [SavedPreference]

    public init(
        shiftID: UUID? = nil, selectedPatientID: UUID? = nil,
        patients: [PatientView] = [], preferences: [SavedPreference] = []
    ) {
        self.shiftID = shiftID
        self.selectedPatientID = selectedPatientID
        self.patients = patients
        self.preferences = preferences
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
    var supportsGeneration: Bool { get }
    var snapshot: DeviceSnapshot { get }

    func startShift() throws
    func createPatient(displayLabel: String, chiefComplaint: String) throws -> UUID
    func selectPatient(_ id: UUID) throws
    func setConsent(_ consent: ConsentStatus, for id: UUID) throws
    func setCaptureMode(_ mode: CaptureMode, for id: UUID) throws
    func replaceContext(_ context: [ContextItem], for id: UUID) throws
    /// Manual editing remains available independently of recording, consent,
    /// and generation. The future Rust adapter must preserve manual revisions.
    func updateNote(_ text: String, for id: UUID) throws
    func savePreference(_ preference: SavedPreference) throws
}
