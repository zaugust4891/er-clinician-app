import Foundation

/// Used until the persistent Rust service is connected in Step 2.
@MainActor
public final class UnavailableDeviceService: DeviceService {
    public let availability = ServiceAvailability.coreUnavailable
    public let supportsRecording = false
    public let supportsGeneration = false
    public let snapshot = DeviceSnapshot()

    public init() {}

    public func startShift() throws {
        throw DeviceServiceError.unavailable
    }

    public func createPatient(
        displayLabel: String, chiefComplaint: String
    ) throws -> UUID {
        throw DeviceServiceError.unavailable
    }

    public func selectPatient(_ id: UUID) throws {
        throw DeviceServiceError.unavailable
    }

    public func setConsent(_ consent: ConsentStatus, for id: UUID) throws {
        throw DeviceServiceError.unavailable
    }

    public func setCaptureMode(_ mode: CaptureMode, for id: UUID) throws {
        throw DeviceServiceError.unavailable
    }

    public func replaceContext(_ context: [ContextItem], for id: UUID) throws {
        throw DeviceServiceError.unavailable
    }

    public func updateNote(_ text: String, for id: UUID) throws {
        throw DeviceServiceError.unavailable
    }

    public func savePreference(_ preference: SavedPreference) throws {
        throw DeviceServiceError.unavailable
    }
}
