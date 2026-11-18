import Foundation

/// Local FFI calls, including open/drop/close, use one serial queue.
/// Network calls use a separate queue so local saves remain responsive.
/// Rust additionally owns its runtime on its own command thread.
private final class RustSession: @unchecked Sendable {
    let queue = DispatchQueue(label: "app.ernote.rust-service", qos: .userInitiated)
    let networkQueue = DispatchQueue(label: "app.ernote.sync-network", qos: .utility)
    let config: DeviceConfig
    private var device: ErDevice?
    private var closed = false

    init(config: DeviceConfig) { self.config = config }

    func perform<T: Sendable>(_ action: @escaping @Sendable (ErDevice) throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            queue.async { [self] in
                do {
                    guard !closed else { throw FfiError.Closed }
                    if device == nil { device = try ErDevice(config: config) }
                    continuation.resume(returning: try action(device!))
                } catch { continuation.resume(throwing: error) }
            }
        }
    }

    func synchronize(config: SyncConfigDto, tokens: any AccessTokenSource, scheduled: Bool = false) async throws -> SyncStatusDto {
        let device = try await perform { $0 }
        return try await withCheckedThrowingContinuation { continuation in
            networkQueue.async {
                do {
                    let result: SyncStatusDto
                    if scheduled { result = try device.pollSync(config: config, tokens: tokens) }
                    else { result = try device.syncNow(config: config, tokens: tokens) }
                    continuation.resume(returning: result)
                }
                catch { continuation.resume(throwing: error) }
            }
        }
    }

    func close() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            queue.async { [self] in
                closed = true
                do {
                    defer { device = nil }
                    try device?.setListener(listener: nil)
                    try device?.close()
                    continuation.resume()
                } catch { continuation.resume(throwing: error) }
            }
        }
    }

    deinit {
        // ErDevice's final release can block while its owner thread drains.
        if let device { queue.async { try? device.setListener(listener: nil); try? device.close() } }
    }
}

private final class ChangeListener: DeviceChangeListener, @unchecked Sendable {
    let receive: @Sendable () -> Void
    init(receive: @escaping @Sendable () -> Void) { self.receive = receive }
    func invalidated(revision: UInt64) { receive() }
}

@MainActor
public final class RustDeviceService: DeviceService {
    public let availability = ServiceAvailability.localCore
    public let supportsRecording = false
    public let supportsGeneration = false
    public private(set) var snapshot = DeviceSnapshot()
    public var onChange: (() -> Void)?
    private let session: RustSession
    private var listening = false
    private var syncConfiguration: SyncConfigDto?
    private var tokenSource: (any AccessTokenSource)?
    private var syncTask: Task<Void, Never>?
    public var supportsSync: Bool { syncConfiguration != nil && tokenSource != nil }

    /// Auth setup supplies the server-verified intended membership. Opening the
    /// offline store still needs neither a token nor a network connection.
    public func configureSync(_ configuration: SyncConfigDto, tokens: any AccessTokenSource) {
        syncConfiguration = configuration
        tokenSource = tokens
        syncTask?.cancel()
        syncTask = Task { [weak self] in
            while !Task.isCancelled && self != nil {
                await self?.pollSynchronization()
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }
    private func pollSynchronization() async {
        guard let configuration = syncConfiguration, let tokens = tokenSource else { return }
        do {
            _ = try await session.synchronize(config: configuration, tokens: tokens, scheduled: true)
            snapshot = try await session.perform { try readSnapshot($0) }
            onChange?()
        } catch { /* The next foreground attempt retains the same durable work. */ }
    }
    public func synchronize() async throws {
        guard let configuration = syncConfiguration, let tokens = tokenSource else { throw DeviceServiceError.unavailable }
        _ = try await session.synchronize(config: configuration, tokens: tokens)
        snapshot = try await session.perform { try readSnapshot($0) }
    }

    public init(databaseURL: URL, accountScope: String) {
        session = RustSession(config: DeviceConfig(databasePath: databaseURL.path, accountScope: accountScope))
    }

    public func load() async throws {
        snapshot = try await session.perform { try readSnapshot($0) }
        if !listening {
            let listener = ChangeListener { [weak self] in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    do {
                        self.snapshot = try await self.session.perform { try readSnapshot($0) }
                        self.onChange?()
                    } catch { /* Keep acknowledged state; commands surface typed failures. */ }
                }
            }
            try await session.perform { try $0.setListener(listener: listener) }
            listening = true
        }
    }

    private func mutate(_ action: @escaping @Sendable (ErDevice) throws -> Void) async throws {
        snapshot = try await session.perform { device in
            try action(device)
            return try readSnapshot(device)
        }
    }
    public func close() async throws { syncTask?.cancel(); syncTask = nil; syncConfiguration = nil; tokenSource = nil; try await session.close() }
    public func startShift() async throws {
        try await mutate { _ = try $0.startShift(atMs: epochNow()) }
    }
    public func endShift() async throws { try await mutate { try $0.endShift(atMs: epochNow()) } }
    public func clearSelection() async throws { try await mutate { try $0.selectPatient(patientId: nil, atMs: epochNow()) } }
    public func createPatient(displayLabel: String, chiefComplaint: String) async throws -> UUID {
        let result = try await session.perform { device -> (String, DeviceSnapshot) in
            let id = try device.addPatient(complaint: chiefComplaint, room: displayLabel.isEmpty ? nil : displayLabel, atMs: epochNow())
            return (id, try readSnapshot(device))
        }
        snapshot = result.1
        guard let id = UUID(uuidString: result.0) else { throw FfiError.Internal }
        return id
    }
    public func selectPatient(_ id: UUID) async throws {
        try await mutate { try $0.selectPatient(patientId: id.uuidString, atMs: epochNow()) }
    }
    public func setConsent(_ consent: ConsentStatus, for id: UUID) async throws {
        // Read expected revision inside the same serial service operation.
        try await mutate { device in
            guard let patient = try device.snapshot().patients.first(where: { UUID(uuidString: $0.id) == id }) else { throw FfiError.NotFound }
            let choice: ConsentChoice
            switch consent { case .unknown: choice = .unknown; case .granted: choice = .granted; case .declined: choice = .declined; case .revoked: choice = .revoked }
            try device.setConsent(patientId: id.uuidString, status: choice, policyVersion: "synthetic-development-v1", expectedRevision: patient.consent.revision, atMs: epochNow())
        }
    }
    public func setCaptureMode(_ mode: CaptureMode, for id: UUID) async throws {
        try await mutate { try $0.setMode(patientId: id.uuidString, solo: mode == .soloDictation) }
    }
    public func replaceContext(_ context: [ContextItem], for id: UUID) async throws {
        // The shell currently adds/edits items only. Reject removal until it has
        // an explicit archive/delete interaction; never silently discard data.
        try await mutate { device in
            let existing = try device.chartContext(patientId: id.uuidString)
            guard existing.allSatisfy({ old in context.contains { UUID(uuidString: old.id) == $0.id } }) else { throw FfiError.InvalidInput }
            for item in context {
                let old = existing.first { UUID(uuidString: $0.id) == item.id }
                if let old, old.text == item.text, old.reviewed == !item.needsReview { continue }
                _ = try device.saveContext(input: ChartContextDto(
                    id: item.id.uuidString, patientId: id.uuidString, version: item.version,
                    category: bridgeCategory(item.category), text: item.text, source: item.source,
                    clinicalAtMs: item.observedAt.map(epoch), enteredAtMs: epoch(item.enteredAt), reviewed: !item.needsReview
                ), expectedVersion: item.version)
            }
        }
    }
    public func updateNote(_ text: String, for id: UUID) async throws {
        try await mutate { device in
            let revision = try device.snapshot().revision
            try device.saveManualDraft(patientId: id.uuidString, text: text, expectedRevision: revision)
        }
    }
    public func savePreference(_ preference: SavedPreference) async throws {
        try await mutate { device in
            let kind: DefinitionKindDto
            switch preference.kind { case .template: kind = .template; case .admissionMessage: kind = .admissionMessage; case .dischargeInstructions: kind = .dischargeInstructions; case .custom: kind = .custom }
            _ = try device.saveDefinition(input: DefinitionDto(
                id: preference.id.uuidString, version: preference.version, name: preference.section,
                kind: kind, instructions: kind == .template ? "" : preference.instructions,
                sections: kind == .template ? [SectionPreferenceDto(section: preference.section.lowercased(), instructions: preference.instructions)] : [],
                archived: preference.archived, isDefault: preference.isDefault
            ), expectedVersion: preference.version)
        }
    }
    public func recordObjective(_ input: ObjectiveInput, for id: UUID) async throws {
        try await mutate { try $0.recordObjective(patientId: id.uuidString, input: input, atMs: epochNow()) }
    }
}

private func epoch(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded(.down)) }
private func epochNow() -> Int64 { epoch(Date()) }
private func bridgeCategory(_ category: ContextCategory) -> ChartCategory {
    switch category { case .history: return .history; case .medication: return .medication; case .vital: return .vital; case .lab: return .lab; case .imaging: return .imaging; case .treatment: return .treatment; case .diagnosis: return .diagnosis }
}
private func appCategory(_ category: ChartCategory) -> ContextCategory {
    switch category { case .history: return .history; case .medication: return .medication; case .vital: return .vital; case .lab: return .lab; case .imaging: return .imaging; case .treatment: return .treatment; case .diagnosis: return .diagnosis }
}
private func readSnapshot(_ device: ErDevice) throws -> DeviceSnapshot {
    let state = try device.snapshot()
    let patients = try state.patients.map { patient -> PatientView in
        guard let id = UUID(uuidString: patient.id) else { throw FfiError.Internal }
        let note = try device.note(patientId: patient.id)
        let context = try device.chartContext(patientId: patient.id).map { item -> ContextItem in
            guard let id = UUID(uuidString: item.id) else { throw FfiError.Internal }
            return ContextItem(id: id, category: appCategory(item.category), text: item.text, source: item.source,
                observedAt: item.clinicalAtMs.map { Date(timeIntervalSince1970: Double($0) / 1000) },
                enteredAt: Date(timeIntervalSince1970: Double(item.enteredAtMs) / 1000), version: item.version, needsReview: !item.reviewed)
        }
        let consent: ConsentStatus
        switch patient.consent.status { case .unknown: consent = .unknown; case .granted: consent = .granted; case .declined: consent = .declined; case .revoked: consent = .revoked }
        let recorder: RecorderStatus
        switch patient.recording.status { case .paused: recorder = .paused; case .interrupted: recorder = .interrupted; case .pendingStop: recorder = .pendingStop; default: recorder = .unavailable }
        return PatientView(id: id, displayLabel: patient.room ?? "Unassigned room", chiefComplaint: patient.complaint,
            consent: consent, captureMode: patient.recording.solo ? .soloDictation : .ambient, recorder: recorder,
            context: context, note: note.manualDraft ?? "", generatedSections: [note.hpi, note.ros, note.exam, note.mdm, note.dispositionText].compactMap { $0 }, objectiveLines: note.results + note.medications + note.diagnoses)
    }
    let preferences = try state.definitions.filter { !$0.archived }.map { definition -> SavedPreference in
        guard let id = UUID(uuidString: definition.id) else { throw FfiError.Internal }
        let kind: PreferenceKind
        switch definition.kind { case .template: kind = .template; case .admissionMessage: kind = .admissionMessage; case .dischargeInstructions: kind = .dischargeInstructions; case .custom: kind = .custom }
        return SavedPreference(id: id, section: definition.name, instructions: definition.sections.first?.instructions ?? definition.instructions, isDefault: definition.isDefault, version: definition.version, kind: kind, archived: definition.archived)
    }
    let delivery = try device.syncStatus()
    return DeviceSnapshot(sync: SyncPresentation(status: delivery.status, queued: delivery.queuedEvents, awaitingConfirmation: delivery.awaitingDownload, conflicts: delivery.conflicts), shiftID: state.shift.flatMap { UUID(uuidString: $0.id) }, selectedPatientID: state.shift?.selectedPatientId.flatMap(UUID.init(uuidString:)), patients: patients, preferences: preferences, revision: state.revision)
}
