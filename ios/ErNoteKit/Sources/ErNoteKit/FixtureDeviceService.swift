#if DEBUG
import Foundation

/// In-memory synthetic presentation data. Excluded from Release builds.
/// No microphone, network, transcription, generation, or clinical persistence.
@MainActor
public final class FixtureDeviceService: DeviceService {
    public let availability = ServiceAvailability.syntheticPreview
    public let supportsRecording = false
    public let supportsGeneration = false
    public private(set) var snapshot = DeviceSnapshot()

    public init() {}

    public func startShift() {
        if snapshot.shiftID == nil { snapshot.shiftID = UUID() }
    }

    public func createPatient(displayLabel: String, chiefComplaint: String) -> UUID {
        let id = UUID()
        snapshot.patients.append(PatientView(
            id: id, displayLabel: displayLabel, chiefComplaint: chiefComplaint
        ))
        snapshot.selectedPatientID = id
        return id
    }

    public func selectPatient(_ id: UUID) throws {
        _ = try index(of: id)
        snapshot.selectedPatientID = id
    }

    public func setConsent(_ consent: ConsentStatus, for id: UUID) throws {
        let index = try index(of: id)
        snapshot.patients[index].consent = consent
    }

    public func setCaptureMode(_ mode: CaptureMode, for id: UUID) throws {
        let index = try index(of: id)
        snapshot.patients[index].captureMode = mode
    }

    public func replaceContext(_ context: [ContextItem], for id: UUID) throws {
        let index = try index(of: id)
        snapshot.patients[index].context = context
    }

    public func updateNote(_ text: String, for id: UUID) throws {
        let index = try index(of: id)
        snapshot.patients[index].note = text
    }

    public func savePreference(_ preference: SavedPreference) {
        snapshot.preferences.removeAll { $0.id == preference.id }
        snapshot.preferences.append(preference)
    }

    public enum PreviewScenario: CaseIterable {
        case consentDeclinedSolo, paused, interrupted
        case waiting, correctedResult, staleMdm, dischargeDraft
    }

    /// Development fixture controls only. Previews never overwrite manual notes.
    public func preview(_ scenario: PreviewScenario, for id: UUID) throws {
        let index = try index(of: id)
        switch scenario {
        case .consentDeclinedSolo:
            snapshot.patients[index].consent = .declined
            snapshot.patients[index].captureMode = .soloDictation
        case .paused:
            snapshot.patients[index].recorder = .paused
        case .interrupted:
            snapshot.patients[index].recorder = .interrupted
        case .waiting:
            snapshot.patients[index].recorder = .waiting
        case .correctedResult:
            snapshot.patients[index].context = [ContextItem(
                category: .lab,
                text: "Synthetic corrected potassium: 4.2 mmol/L",
                source: "Synthetic fixture — EMR disconnected",
                observedAt: Date(timeIntervalSince1970: 0),
                needsReview: true, isStale: true
            )]
        case .staleMdm:
            snapshot.patients[index].mdmIsStale = true
        case .dischargeDraft:
            snapshot.patients[index].separateDraft =
                "Synthetic discharge draft — not generated or signed."
        }
    }

    private func index(of id: UUID) throws -> Int {
        guard let index = snapshot.patients.firstIndex(where: { $0.id == id }) else {
            throw DeviceServiceError.unknownPatient
        }
        return index
    }
}
#endif
