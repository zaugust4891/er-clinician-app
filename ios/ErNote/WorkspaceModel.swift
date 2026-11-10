import Combine
import Foundation
import ErNoteKit

@MainActor
final class WorkspaceModel: ObservableObject {
    @Published private(set) var snapshot: DeviceSnapshot
    @Published private(set) var commandError: String?
    @Published private(set) var noteErrors: [UUID: String] = [:]
    @Published private var noteDrafts: [UUID: String] = [:]

    private let service: any DeviceService

    init(service: any DeviceService) {
        self.service = service
        snapshot = service.snapshot
    }

    var availability: ServiceAvailability { service.availability }
    var supportsRecording: Bool { service.supportsRecording }
    var supportsGeneration: Bool { service.supportsGeneration }

    func startShift() {
        perform { try service.startShift() }
    }

    func createPatient(displayLabel: String, chiefComplaint: String) {
        perform {
            _ = try service.createPatient(
                displayLabel: displayLabel,
                chiefComplaint: chiefComplaint
            )
        }
    }

    func selectPatient(_ id: UUID) {
        perform { try service.selectPatient(id) }
    }

    func setConsent(_ consent: ConsentStatus, for id: UUID) {
        perform { try service.setConsent(consent, for: id) }
    }

    func setCaptureMode(_ mode: CaptureMode, for id: UUID) {
        perform { try service.setCaptureMode(mode, for: id) }
    }

    func replaceContext(_ context: [ContextItem], for id: UUID) {
        perform { try service.replaceContext(context, for: id) }
    }

    func savePreference(_ preference: SavedPreference) {
        perform { try service.savePreference(preference) }
    }

    func noteText(for id: UUID) -> String {
        noteDrafts[id]
            ?? snapshot.patients.first(where: { $0.id == id })?.note
            ?? ""
    }

    func updateNote(_ text: String, for id: UUID) {
        // Retain the user's text before attempting the service command.
        // An empty string is an intentional edit and must also be retained.
        noteDrafts[id] = text
        do {
            try service.updateNote(text, for: id)
            snapshot = service.snapshot
            noteErrors.removeValue(forKey: id)
            noteDrafts.removeValue(forKey: id)
        } catch {
            noteErrors[id] =
                "This edit could not be saved. Your text is retained here."
        }
    }

    func retryNoteSave(for id: UUID) {
        guard let text = noteDrafts[id] else { return }
        updateNote(text, for: id)
    }

    func refresh() {
        // Unsaved drafts remain separate from the refreshed service snapshot.
        snapshot = service.snapshot
    }

    #if DEBUG
    func preview(
        _ scenario: FixtureDeviceService.PreviewScenario, for id: UUID
    ) {
        perform {
            guard let fixture = service as? FixtureDeviceService else {
                throw DeviceServiceError.unavailable
            }
            try fixture.preview(scenario, for: id)
        }
    }
    #endif

    private func perform(_ command: () throws -> Void) {
        do {
            try command()
            snapshot = service.snapshot
            commandError = nil
        } catch {
            // Do not expose raw backend errors or patient data in diagnostics.
            commandError = "The change could not be completed. Please try again."
        }
    }
}
