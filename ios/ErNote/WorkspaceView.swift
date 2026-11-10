import Foundation
import SwiftUI
import ErNoteKit

@MainActor
struct WorkspaceView: View {
    @ObservedObject var model: WorkspaceModel
    @State private var patientLabel = ""
    @State private var complaint = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    switch model.availability {
                    case .syntheticPreview:
                        Text("Synthetic preview • changes are lost on restart")
                    case .coreUnavailable:
                        Text("Local storage is unavailable in this build.")
                    }
                    Text("Recording, generation, and EMR connections are unavailable.")
                        .font(.footnote)
                    if let error = model.commandError {
                        Text(error).foregroundStyle(.red)
                    }
                }

                Section("Shift") {
                    if model.snapshot.shiftID == nil {
                        Button("Start shift") { model.startShift() }
                    } else {
                        Text("Shift open")
                        TextField("Patient label", text: $patientLabel)
                        TextField("Chief complaint", text: $complaint)
                        Button("Add patient") {
                            let count = model.snapshot.patients.count
                            model.createPatient(
                                displayLabel: patientLabel,
                                chiefComplaint: complaint
                            )
                            if model.snapshot.patients.count > count {
                                patientLabel = ""
                                complaint = ""
                            }
                        }
                        .disabled(
                            patientLabel.trimmingCharacters(
                                in: .whitespacesAndNewlines
                            ).isEmpty
                        )
                    }
                }

                Section("Patients") {
                    if model.snapshot.patients.isEmpty {
                        Text("Add a patient to begin.")
                    }
                    ForEach(model.snapshot.patients) { patient in
                        Button {
                            model.selectPatient(patient.id)
                        } label: {
                            HStack {
                                VStack(alignment: .leading) {
                                    Text(patient.displayLabel)
                                    Text(patient.chiefComplaint)
                                        .font(.caption)
                                }
                                Spacer()
                                if model.snapshot.selectedPatientID == patient.id {
                                    Image(systemName: "checkmark")
                                        .accessibilityLabel("Selected patient")
                                }
                            }
                        }
                    }
                }

                if let patient = model.snapshot.patients.first(where: {
                    $0.id == model.snapshot.selectedPatientID
                }) {
                    PatientWorkspace(model: model, patient: patient)
                        .id(patient.id)
                }
            }
            .navigationTitle("ER Note")
            .toolbar {
                NavigationLink("Preferences") {
                    PreferencesView(model: model)
                }
            }
        }
    }
}

@MainActor
private struct PatientWorkspace: View {
    @ObservedObject var model: WorkspaceModel
    let patient: PatientView
    @State private var page = "Note"
    @State private var category = ContextCategory.history
    @State private var contextText = ""
    #if DEBUG
    @State private var generationPreview = "Unavailable"
    #endif

    var body: some View {
        Section(patient.displayLabel) {
            Picker("Workspace", selection: $page) {
                Text("Record").tag("Record")
                Text("Note").tag("Note")
                Text("Chart").tag("Chart")
            }
            .pickerStyle(.segmented)
        }

        switch page {
        case "Record":
            Section("Recording") {
                Picker("Ambient consent", selection: Binding(
                    get: { patient.consent },
                    set: { model.setConsent($0, for: patient.id) }
                )) {
                    Text("Unknown").tag(ConsentStatus.unknown)
                    Text("Granted").tag(ConsentStatus.granted)
                    Text("Declined").tag(ConsentStatus.declined)
                }
                Picker("Mode", selection: Binding(
                    get: { patient.captureMode },
                    set: { model.setCaptureMode($0, for: patient.id) }
                )) {
                    Text("Ambient").tag(CaptureMode.ambient)
                    Text("Solo dictation").tag(CaptureMode.soloDictation)
                }
                Text("Solo dictation means clinician-only narration.")
                    .font(.footnote)
                Text("Recorder: \(patient.recorder.rawValue)")
                Text("Microphone access is not requested in this build.")
                HStack {
                    Button("Record") {}
                    Button("Pause") {}
                    Button("Resume") {}
                }
                .disabled(true)
            }
        case "Chart":
            Section("Chart context") {
                Text("EMR disconnected • manual and synthetic context only")
                ForEach(patient.context) { item in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(item.category.rawValue.capitalized).font(.headline)
                        Text(item.text)
                        Text(item.source).font(.caption)
                        Text(item.observedAt, style: .date).font(.caption)
                        if item.isStale { Text("Stale context") }
                        if item.needsReview { Text("Needs review") }
                    }
                }
                Picker("Category", selection: $category) {
                    ForEach(ContextCategory.allCases, id: \.self) { value in
                        Text(value.rawValue.capitalized).tag(value)
                    }
                }
                TextField("Manual context", text: $contextText, axis: .vertical)
                Button("Add context") {
                    let item = ContextItem(
                        category: category,
                        text: contextText,
                        source: "Clinician supplied — entered now; clinical time unverified",
                        observedAt: Date(),
                        needsReview: true
                    )
                    model.replaceContext(patient.context + [item], for: patient.id)
                    if model.snapshot.patients.first(where: {
                        $0.id == patient.id
                    })?.context.contains(where: { $0.id == item.id }) == true {
                        contextText = ""
                    }
                }
                .disabled(
                    contextText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
            }
        default:
            Section {
                NoteEditor(model: model, patientID: patient.id)
                if patient.mdmIsStale {
                    Text("New context needs review. Existing note text is preserved.")
                }
                Button("Generate draft") {}
                    .disabled(true)
                if let draft = patient.separateDraft {
                    DisclosureGroup("Separate synthetic draft") {
                        Text(draft)
                        Text("This draft has not replaced the note.")
                            .font(.footnote)
                    }
                }
            }
        }

        #if DEBUG
        Section("Synthetic previews") {
            Text("These controls change preview data only.")
            Menu("Preview patient state") {
                Button("Consent declined → solo") {
                    model.preview(.consentDeclinedSolo, for: patient.id)
                }
                Button("Paused") { model.preview(.paused, for: patient.id) }
                Button("Interrupted") { model.preview(.interrupted, for: patient.id) }
                Button("Waiting") { model.preview(.waiting, for: patient.id) }
                Button("Corrected result") {
                    model.preview(.correctedResult, for: patient.id)
                }
                Button("Stale MDM") { model.preview(.staleMdm, for: patient.id) }
                Button("Separate discharge draft") {
                    model.preview(.dischargeDraft, for: patient.id)
                }
            }
            Button("Append synthetic chart categories") {
                let items = ContextCategory.allCases.map {
                    ContextItem(
                        category: $0,
                        text: "Synthetic \($0.rawValue) example — not clinical data",
                        source: "Synthetic fixture — EMR disconnected",
                        observedAt: Date(timeIntervalSince1970: 0),
                        needsReview: true,
                        isStale: true
                    )
                }
                model.replaceContext(patient.context + items, for: patient.id)
            }
            Picker("Generation preview", selection: $generationPreview) {
                Text("Unavailable").tag("Unavailable")
                Text("Offline").tag("Offline")
                Text("Failed").tag("Failed")
            }
            Text("Synthetic generation status: \(generationPreview). Note remains editable.")
        }
        #endif
    }
}
