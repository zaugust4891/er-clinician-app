import Foundation
import SwiftUI
import ErNoteKit

@MainActor
struct PreferencesView: View {
    @ObservedObject var model: WorkspaceModel
    @State private var editing: SavedPreference?

    var body: some View {
        List {
            Text("Preview definitions only. Changes are not retained after restart.")
                .font(.footnote)
            ForEach(model.snapshot.preferences) { preference in
                Button {
                    editing = preference
                } label: {
                    VStack(alignment: .leading) {
                        Text(preference.section)
                        Text(preference.instructions).font(.caption)
                        if preference.isDefault { Text("Preferred").font(.caption) }
                    }
                }
            }
            Button("Add section preference or saved prompt") {
                editing = SavedPreference(section: "MDM", instructions: "")
            }
        }
        .navigationTitle("Preferences")
        .sheet(item: $editing) { preference in
            PreferenceEditor(model: model, preference: preference)
        }
    }
}

@MainActor
private struct PreferenceEditor: View {
    @ObservedObject var model: WorkspaceModel
    @Environment(\.dismiss) private var dismiss
    private let preferenceID: UUID
    @State private var section: String
    @State private var instructions: String
    @State private var isDefault: Bool

    init(model: WorkspaceModel, preference: SavedPreference) {
        self.model = model
        preferenceID = preference.id
        _section = State(initialValue: preference.section)
        _instructions = State(initialValue: preference.instructions)
        _isDefault = State(initialValue: preference.isDefault)
    }

    var body: some View {
        NavigationStack {
            Form {
                TextField("Section or prompt name", text: $section)
                Text("Examples: HPI, MDM, Admission message, Discharge instructions.")
                    .font(.footnote)
                TextEditor(text: $instructions)
                    .frame(minHeight: 180)
                    .accessibilityLabel("Preference instructions")
                Toggle("Preferred", isOn: $isDefault)
                Text("Saving a prompt does not run generation or change a note.")
                    .font(.footnote)
                if let error = model.commandError {
                    Text(error).foregroundStyle(.red)
                }
            }
            .navigationTitle("Edit preference")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        let preference = SavedPreference(
                            id: preferenceID, section: section,
                            instructions: instructions, isDefault: isDefault
                        )
                        model.savePreference(preference)
                        if model.snapshot.preferences.contains(preference) {
                            dismiss()
                        }
                    }
                    .disabled(
                        section.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    )
                }
            }
        }
    }
}
