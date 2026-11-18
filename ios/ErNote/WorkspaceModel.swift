import Combine
import Foundation
import ErNoteKit

@MainActor
final class WorkspaceModel: ObservableObject {
    @Published private(set) var snapshot: DeviceSnapshot
    @Published private(set) var commandError: String?
    @Published private(set) var noteErrors: [UUID: String] = [:]
    @Published private var noteDrafts: [UUID: String] = [:]

    @Published private(set) var isWorking = false
    private let service: any DeviceService

    init(service: any DeviceService) {
        self.service = service
        snapshot = service.snapshot
        service.onChange = { [weak self] in self?.refresh() }
    }

    var availability: ServiceAvailability { service.availability }
    var supportsSync: Bool { service.supportsSync }
    func synchronize() async { await perform { try await service.synchronize() } }
    var supportsRecording: Bool { service.supportsRecording }
    var supportsGeneration: Bool { service.supportsGeneration }

    func startShift() async {
        await perform { try await service.startShift() }
    }

    func createPatient(displayLabel: String, chiefComplaint: String) async {
        await perform {
            _ = try await service.createPatient(
                displayLabel: displayLabel,
                chiefComplaint: chiefComplaint
            )
        }
    }

    func selectPatient(_ id: UUID) async {
        await perform { try await service.selectPatient(id) }
    }

    func setConsent(_ consent: ConsentStatus, for id: UUID) async {
        await perform { try await service.setConsent(consent, for: id) }
    }

    func setCaptureMode(_ mode: CaptureMode, for id: UUID) async {
        await perform { try await service.setCaptureMode(mode, for: id) }
    }

    func replaceContext(_ context: [ContextItem], for id: UUID) async {
        await perform { try await service.replaceContext(context, for: id) }
    }

    func savePreference(_ preference: SavedPreference) async {
        await perform { try await service.savePreference(preference) }
    }

    func noteText(for id: UUID) -> String {
        noteDrafts[id]
            ?? snapshot.patients.first(where: { $0.id == id })?.note
            ?? ""
    }

    func editNote(_ text: String, for id: UUID) {
        noteDrafts[id] = text
        Task { await persistNote(text, for: id) }
    }

    func updateNote(_ text: String, for id: UUID) async {
        noteDrafts[id] = text
        await persistNote(text, for: id)
    }

    private func persistNote(_ text: String, for id: UUID) async {
        do {
            try await service.updateNote(text, for: id)
            snapshot = service.snapshot
            noteErrors.removeValue(forKey: id)
            if noteDrafts[id] == text { noteDrafts.removeValue(forKey: id) }
        } catch {
            noteErrors[id] =
                "This edit could not be saved. Your text is retained here."
        }
    }

    func retryNoteSave(for id: UUID) async {
        guard let text = noteDrafts[id] else { return }
        await updateNote(text, for: id)
    }

    func load() async { await perform { try await service.load() } }
    func endShift() async { await perform { try await service.endShift() } }
    func clearSelection() async { await perform { try await service.clearSelection() } }
    func recordObjective(_ input: ObjectiveInput, for id: UUID) async { await perform { try await service.recordObjective(input, for: id) } }

    func refresh() {
        // Unsaved drafts remain separate from the refreshed service snapshot.
        snapshot = service.snapshot
    }

    #if DEBUG
    func preview(
        _ scenario: FixtureDeviceService.PreviewScenario, for id: UUID
    ) {
        do {
            guard let fixture = service as? FixtureDeviceService else {
                throw DeviceServiceError.unavailable
            }
            try fixture.preview(scenario, for: id)
            snapshot = service.snapshot
        } catch { commandError = "Preview unavailable in local-core mode." }
    }
    #endif

    private func perform(_ command: () async throws -> Void) async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await command()
            snapshot = service.snapshot
            commandError = nil
        } catch {
            // Do not expose raw backend errors or patient data in diagnostics.
            switch error {
            case FfiError.Conflict: commandError = "This item changed. Reopen the editor and review the current version."
            case FfiError.Recovery: commandError = "The local store or an unfinished segment needs recovery before this action."
            case FfiError.Scope: commandError = "This local store belongs to a different account scope."
            case FfiError.Busy: commandError = "The local core is busy. Please try again."
            case FfiError.InvalidInput: commandError = "Check the entered fields and try again."
            case FfiError.Storage: commandError = "The local store could not be read or saved."
            default: commandError = "The change could not be completed. Please try again."
            }
        }
    }
}
