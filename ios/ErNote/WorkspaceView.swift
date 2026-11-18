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
                    case .localCore:
                        Text("Local core • saved on this device • synthetic data only")
                    }
                    Text(model.snapshot.sync.message)
                    if model.snapshot.sync.queued > 0 { Text("Waiting to send: \(model.snapshot.sync.queued)") }
                    if model.snapshot.sync.awaitingConfirmation > 0 { Text("Waiting for confirmation: \(model.snapshot.sync.awaitingConfirmation)") }
                    Text("Preferences and pasted context are saved on this device.").font(.footnote)
                    Button("Synchronize") { Task { await model.synchronize() } }.disabled(!model.supportsSync || model.isWorking)
                    Text("Recording, generation, and EMR connections are unavailable.")
                        .font(.footnote)
                    if let error = model.commandError {
                        Text(error).foregroundStyle(.red)
                    }
                }

                Section("Shift") {
                    if model.snapshot.shiftID == nil {
                        Button("Start shift") { Task { await model.startShift() } }
                    } else {
                        Text("Shift open")
                        Button("End shift") { Task { await model.endShift() } }
                        Button("Clear patient selection") { Task { await model.clearSelection() } }
                        TextField("Patient label", text: $patientLabel)
                        TextField("Chief complaint", text: $complaint)
                        Button("Add patient") {
                            Task {
                                let count = model.snapshot.patients.count
                                await model.createPatient(
                                    displayLabel: patientLabel,
                                    chiefComplaint: complaint
                                )
                                if model.snapshot.patients.count > count {
                                    patientLabel = ""
                                    complaint = ""
                                }
                            }
                        }
                        .disabled(
                            patientLabel.trimmingCharacters(
                                in: .whitespacesAndNewlines
                            ).isEmpty
                        )
                    }
                }.disabled(model.isWorking)

                Section("Patients") {
                    if model.snapshot.patients.isEmpty {
                        Text("Add a patient to begin.")
                    }
                    ForEach(model.snapshot.patients) { patient in
                        Button {
                            Task { await model.selectPatient(patient.id) }
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
            .task { await model.load() }
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
                    set: { value in Task { await model.setConsent(value, for: patient.id) } }
                )) {
                    Text("Unknown").tag(ConsentStatus.unknown)
                    Text("Granted").tag(ConsentStatus.granted)
                    Text("Declined").tag(ConsentStatus.declined)
                    Text("Revoked").tag(ConsentStatus.revoked)
                }
                Picker("Mode", selection: Binding(
                    get: { patient.captureMode },
                    set: { value in Task { await model.setCaptureMode(value, for: patient.id) } }
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
                ForEach(patient.objectiveLines, id: \.self) { Text($0) }
                ObjectiveEntry(model: model, patientID: patient.id)
                ForEach(patient.context) { item in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(item.category.rawValue.capitalized).font(.headline)
                        Text(item.text)
                        Text(item.source).font(.caption)
                        if let observed = item.observedAt { Text(observed, style: .date).font(.caption) } else { Text("Clinical time unverified").font(.caption) }
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
                        observedAt: nil,
                        needsReview: true
                    )
                    Task {
                        await model.replaceContext(patient.context + [item], for: patient.id)
                        if model.snapshot.patients.first(where: {
                            $0.id == patient.id
                        })?.context.contains(where: { $0.id == item.id }) == true {
                            contextText = ""
                        }
                    }
                }
                .disabled(
                    contextText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
            }
        default:
            Section {
                NoteEditor(model: model, patientID: patient.id)
                if !patient.generatedSections.isEmpty {
                    DisclosureGroup("Generated sections") {
                        ForEach(patient.generatedSections, id: \.self) { Text($0) }
                    }
                }
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
        if model.availability == .syntheticPreview {
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
                    Task { await model.replaceContext(patient.context + items, for: patient.id) }
                }
                Picker("Generation preview", selection: $generationPreview) {
                    Text("Unavailable").tag("Unavailable")
                    Text("Offline").tag("Offline")
                    Text("Failed").tag("Failed")
                }
                Text("Synthetic generation status: \(generationPreview). Note remains editable.")
            }
        }
        #endif
    }
}

@MainActor
private struct ObjectiveEntry: View {
    @ObservedObject var model: WorkspaceModel
    let patientID: UUID
    @State private var name = ""
    @State private var value = ""
    @State private var unit = ""
    var body: some View {
        DisclosureGroup("Add manual lab") {
            TextField("Lab name", text: $name)
            TextField("Value", text: $value).keyboardType(.numbersAndPunctuation)
            TextField("Unit", text: $unit)
            Button("Save lab") {
                guard let number = Double(value) else { return }
                Task { await model.recordObjective(.lab(name: name, value: number, unit: unit, abnormal: false), for: patientID) }
            }
            .disabled(name.isEmpty || value.isEmpty || unit.isEmpty)
        }
    }
}
