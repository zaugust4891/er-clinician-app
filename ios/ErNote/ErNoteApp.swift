import SwiftUI
import ErNoteKit

@main
@MainActor
struct ErNoteApp: App {
    @StateObject private var model: WorkspaceModel
    private let storageError: String?

    init() {
        #if DEBUG
        let service: any DeviceService = FixtureDeviceService()
        #else
        let service: any DeviceService = UnavailableDeviceService()
        #endif
        _model = StateObject(wrappedValue: WorkspaceModel(service: service))
        do {
            _ = try ProtectedStorage.databaseURL()
            storageError = nil
        } catch {
            storageError =
                "Protected storage could not be prepared. Persistent saving is unavailable."
        }
    }

    var body: some Scene {
        WindowGroup {
            VStack(spacing: 0) {
                if let storageError {
                    Text(storageError)
                        .foregroundStyle(.red)
                        .padding()
                        .accessibilityIdentifier("storageBootstrapError")
                }
                WorkspaceView(model: model)
            }
        }
    }
}
