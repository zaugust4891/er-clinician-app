import Foundation

/// Step 1 bridge checks only; success does not imply persistence or recording.
public enum BridgeHealth {
    public static func check() throws {
        guard try ping() == "pong" else { throw DeviceServiceError.unavailable }
    }

    /// Validates transport fields without opening a database or contacting a server.
    public static func validateLocalConfiguration(
        databaseURL: URL, accountScope: String
    ) throws -> DeviceConfig {
        try validateConfig(config: DeviceConfig(
            databasePath: databaseURL.path, accountScope: accountScope
        ))
    }
}
