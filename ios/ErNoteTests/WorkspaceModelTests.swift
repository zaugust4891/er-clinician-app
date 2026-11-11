import Foundation
import XCTest
import ErNoteKit
#if DEBUG
@testable import ErNote

final class WorkspaceModelTests: XCTestCase {
    @MainActor
    func testFailedEditsSurviveRefreshSwitchAndRetry() async throws {
        let fixture = FixtureDeviceService()
        let first = fixture.createPatient(
            displayLabel: "Synthetic first", chiefComplaint: ""
        )
        let second = fixture.createPatient(
            displayLabel: "Synthetic second", chiefComplaint: ""
        )
        try fixture.updateNote("Original", for: first)
        let service = FailingNoteService(backing: fixture)
        let model = WorkspaceModel(service: service)

        model.updateNote("Unsaved revision", for: first)
        model.selectPatient(second)
        model.refresh()
        XCTAssertEqual(model.noteText(for: first), "Unsaved revision")
        XCTAssertEqual(model.noteText(for: second), "")
        XCTAssertEqual(fixture.snapshot.patients[0].note, "Original")
        XCTAssertNotNil(model.noteErrors[first])

        service.failNotes = false
        model.retryNoteSave(for: first)
        XCTAssertEqual(fixture.snapshot.patients[0].note, "Unsaved revision")
        XCTAssertNil(model.noteErrors[first])

        service.failNotes = true
        model.updateNote("", for: first)
        try fixture.preview(.staleMdm, for: first)
        model.refresh()
        XCTAssertEqual(model.noteText(for: first), "")
        XCTAssertEqual(fixture.snapshot.patients[0].note, "Unsaved revision")

        service.failNotes = false
        model.retryNoteSave(for: first)
        XCTAssertEqual(fixture.snapshot.patients[0].note, "")
        XCTAssertNil(model.noteErrors[first])
    }

    @MainActor
    func testModelKeepsNoteEditableAcrossEveryPreview() async throws {
        let fixture = FixtureDeviceService()
        let patient = fixture.createPatient(
            displayLabel: "Synthetic patient", chiefComplaint: ""
        )
        let model = WorkspaceModel(service: fixture)
        model.updateNote("Manual note", for: patient)

        for scenario in FixtureDeviceService.PreviewScenario.allCases {
            let previous = model.noteText(for: patient)
            model.preview(scenario, for: patient)
            XCTAssertEqual(model.noteText(for: patient), previous)
            model.updateNote("Edited after \(scenario)", for: patient)
            XCTAssertEqual(
                fixture.snapshot.patients[0].note, "Edited after \(scenario)"
            )
            XCTAssertNil(model.noteErrors[patient])
        }
    }

    func testStorageDirectoryHasRequiredAttributes() throws {
        let database = try ProtectedStorage.databaseURL()
        let directory = database.deletingLastPathComponent()
        XCTAssertEqual(database.lastPathComponent, "device.sqlite")
        XCTAssertEqual(directory.lastPathComponent, "ClinicalData")

        let resources = try directory.resourceValues(
            forKeys: [.isExcludedFromBackupKey, .isDirectoryKey]
        )
        XCTAssertEqual(resources.isDirectory, true)
        XCTAssertEqual(resources.isExcludedFromBackup, true)
        let attributes = try FileManager.default.attributesOfItem(
            atPath: directory.path
        )
        XCTAssertEqual(
            attributes[.protectionKey] as? String,
            FileProtectionType.completeUntilFirstUserAuthentication.rawValue
        )
    }
}

@MainActor
private final class FailingNoteService: DeviceService {
    let backing: FixtureDeviceService
    var failNotes = true
    let availability = ServiceAvailability.syntheticPreview
    let supportsRecording = false
    let supportsGeneration = false
    var snapshot: DeviceSnapshot { backing.snapshot }

    init(backing: FixtureDeviceService) {
        self.backing = backing
    }

    func startShift() throws { backing.startShift() }
    func createPatient(
        displayLabel: String, chiefComplaint: String
    ) throws -> UUID {
        backing.createPatient(
            displayLabel: displayLabel, chiefComplaint: chiefComplaint
        )
    }
    func selectPatient(_ id: UUID) throws {
        try backing.selectPatient(id)
    }
    func setConsent(_ consent: ConsentStatus, for id: UUID) throws {
        try backing.setConsent(consent, for: id)
    }
    func setCaptureMode(_ mode: CaptureMode, for id: UUID) throws {
        try backing.setCaptureMode(mode, for: id)
    }
    func replaceContext(_ context: [ContextItem], for id: UUID) throws {
        try backing.replaceContext(context, for: id)
    }
    func updateNote(_ text: String, for id: UUID) throws {
        if failNotes { throw DeviceServiceError.unavailable }
        try backing.updateNote(text, for: id)
    }
    func savePreference(_ preference: SavedPreference) throws {
        backing.savePreference(preference)
    }
}
#endif
