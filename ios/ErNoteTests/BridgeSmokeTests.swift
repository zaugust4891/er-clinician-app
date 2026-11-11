import Foundation
import XCTest
import ErNoteKit

final class BridgeSmokeTests: XCTestCase {
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
