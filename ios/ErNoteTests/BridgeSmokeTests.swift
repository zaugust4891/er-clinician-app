import Foundation
import XCTest
import ErNoteKit

final class BridgeSmokeTests: XCTestCase {
    @MainActor
    func testPersistentRustServiceSurvivesReopen() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = directory.appendingPathComponent("device.sqlite")
        let service = RustDeviceService(databaseURL: database, accountScope: "synthetic-smoke")
        try await service.load()
        try await service.startShift()
        let first = try await service.createPatient(displayLabel: "Room 4", chiefComplaint: "Synthetic chest pain 🫀")
        _ = try await service.createPatient(displayLabel: "Hall 3", chiefComplaint: "Synthetic ankle pain")
        try await service.selectPatient(first)
        try await service.setConsent(.declined, for: first)
        try await service.setCaptureMode(.soloDictation, for: first)
        try await service.recordObjective(.lab(name: "Synthetic potassium", value: 4.2, unit: "mmol/L", abnormal: false), for: first)
        try await service.replaceContext([ContextItem(category: .history, text: "Synthetic context", source: "Clinician supplied", observedAt: nil, needsReview: true)], for: first)
        try await service.savePreference(SavedPreference(section: "HPI", instructions: "Brief prose"))
        try await service.savePreference(SavedPreference(section: "Admission message", instructions: "Separate reviewable message", kind: .admissionMessage))
        try await service.updateNote("Synthetic manual draft", for: first)
        let shift = service.snapshot.shiftID
        try await service.close()
        let reopened = RustDeviceService(databaseURL: database, accountScope: "synthetic-smoke")
        try await reopened.load()
        XCTAssertEqual(reopened.snapshot.shiftID, shift)
        XCTAssertEqual(reopened.snapshot.selectedPatientID, first)
        XCTAssertEqual(reopened.snapshot.patients.count, 2)
        XCTAssertEqual(reopened.snapshot.patients[0].consent, .declined)
        XCTAssertEqual(reopened.snapshot.patients[0].captureMode, .soloDictation)
        XCTAssertEqual(reopened.snapshot.patients[0].note, "Synthetic manual draft")
        XCTAssertEqual(reopened.snapshot.patients[0].context[0].observedAt, nil)
        XCTAssertEqual(reopened.snapshot.preferences.count, 2)
        XCTAssertEqual(reopened.snapshot.sync.status, "not_connected")
        XCTAssertEqual(reopened.snapshot.sync.queued, 1)
        XCTAssertFalse(reopened.supportsRecording)
        XCTAssertFalse(reopened.supportsGeneration)
        try await reopened.close()
        XCTAssertEqual(try deviceInterfaceVersion(), 3)
    }
    func testProductionBridgeAndConfigRoundTrip() throws {
        XCTAssertEqual(try ping(), "pong")
        try BridgeHealth.check()
        let config = try BridgeHealth.validateLocalConfiguration(
            databaseURL: URL(fileURLWithPath: "/synthetic/ClinicalData/device.sqlite"),
            accountScope: "synthetic-account"
        )
        XCTAssertEqual(config.databasePath, "/synthetic/ClinicalData/device.sqlite")
        XCTAssertEqual(config.accountScope, "synthetic-account")
    }

    func testInvalidConfigThrowsDeclaredSanitizedError() {
        XCTAssertThrowsError(try validateConfig(config: DeviceConfig(
            databasePath: "ER_FFI_PRIVATE_SENTINEL", accountScope: "synthetic-account"
        ))) { error in
            guard case FfiError.InvalidInput = error else {
                return XCTFail("Expected typed InvalidInput")
            }
            XCTAssertFalse(String(describing: error).contains("ER_FFI_PRIVATE_SENTINEL"))
        }
    }

    // Compiled only in the isolated test project with matching test bindings.
    #if ER_FFI_PANIC_TEST
    func testStatefulPanicFaultsHandleAndAllowsDurableReopen() throws {
        let database = FileManager.default.temporaryDirectory.appendingPathComponent("panic-\(UUID().uuidString).sqlite")
        defer { try? FileManager.default.removeItem(at: database) }
        let config = DeviceConfig(databasePath: database.path, accountScope: "synthetic-panic")
        let core = try ErDevice(config: config)
        let shift = try core.startShift(atMs: Int64(Date().timeIntervalSince1970 * 1000))
        XCTAssertThrowsError(try core.panicDeviceForTest())
        XCTAssertThrowsError(try core.snapshot())
        try core.close()
        let reopened = try ErDevice(config: config)
        XCTAssertEqual(try reopened.snapshot().shift?.id, shift)
        try reopened.close()
    }
    func testOptimizedPanicThrowsTypedErrorAndPingStillWorks() throws {
        XCTAssertThrowsError(try panicForTest()) { error in
            guard case FfiError.Internal = error else {
                return XCTFail("Expected typed Internal")
            }
            XCTAssertFalse(String(describing: error).contains("ER_FFI_PRIVATE_SENTINEL"))
        }
        XCTAssertEqual(try ping(), "pong")
    }
    #endif
}
