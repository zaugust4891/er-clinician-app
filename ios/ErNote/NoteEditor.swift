import Foundation
import SwiftUI

@MainActor
struct NoteEditor: View {
    @ObservedObject var model: WorkspaceModel
    let patientID: UUID

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Note")
                .font(.headline)

            TextEditor(text: Binding(
                get: { model.noteText(for: patientID) },
                set: { model.updateNote($0, for: patientID) }
            ))
            .frame(minHeight: 240)
            .accessibilityLabel("Editable patient note")
            .accessibilityIdentifier("patientNoteEditor")

            if let error = model.noteErrors[patientID] {
                Text(error)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("noteSaveError")

                Button("Retry saving note") {
                    model.retryNoteSave(for: patientID)
                }
            }
        }
    }
}
