import SwiftUI
import ErNoteKit

@main
@MainActor
struct ErNoteApp: App {
    @StateObject private var model: WorkspaceModel
    private let storageError: String?

    init() {
        let service: any DeviceService
        do {
            let url = try ProtectedStorage.databaseURL()
            service = RustDeviceService(databaseURL: url, accountScope: "synthetic-development")
            storageError = nil
        } catch {
            service = UnavailableDeviceService()
            storageError = "Protected storage could not be prepared. Persistent saving is unavailable."
        }
        _model = StateObject(wrappedValue: WorkspaceModel(service: service))
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
