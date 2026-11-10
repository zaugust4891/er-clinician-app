import SwiftUI

@main
struct ErNoteWatchApp: App {
    var body: some Scene {
        WindowGroup {
            VStack(spacing: 12) {
                Text("ER Note").font(.headline)
                Text("Use the iPhone app.")
                Text("Watch controls and recording are unavailable.")
                    .font(.footnote)
            }
            .padding()
        }
    }
}
