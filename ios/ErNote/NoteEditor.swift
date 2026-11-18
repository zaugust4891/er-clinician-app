import Foundation
import SwiftUI

@MainActor
struct NoteEditor: View {
    @ObservedObject var model: WorkspaceModel
    let patientID: UUID

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Manual draft")
                .font(.headline)

            Text("Saved locally, separate from generated sections and export until the note-edit sync contract is added.").font(.footnote)
            TextEditor(text: Binding(
                get: { model.noteText(for: patientID) },
                set: { value in model.editNote(value, for: patientID) }
            ))
            .frame(minHeight: 240)
            .accessibilityLabel("Editable patient note")
            .accessibilityIdentifier("patientNoteEditor")

            if let error = model.noteErrors[patientID] {
                Text(error)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("noteSaveError")

                Button("Retry saving note") {
                    Task { await model.retryNoteSave(for: patientID) }
                }
            }
        }
    }
}
