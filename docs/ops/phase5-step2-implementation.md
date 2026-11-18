# Phase 5 Step 2 — Durable local device core

Implementation record: 2026-11-17. Companion to the [Step 2 plan](../planning/phase5/phase5-part0-step1-step2-implementation-steps.md#5-step-2--durable-device-core-with-server-owned-generation), [Phase 5 roadmap](../planning/phase5/phase5.md), and [Step 1 record](phase5-step1-implementation.md).

Step 2 now supplies the offline Rust composition root and an owned UniFFI object. The phone shell uses the persistent adapter: a shift, ordered patient visits, selection, consent, mode, manual context, objective inputs, section preferences, saved prompts, and independent manual drafts survive reopening. It opens without a server, access token, or provider credentials.

The Rust implementation and Linux verification are distinct from Apple acceptance. Xcode compilation, simulator execution, XCFramework packaging with the SQLite graph, and a signed physical-phone run still require the M1/Mac environment. These are open gates, not implied passes. No AWS resources or server wire schemas were changed.

## Where this fits

The working application path is:

1. SwiftUI calls `WorkspaceModel` with an explicit patient/visit ID.
2. The asynchronous `DeviceService` implementation schedules blocking FFI work on a serial background `DispatchQueue`.
3. Generated Swift/C bindings call `ErDevice`, a `Send + Sync` handle.
4. A bounded Rust command channel delivers the operation to one owner thread.
5. `DeviceCore` validates scope, membership, lifecycle, consent, inputs, versions, and timestamps.
6. SQLite acknowledges the write before the core updates its projection and returns success.
7. A separate dispatcher emits a coalesced full invalidation. Swift reads current views and publishes them on `MainActor`.

Swift owns platform/UI behavior. Rust owns clinical commands and persistence. `er-domain` still owns the pure note state machine, `er-store` owns SQLite, and `er-app` supplies event publication, replay/loading, shared validation, and export. `er-device` composes these without UniFFI or AI dependencies. `er-ffi` owns the foreign DTOs and conversions.

There is no `RemoteRepository` substituted for the local store. Step 3 will add synchronization beside this durable state. Step 4 will add actual hardware/chunks. Step 7 will deliver generated content through server events.

## New files

| File | Purpose and reason |
|---|---|
| `crates/er-device/Cargo.toml` | Introduces the offline composition crate. Depends on `er-app` without `local-generation`; its `test-support` feature permits an injected capture adapter only for Rust harnesses. |
| `crates/er-device/src/lib.rs` | Public core/handle/read-model surface and fixed `DeviceError` categories. Converts internal failures without exposing SQL, paths, clinical text, or prompt bodies. |
| `crates/er-device/src/core.rs` | Scoped open, ownership, metadata validation, note rehydration, commands, consent/capture lifecycle, context, definitions, review/sign, and existing-format export. Keeps a failed stop's event ID/body available for retry. |
| `crates/er-device/src/handle.rs` | Dedicated command owner, runtime lifetime, bounded admission, command panic containment, callback dispatcher, and explicit close. Separates callback execution from persistence. |
| `crates/er-device/src/views.rs` | Plain, flat read models, including typed clinical phases, consent, recording/recovery, objective provenance, context, and definitions. No domain state objects or raw events leave the core. |
| `crates/er-device/src/tests.rs` | File-backed lifecycle, wrong-patient/stale-editor validation, capture policy, failure injection, actual process termination, server-event fixtures, and thread/lifetime coverage. |
| `crates/er-store/src/device.rs` | Typed versioned metadata and transactional scope binding/compare-and-save. Optional capture boundary and metadata commit together. No general SQL method is exposed through FFI. |
| `crates/er-store/src/device_tests.rs` | Legacy-schema preservation, repeated migration, migration rollback, future-schema rejection, scope/CAS isolation, duplicate event validation, and real SQLite disk-full rollback. |
| `crates/er-app/src/generation.rs` | Provider-neutral policy plus dependency-free `RemoteGeneration`. The feature-gated adapter for existing `Summarizer` implementations preserves local CLI behavior. |
| `crates/er-app/src/validation.rs` | Shared signed-note and capture-phase command checks. Tolerant replay remains separate from validating new commands. |
| `crates/er-app/tests/remote_policy.rs` | Runs with `--no-default-features`. A transcriber that panics if called proves that server-owned stops work before an upload exists. |
| `crates/er-ffi/src/device.rs` | The `ErDevice` object, explicit records/enums, typed objective inputs, listener adapter, core conversions, and interface version 2. There is no production capture or raw-event-injection export. |
| `ios/ErNoteKit/Sources/ErNoteKit/RustDeviceService.swift` | Real asynchronous service, serial FFI queue, weak notification adapter, view conversion, and explicit shutdown. No fixture fallback after storage errors. |
| This document | Decisions, data flows, verification limits, and Step 3 handoff. |

## Changes to existing files

| Area | Changes |
|---|---|
| Workspace/manifests/lockfile | Adds `er-device`; sets the workspace `er-app` dependency to disable defaults. `er-cli` and `er-server` explicitly request `local-generation`. `er-ai` is optional in `er-app`. UniFFI and all third-party version resolutions remain pinned as before. |
| `er-app/src/shift_manager.rs` | Uses explicit `GenerationOwnership` and a neutral policy bound. Keeps per-encounter upload handoff, removes its marker only after a successful stop, validates editable/capture phases, and ends an empty/already-ended shift correctly. |
| `er-app/src/encounter_driver.rs` | Adds independent transcription ownership. Switch follows that policy. Start failure releases capture; stop failure retains the stopped resource's pending event and stable ID. |
| `er-app/src/event_bus.rs` | Adds stable-ID publication for stop retries, rebuilding the projection to avoid duplicate application. Snapshot failure diagnostics no longer include arbitrary error text/patient IDs. |
| `er-app/src/error.rs`, `lib.rs` | Feature-gates AI-specific errors; exposes typed unsupported operation and the new modules. |
| Existing CLI/application generation tests | Constructor/policy changes preserve local summary/MDM/de-identification regressions. The local-generation integration target is explicitly feature-gated. |
| `er-store/src/db.rs` | Transactional schema version 1 migration, legacy preservation, table-shape validation, WAL, foreign keys, 1.5-second busy timeout, and `synchronous=FULL`. |
| `er-store/src/repository.rs` | Validates duplicate event bodies/patients, preserves original sequence IDs, removes sensitive snapshot diagnostics, and rebuilds when a snapshot cannot be decoded. |
| `er-store/src/error.rs`, `lib.rs` | Typed metadata conflict/schema/scope/integrity failures and metadata exports. |
| CLI error presentation and server telemetry | Exhaustively classify new store errors using fixed categories. No HTTP DTO or OpenAPI change. |
| `er-ffi/src/lib.rs` | Attaches the object, adds conflict/scope/recovery errors, and asserts the object's thread-safety. Retains Step 1's sanitized panic hook and unwind guards. |
| Generated `RustBridge.swift`, `RustCoreFFI.h` | Regenerated from the matching production library. Generated code must never be edited independently of Rust. The module map's contents are unchanged. |
| Swift `DeviceService.swift` | Async commands, local-core mode, revoked consent, pending-stop presentation, context versions/optional clinical time, preference versions/kinds, objective/generated display data, and cache revision. |
| Swift `WorkspaceModel`, `WorkspaceView`, `NoteEditor`, `PreferencesView`, `ErNoteApp` | Open protected local storage; use the real service in Debug and Release; retain failed text drafts; show typed-error guidance, provenance, definition kind, manual lab entry, and unavailable audio/generation/EMR. Fixture controls appear only with the fixture service. |
| Swift tests | Existing model tests await asynchronous commands. Hosted bridge tests cover real service reopen and stateful panic/close/reopen using matching isolated test bindings. |
| Bridge scripts | Linux checks include core tests and no-default application tests. Dependency checks reject `reqwest`, local-generation, and test capture support. Generated optimized Python calls now exercise the actual object and rich objective data. |
| `ARCHITECTURE.md` and Phase 5 planning/setup records | Link this implementation and distinguish source completion from the outstanding Apple/device acceptance. |

The user's existing `.gitignore` change was left intact.

## Durable storage and identity

Schema version is stored in SQLite `user_version`. Version 0 means the existing events/snapshots layout; upgrading adds `device_scope` and `device_state` in one transaction without replacing events or snapshots. Version 1 is the supported layout. Newer versions fail explicitly. Table names and required columns are checked, including during migration, so a conflicting view/table causes rollback rather than a partly accepted schema.

The metadata payload has its own `format_version = 1` and revision. Unsupported or malformed authoritative payloads fail with recovery required. Future metadata migrations must explicitly transform the old payload and advance its version transactionally; optional new fields must not become an accidental substitute for an upgrade policy.

The initial implementation stores one typed metadata aggregate in a singleton row:

- All shifts, with stable local ID, optional future server-shift ID, start/end, selection, and ordered visits. Starting another shift retains prior ended shifts.
- Patient records/room labels. A patient-note ID represents this ED visit; it does not identify a person across separate visits. Array order is roster order.
- Per-visit consent history and mode preference.
- Per-segment source, mode, consent revision, start and confirmed stop. Recorder/recovery presentation is separate from `NotePhase` and selection.
- Manual context and independent manual draft text.
- Immutable template/prompt versions, including default/archive state.
- The last control-command timestamp.

The opaque account/organization scope binds the database on first open. Reusing it with another scope fails. The application currently supplies the explicit synthetic-development scope; authenticated identity selection belongs to Step 3/5. This is local isolation, not server authorization or a consent-policy approval.

An OS file lock is held on the actual database inode for the core's lifetime. A second core, including one using an alias to that file, cannot maintain another live projection. SQLite's writer lock alone would not provide that guarantee.

SQLite remains in the phone-owned protected Application Support directory supplied by `ProtectedStorage`. WAL/SHM siblings live there as well; the clinical database stays outside the App Group. Existing directory protection/backup setup is reused. Physical protection, complete retention, corruption/pressure drills, and hardware verification remain Step 9 work.

### Transaction choices and failure boundaries

Ordinary clinical commands follow validation → append → projection → notification. The existing snapshot/checksum loader is reused, with authoritative event decoding checked on reopen even if a snapshot exists. Invalid snapshots rebuild; unreadable authoritative data fails. Registered patients with no events are restored from metadata.

Metadata changes stage a clone and use an expected revision. The SQLite save commits before replacing live state. A failed add never creates an in-memory-only patient. Stale editors cannot overwrite acknowledged context/definition versions.

Capture start/stop uses a stronger narrow transaction: the event and its segment/consent/source metadata commit together. There is no partially committed capture attribution to repair between two tables. After commit, a projection panic faults the owner and requires close/reopen; it is not reported as a safe retry with a new ID.

Switch/end remains two stages: stop the old segment, then save selection/end. If the second stage fails, the old patient remains selected/the shift remains open, with capture stopped. A retry completes the metadata action without restarting capture. These two stages are deliberately not described as atomic.

## Capture, consent, and recovery

Production Swift recording methods are absent. `supportsRecording` is false; no microphone permission is requested, and no real-core running timer is shown. Rust tests inject a `CaptureAdapter`; the production adapter is unavailable.

The Rust start/resume policy requires an open shift, an explicitly selected member visit, a permitted phase (`Registered`, `Idle`, or `Finalizing`), an available adapter, and no conflicting or unresolved segment. Ambient additionally requires that visit's latest granted consent. Unknown, declined, and revoked never authorize ambient. Solo requires an explicit mode choice, and uses `CaptureMode::Dictation`; consent decline does not automatically change modes.

Consent entries retain actor scope, policy version, recorded UTC time, and monotonically increasing revision. The segment records the revision and mode at start. A non-granted consent action stops an active segment before attempting the consent write, including an explicitly active solo segment as a conservative stop boundary. Ambient is locally latched ineligible after a failed revocation save until an explicit granted choice is durably recorded.

Pause closes the current snippet without ending the visit or changing selection. Resume creates a new encounter UUID. `PendingSignOff` and `Signed` reject resume; a `Finalizing` re-evaluation preserves the existing MDM. There is no new clinical `Paused` phase and no unassigned recording buffer.

If start persistence fails, the adapter releases its capture resource. If stop persistence fails, hardware has stopped and the core reports `PendingStop`, retaining the same pending event ID/body for retry. New conflicting mutations are blocked. If the process dies first, the last acknowledged metadata/event pair still describes an unfinished segment, so reopen shows `Interrupted` and blocks capture. It never extends the end time to launch time, authors a transcript, or starts the microphone.

`reconcile_interrupted` is Rust-only in this step and requires a known stop boundary. Step 4 must derive that evidence from the hardware/file manifest and then expose the supported recovery action. A failed, unacknowledged revocation intent cannot be made durable when storage refuses all writes; interruption reconciliation and an explicit consent decision are required operationally after such a crash.

All device stop paths produce zero local `TranscriptReady`, `EncounterSummarized`, `MdmGenerated`, or `TriggerMDM` events. The device core has no transcription/generation operation. The shared driver separately supports server-owned STT before any upload, while preserving the CLI's individual upload handoff and local-generation behavior.

## Context, objective data, and preferences

Manual context is bound to an explicit visit and category: history, medication, vital, lab, imaging, treatment, or diagnosis. It carries clinician-supplied source, optional clinical time, entered time, review state, and expected version. Unknown clinical time stays absent; entering a record does not mark it reviewed or included in MDM. Context updates do not change the clinical aggregate.

Structured labs/imaging/vitals, medication, diagnosis, data reviewed, and disposition reuse existing domain events. Inputs identify their target patient; changing selection while a form is open cannot redirect the submission. Numeric results must be finite, fields bounded, identifiers valid, and signed/ended visits reject writes before append.

`NoteDto` includes flat sections, independent disposition text, formatted objective display lines, objective IDs/category/source/clinical time where available, encounter/audit counts, projection version, and recovery state. Result source comes from the aggregate. Medication/diagnosis source provenance is unspecified because their current domain models do not establish it; diagnosis clinical time remains absent. The bridge does not manufacture external identifiers.

Templates have section preferences; saved prompts have an allowed output kind and bounded instructions. The shared storage/API type is a discriminated definition, with `Template`, `AdmissionMessage`, `DischargeInstructions`, and `Custom` kinds. Stable IDs persist across edits; each save appends an immutable version. Duplicate creates another ID; restore copies an earlier version into a new version. Archive/default changes are new versions too. Selecting a default also appends deselection versions for other defaults of that kind, atomically. Referenced old bodies are never erased.

The phone editor currently edits a single-section template or a saved-prompt definition; the bridge supports a full multi-section template. Definitions store no generated output. Changes leave the clinical note unchanged. Prompt execution, pinned generation inputs, derived-document storage, and accepted MDM candidates remain Step 7 contracts.

The existing free-text editor now saves `manual_draft`, a separate local scratch draft with expected metadata revision. It does not impersonate server-authored generated sections and is excluded from the established note export. This preserves the user's local writing without inventing an unsupported server event. Step 3/7 must define the clinician-edit sync/revision contract before this text participates in authoritative note editing/export. The UI labels this distinction.

## Boundary and lifecycle contract

The supported production `ErDevice` methods cover open/close; snapshot and shift history; start/end/read current shift; add/select/clear visits; consent/mode; note/context reads; context/manual-draft saves; definition save/history/duplicate/restore; objective/medication/diagnosis/reviewed-data/disposition entry; explicit-patient review/sign/export; and listener registration. Reading the existing open shift is the resume operation; starting another open shift conflicts.

There are no production methods for audio push/start/pause/resume, network sync, tokens, jobs, MDM generation, EMR refresh, prompt execution, reassignment, or arbitrary event injection. The isolated `ffi-test` artifact adds stateful panic injection and remains separate from production sources/binaries.

| Boundary detail | Contract |
|---|---|
| IDs | Non-nil UUID strings. New local shift/patient/encounter/event/definition/context IDs use UUIDv7; upload/sync must reuse them. |
| Time | Checked signed UTC epoch milliseconds. No pre-epoch/further-than-5-seconds-future command timestamps. Control operations cannot precede the last acknowledged control/boundary; patient creation/capture cannot precede its containing shift/visit. This is not delayed-watch attribution reconciliation. |
| Ordering | Visits preserve insertion order; definitions are listed by stable UUID. Definition history is append/version order. Context preserves insertion order. |
| Revisions/counts | `u64` maps to Swift `UInt64`, never JavaScript numeric cursors. Position/encounter count uses `u32`. Expected versions are checked in Rust. Metadata revision and note projection version have different meanings. |
| Optional strings | `None` means absent. Empty manual draft/instructions can be intentional; nonempty names, sources, room labels if supplied, and context text are required. |
| Limits | UTF-8 bytes: scope 256; path 4096; complaint 1024; room/policy/name 128; source 256; context text 16,384; definition instructions 8192 per field; manual draft 65,536. At most 512 context items/visit and five unique recognized sections/template. Structured-input limits are validated by field in `core.rs`; NaN/infinity are rejected. NUL is rejected. |
| Errors | Fixed `InvalidInput`, `InvalidState`, `NotFound`, `Conflict`, `Scope`, `Storage`, `Recovery`, `Closed`, `Busy`, `Internal`. No error carries an implementation payload. Storage can represent read-only, lock, full, or driver failures; recovery identifies authoritative/schema problems. |
| Diagnostics | Interface version 2 plus matching generated bindings/library. No arbitrary binding compatibility promise. Panic payloads and raw persistence/snapshot errors are excluded from phone diagnostics. |

The owner creates, runs, and drops its current-thread Tokio runtime on one dedicated Rust thread. A 64-entry channel bounds admission. A full queue returns `Busy`; admission after close/fault is rejected. Accepted commands have no caller-side timeout that could return failure while a mutation later commits. A panic returns `Internal`, faults the handle, releases owner resources, and resolves waiting callers through channel closure if necessary.

Notifications use a separate one-entry channel and monotonic dispatcher revision. Every message is a full invalidation, so intermediate notifications can coalesce without dropping clinical work. Read views are authoritative. This is intentionally simpler than fine-grained patient/topic subscriptions at the first local milestone.

Callbacks execute outside database/projection locks and can re-enter reads. Slow listeners do not block commits. `close` stops admission, drains accepted work, releases the core, clears the listener, and stops notification delivery. It is idempotent and supports being called from a callback without self-joining. External close waits for an already-running callback to return; foreign callbacks must return promptly and dispatch UI work asynchronously. Swift's listener captures the service weakly, preventing a service/device/listener retain cycle.

## Example flows

### A. Two persistent visits without a backend

1. `ProtectedStorage` supplies `ClinicalData/device.sqlite`; the service opens it on its queue with the synthetic scope.
2. `startShift` creates local shift `S1`. No server shift is invented.
3. Add Room 4 → `A`, then Hall 3 → `B`. The metadata transaction retains `[A, B]` even though neither has note events.
4. Select `A`; record declined consent revision 1; explicitly choose solo.
5. Add manual history with missing clinical time and `reviewed=false`. Enter a structured potassium lab for `A`; only the latter publishes `ResultReceived(A)`.
6. Save HPI template version 1 and admission prompt version 1. Save a separate manual draft.
7. Reopen: scope/metadata validate; notes load by snapshot/delta/full replay. `S1`, `[A, B]`, selection, consent, context, preferences, and draft survive. The recorder remains unavailable, and nothing claims to have synced.

### B. Pause, another patient, and return

Using the Rust test adapter: grant ambient consent for `A`; start `A1`; pause for results at the confirmed boundary; select `B`; explicitly use solo for `B1`; stop; select `A`; start `A2` with a new encounter ID. `A1` and `A2` belong to the same note/visit. The wait has no audio window. Each segment pins its mode/source/consent revision. The only authored capture events are ordered `AudioStarted`/`AudioStopped` pairs.

### C. A form completes after selection changes

A lab form retains `patient_id=A`. The clinician selects `B` before saving. `record_objective(A, ...)` validates membership and writes `ResultReceived(A)`. It never reads selection to choose the target. An unknown visit fails before append; a signed `A` fails before append. Selection/capture on `B` stays unchanged.

### D. Stop succeeds, selection save fails

The adapter stops `A1`; SQLite commits `AudioStopped(A1)` plus its paused metadata. Saving selection `B` fails. The caller gets a storage error, the last selection remains `A`, and the recorded state is paused. Reopen reproduces this state. Retrying selection saves `B` without manufacturing another stop or starting new audio.

### E. Server consequences are projected, not fabricated by the phone

A Rust-only test seeds transcript, summary, MDM, and transition events through the repository. Reopen projects them into `NoteDto`; explicit-patient review/sign commands validate and append real clinician transition events. Existing plain-text formatting and transcript fallback produce export. Production Swift has no seed/injection API, and job receipts alone cannot substitute for note content when Step 7 connects the server loop.

## Tradeoffs and growth path

- **One metadata aggregate:** makes initial membership/selection/consent/definition transactions straightforward and retains old shifts. Writes clone and serialize the aggregate, so cost grows with retained history and immutable versions. Before long retention/large accounts, introduce a schema migration to per-shift/context/definition-version rows, preserving stable IDs and the typed store API. Do not delete old shifts to hide this cost.
- **One serialized owner:** provides deterministic ordering and avoids nested-runtime/blocking-FFI problems. Long work would block subsequent commands; networking, provider calls, audio processing, and future sync scheduling must use bounded background work with explicit durable results rather than run on this thread.
- **Full invalidation:** keeps notification correctness simple and allows coalescing. The current Swift refresh reads each visible visit's note/context; this is acceptable for the first local slice but O(roster size). Add batched views/topic revisions when measurements justify them.
- **Strong initial durability:** FULL synchronization and persist-first publication favor acknowledged-write safety over maximum throughput. Measure actual device latency/battery before relaxing this.
- **No speculative wire additions:** consent, context, definitions, and scratch drafts remain local. This avoids claiming compatibility with existing endpoints; Step 3/7 must add their explicit contracts.
- **Source-qualified chart integration later:** room labels never become EMR identity. Future external mappings must name their source and be verified separately from this visit ID.
- **Functional shell before visual polish:** the supplied dashboard/active-encounter/review mockups informed roster ordering, room labels, explicit recorder state, repeated visit segments, flat sections, and objective provenance. No colors, waveform behavior, watch connectivity, unassigned-audio suggestions, or live generation were inferred from the pictures. Native semantic styling remains compatible with later light/dark work in Phase 6.

## Verification and remaining acceptance

Linux verification uses the pinned Rust 1.97.1 and UniFFI 0.32.2. The repository commands are:

```bash
cargo fmt --all -- --check
cargo test --locked -p er-device -p er-ffi
cargo test --locked -p er-app --no-default-features
cargo clippy --locked --workspace --all-targets -- -D warnings
scripts/check-device-deps.sh
scripts/check-bridge-linux.sh
scripts/ci-services.sh
```

The service gate runs the full workspace and server/worker regressions against disposable Postgres/MinIO/ElasticMQ with synthetic keys, plus OpenAPI consistency, online SQLx freshness/stale-metadata rejection, and missing-service readiness checks. The ordinary local-generation regressions still pass. The optional ignored load test is not enabled.

| Gate | Session result |
|---|---|
| Formatting and whitespace | Pass |
| File-backed device tests | 12 pass |
| Store migration/integrity/full-database tests | Five pass |
| FFI unit tests, production and isolated test feature | Six pass in each configuration |
| No-default application tests, including remote ownership | Pass |
| Workspace Clippy with warnings denied | Pass |
| Disposable service/workspace/OpenAPI/SQLx/readiness gate | Pass; final device-only recovery/read-model changes also rechecked with targeted tests and workspace Clippy |
| Both Apple-target dependency graphs | Pass; this inspects dependencies without compiling/linking Apple binaries |
| Production binding drift, artifact policy, optimized generated foreign lifecycle | Pass |
| Swift/Xcode, bundled-SQLite Apple builds, simulator and physical iPhone | Not run on this Linux host; open |

Current local evidence includes 12 file-backed device scenarios, five store scenarios, the isolated remote-policy suite, existing application/CLI/generation regressions, and bridge thread-safety/sanitization tests. The device capture matrix spans both modes and pause/switch/clear/revoke/end/close; other scenarios cover interrupted reopen, rejected phases, prior MDM preservation, stale/wrong-visit edits, failed start/stop/selection, revocation under write failure, disk full, migration rollback, bounded admission, blocked/re-entrant listeners, callback close, and process kill after acknowledgment.

The generated foreign-language smoke uses the optimized host library with matching isolated panic-test bindings. It tests open/reopen, Unicode, absent fields, consent/mode, lab/imaging/vital/medication/diagnosis/disposition, manual context, definitions, typed stateful panic, fault rejection, and durable reopen. Synthetic private sentinels must be absent from its output. This supplements the checked-in Swift hosted tests; it cannot prove Swift/Xcode behavior.

Ephemeral logs from this session are `/tmp/er-note-phase5-step2-services.log`, `/tmp/er-note-phase5-step2-bridge.log`, and `/tmp/er-note-phase5-step2-deps.log`. They are local evidence, not hosted CI results or durable release artifacts.

Apple target compilation with bundled SQLite, matching XCFramework construction, simulator Debug/Release tests, optimized stateful panic behavior in Swift, and signed physical-device persistence remain open. No Swift/Xcode toolchain is installed on this Linux host. Run `scripts/check-bridge-apple.sh` on the M1 with the recorded toolchain configuration, then perform the physical two-visit/consent/context/preference relaunch drill and record the result in [ios-development.md](ios-development.md).

## Current Phase 5 status and Step 3

| Work | State |
|---|---|
| Part 0 shell/tooling sources | Present; actual Apple environment, enrollment/signing, and hardware acceptance remain separate. |
| Step 1 bridge sources/Linux verification | Present. Step 2 expands the interface to version 2; regenerate and package matching binaries on the Mac. |
| Step 2 local core/bridge/phone adapter | Implemented, with Linux tests and service regressions; Apple/device execution gate remains open. |
| Step 3 sync | Next implementation step; no network/outbox/cursor is claimed here. |
| Step 4 audio | Deferred; Rust policy tested through adapters, production hardware commands absent. |
| Step 5 UI | Local shell wired; full workflows, authenticated scope, accessibility acceptance, and final navigation still follow. |
| Steps 6–10 | Watch, server customization/results, reassignment, device resilience, and distribution gates remain open. |
| Phase 4 live AWS / EMR discovery | Independent tracks; no live acceptance or EMR connectivity established by Step 2. |

Step 3 should proceed in these reviewable slices:

1. **Durable outbox and account lifecycle.** Add migrations for pending event/registration intents, acknowledged IDs, server sequence mappings, downloaded pages, and per-visit cursors. Build authenticated scope/token-provider setup around the owned handle. Close the prior account's core and explicitly handle unsynced work before switching accounts.
2. **Shift/patient registration.** Map stable local shift `S1` to the server-generated shift ID. Recover uncertain starts through the active-shift endpoint. Register patients before their events, reusing original patient IDs. Do not add a fictitious client-shift field to the current request.
3. **Ordered reconciliation.** Preserve event IDs across retries; commit acks, server ordering, downloaded pages, and cursor movement atomically. Replay acknowledged events in server order, then the pending local tail. Invalidate/rebuild local-sequence snapshots when that order changes. An HTTP success or job receipt is not a durable projected note.
4. **Explicit metadata contracts.** Decide and test consent, context, preference, and clinician-draft synchronization. Current DTOs/events do not already carry these fields. Extend `er-api`/server migrations/OpenAPI deliberately and define author/scope/version checks. Keep server execution/candidate/derived-document contracts in Step 7.
5. **Budgeted scheduler.** Use the existing `ServerApi` client feature with a verified Apple TLS graph. Handle 401 refresh, 429 Retry-After/backoff, uncertainty, partial batches, and bounded retries. Respect 60 general requests/minute, five MDM/minute, and 30 audio intents/minute; prioritize active visits/jobs instead of polling every visit every ten seconds.
6. **Offline/retry tests and UI truth.** Test uncertain writes, partial acknowledgment, duplicate/reordered pages, crash between ack/cursor updates, account mismatch, stale local commands, and scope isolation. Keep unsynced/failed/pending visible. Networking and synchronization cannot turn unavailable audio/generation into fixture success.

Step 3 can use local services and development auth. It does not wait for accepted AWS staging, real audio, a watch, or Epic. Its design must preserve the boundaries and identities established here.
