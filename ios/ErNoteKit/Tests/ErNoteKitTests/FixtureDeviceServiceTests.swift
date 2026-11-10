import XCTest
@testable import ErNoteKit

#if DEBUG
final class FixtureDeviceServiceTests: XCTestCase {
    @MainActor
    func testSelectionAndPreviewStayWithTheirPatient() async throws {
        let service = FixtureDeviceService()
        service.startShift()
        let first = service.createPatient(
            displayLabel: "Synthetic room 1", chiefComplaint: "Synthetic chest pain"
        )
        let second = service.createPatient(
            displayLabel: "Synthetic room 2", chiefComplaint: "Synthetic ankle pain"
        )
        try service.selectPatient(first)
        try service.preview(.consentDeclinedSolo, for: first)
        try service.preview(.paused, for: first)
        XCTAssertEqual(service.snapshot.selectedPatientID, first)
        XCTAssertEqual(service.snapshot.patients[0].consent, .declined)
        XCTAssertEqual(service.snapshot.patients[0].captureMode, .soloDictation)
        XCTAssertEqual(service.snapshot.patients[0].recorder, .paused)
        XCTAssertEqual(service.snapshot.patients[1].id, second)
        XCTAssertEqual(service.snapshot.patients[1].consent, .unknown)
        XCTAssertEqual(service.snapshot.patients[1].recorder, .unavailable)
        XCTAssertFalse(service.supportsRecording)
        XCTAssertFalse(service.supportsGeneration)
    }

    @MainActor
    func testUnknownPatientDoesNotMutateSnapshot() async throws {
        let service = FixtureDeviceService()
        let before = service.snapshot
        XCTAssertThrowsError(try service.selectPatient(UUID())) {
            XCTAssertEqual($0 as? DeviceServiceError, .unknownPatient)
        }
        XCTAssertThrowsError(try service.updateNote("Must not apply", for: UUID()))
        XCTAssertEqual(service.snapshot, before)
    }

    @MainActor
    func testNoteRemainsEditableThroughoutEveryPreview() async throws {
        let fixture = FixtureDeviceService()
        let service: any DeviceService = fixture
        let patient = try service.createPatient(
            displayLabel: "Synthetic room", chiefComplaint: "Synthetic complaint"
        )
        try service.updateNote("Manual text before consent", for: patient)
        for scenario in FixtureDeviceService.PreviewScenario.allCases {
            let before = service.snapshot.patients[0].note
            try fixture.preview(scenario, for: patient)
            XCTAssertEqual(service.snapshot.patients[0].note, before)
            try service.updateNote("Manual revision: \(scenario)", for: patient)
            XCTAssertEqual(
                service.snapshot.patients[0].note, "Manual revision: \(scenario)"
            )
        }
        try service.updateNote("", for: patient)
        try fixture.preview(.staleMdm, for: patient)
        XCTAssertEqual(service.snapshot.patients[0].note, "")
    }

    @MainActor
    func testPreviewDraftDoesNotReplaceNote() async throws {
        let service = FixtureDeviceService()
        let patient = service.createPatient(
            displayLabel: "Synthetic room", chiefComplaint: "Synthetic complaint"
        )
        try service.updateNote("Clinician's manual note", for: patient)
        try service.preview(.staleMdm, for: patient)
        try service.preview(.dischargeDraft, for: patient)
        XCTAssertEqual(service.snapshot.patients[0].note, "Clinician's manual note")
        XCTAssertNotNil(service.snapshot.patients[0].separateDraft)
    }
}
#endif
