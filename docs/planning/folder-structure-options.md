# Folder Structure Options

*2026-06-10 — analysis of how to reorganize the repo for the remaining roadmap (Phases 3–8) and product scaling.*

## Why reorganize now

The current layout is a flat `src/` with 17 top-level modules plus `models/`, `conversions/`, and `tests/`. It has served Phases 0–3 well, but several forces on the roadmap will strain it:

1. **The FFI boundary (Phase 4).** UniFFI bindings need a crisply defined "core API" surface. Today that surface is smeared across `shift_manager.rs`, `state.rs`, and `events.rs`. A Swift shell linking the whole crate — including the CLI, mock drivers, and spike binaries — is wasteful and confusing.
2. **Platform-conditional dependencies (Phases 3–4).** `whisper-rs`, `cpal`/AVAudio FFI, and SQLCipher are heavy, platform-specific deps. Feature flags on one crate work, but they get tangled fast; separate crates isolate them cleanly.
3. **Integration adapters (Phases 6–8).** FHIR read, FHIR write, and HL7 are each substantial subsystems with their own HTTP clients, resource mappers, and retry/queue logic. Dumping them as `fhir.rs` siblings of `db.rs` will recreate the flatness problem at 3× the size.
4. **Compile times and test isolation.** Event-sourced cores want fast, pure unit tests. AI/network code wants mocked integration tests. Today everything rebuilds together.
5. **Doc sprawl.** ~14 markdown files (roadmaps, pricing, feedback, architecture) sit at repo root alongside `Cargo.toml`. None of them are code-adjacent.

All three options below share two uncontroversial moves:

- **`docs/`** — move all non-CLAUDE.md markdown into `docs/` (subfoldered: `docs/architecture/`, `docs/product/`, `docs/planning/`).
- **`tests/`** — promote `src/tests/` integration tests to Cargo's native `tests/` directory so they compile against the public API only (which is exactly what the FFI layer will consume — free API-surface validation).

---

## Option 1 — Layered Single Crate (Clean-Architecture-Lite)

Keep one crate; reorganize `src/` into horizontal layers by *technical responsibility*: domain at the center, infrastructure at the edge, application services between.

```
er-note-app/
├── Cargo.toml
├── docs/
│   ├── architecture/        # ARCHITECTURE.md, repository-architecture.md, state_pattern.md
│   ├── product/             # product-summary.md, pricing-strategy.md, doctor-feedback-*.md
│   └── planning/            # phase2.md, phase3.md, roadmap-*.md, creative-improvements.md
├── src/
│   ├── lib.rs
│   ├── domain/              # pure — no I/O, no tokio, no reqwest
│   │   ├── mod.rs
│   │   ├── note.rs          # PatientNote aggregate (from models/note.rs)
│   │   ├── encounter.rs
│   │   ├── patient.rs
│   │   ├── shift.rs
│   │   ├── result.rs        # ClinicalResult / labs
│   │   ├── disposition.rs
│   │   ├── audit.rs
│   │   ├── state.rs         # NoteState trait + phase structs (state machine only)
│   │   ├── events.rs        # NoteEvent enum + constructors (NOT EventBus)
│   │   └── replay.rs        # apply_event, replay — pure functions
│   ├── application/         # orchestration — owns the use cases
│   │   ├── mod.rs
│   │   ├── shift_manager.rs
│   │   ├── encounter_driver.rs
│   │   ├── event_bus.rs     # EventBus split out of events.rs (it does I/O)
│   │   ├── load.rs          # load_patient_note, PatientNoteStore
│   │   └── conversions/     # CLI/domain conversions
│   ├── infrastructure/      # everything that touches the outside world
│   │   ├── mod.rs
│   │   ├── persistence/     # repository.rs, db.rs, snapshots
│   │   ├── ai/              # claude_summarizer.rs, summarizer.rs (trait), deidentify.rs
│   │   ├── audio/           # audio.rs trait + mocks; later cpal/AVAudio bridge
│   │   ├── stt/             # stt.rs trait + mocks; later WhisperTranscriber
│   │   └── emr/             # (future) fhir_read/, fhir_write/, hl7/
│   ├── interface/           # delivery mechanisms
│   │   ├── mod.rs
│   │   ├── cli.rs
│   │   └── export.rs        # note formatting is a presentation concern
│   ├── error.rs
│   └── main.rs
├── src/bin/spike_summarizer.rs
└── tests/                   # crash_recovery.rs, persistence_integration.rs
```

**Key rule enforced by review (not the compiler):** `domain/` imports nothing from `application/` or `infrastructure/`. The dependency arrow points inward only.

### Pros
- **Lowest migration cost.** Pure `git mv` + `mod` path updates; no Cargo.toml surgery, no version juggling. One afternoon, zero risk to the working test suite.
- **Reveals the architecture you already have.** The code is *already* layered (traits in domain, impls at the edge — `Summarizer`/`MockSummarizer`, `Repository`/`SqliteRepository`, `Transcriber`/`MockTranscriber`). This makes the existing discipline visible and navigable.
- **Forces the one overdue split:** `events.rs` currently mixes the pure `NoteEvent` enum with the I/O-performing `EventBus`. Phase 4's FFI work needs that split anyway.
- **Single crate keeps iteration fast** at the current 5K LOC — no cross-crate `cargo` overhead, one test command, simple CI.
- **Easy to evolve into Option 2 later** — the layer folders become crate boundaries almost 1:1.

### Cons
- **Boundaries are conventions, not guarantees.** Nothing stops a future `domain/note.rs` from importing `rusqlite`. In a medical app where "domain stays pure for deterministic replay" is a correctness property, compiler-enforced boundaries have real value you're leaving on the table.
- **Feature-flag tangle remains.** `whisper-rs`, SQLCipher, and UniFFI all land as features on the *same* crate; `cargo test` on a dev machine has to navigate `--features`/`--no-default-features` combinations.
- **FFI surface stays implicit.** Phase 4 binds against "whatever is `pub` in the crate," which includes CLI helpers and mocks unless carefully re-exported.
- **Doesn't help compile times** as AI/EMR/STT code grows — every change rebuilds everything.

### Best if
You want immediate clarity with minimal disruption, expect to stay solo through Phase 4, and are comfortable deferring hard boundaries until the iOS work actually starts.

---

## Option 2 — Cargo Workspace, Crates by Architectural Layer

Split the project into a workspace of small crates whose dependency graph *is* the architecture. This is the standard Rust shape for "library core + multiple frontends" — exactly what Phases 4–8 demand (CLI frontend, iOS FFI frontend, maybe a server later per `approck-web-architecture.md`).

```
er-note-app/
├── Cargo.toml               # [workspace] members, shared deps via workspace.dependencies
├── docs/                    # same as Option 1
├── crates/
│   ├── er-domain/           # PatientNote, state machine, NoteEvent, replay/apply_event
│   │   └── src/             # deps: serde, uuid, chrono, thiserror ONLY
│   ├── er-store/            # Repository trait, SqliteRepository, db schema, snapshots,
│   │   └── src/             # EventBus, PatientNoteStore, load_patient_note
│   ├── er-ai/               # Summarizer trait, ClaudeSummarizer, deidentify, prompts
│   │   └── src/             # deps: reqwest, tokio
│   ├── er-pipeline/         # AudioCapture + Transcriber traits, mocks, EncounterDriver;
│   │   └── src/             # later: whisper feature lives HERE, not on the core
│   ├── er-app/              # ShiftManager, conversions, export — the use-case layer
│   │   └── src/             # the ONLY crate the frontends below talk to
│   ├── er-cli/              # main.rs, cli.rs, shift.json handling, spike_summarizer bin
│   │   └── src/
│   ├── er-ffi/              # (Phase 4) uniffi .udl + thin wrappers over er-app
│   │   └── src/
│   └── er-emr/              # (Phase 6+) FhirClient, resource mappers, sync task, HL7
│       └── src/
└── tests/  → moves into the crate that owns each test (er-store gets persistence
            tests, er-app gets crash_recovery)
```

Dependency graph (arrows = `depends on`):

```
er-cli ──┐
er-ffi ──┼──► er-app ──► er-domain
         │      ├──────► er-store ──► er-domain
         │      ├──────► er-ai ─────► er-domain
         │      └──────► er-pipeline ► er-domain
er-emr ──┴──────────────────────────► er-domain
```

### Pros
- **The compiler enforces the architecture.** `er-domain` physically cannot import `rusqlite` or `reqwest` — its `Cargo.toml` doesn't list them. "Replay is pure and deterministic" becomes a build-time guarantee. For event-sourced medical records, this is the strongest argument on this page.
- **Phase 4 becomes dramatically cleaner.** `er-ffi` is a thin crate exposing exactly the surface Swift needs; the xcframework build pulls `er-app` + deps and *nothing else* — no CLI, no mocks, no spike binaries in the iOS binary.
- **Platform deps are quarantined.** `whisper-rs` is a feature on `er-pipeline`; SQLCipher a feature on `er-store`; neither pollutes anyone else's build. Desktop dev never compiles whisper.
- **Compile-time and test wins.** Touching the Claude prompt rebuilds `er-ai` + leaves `er-domain`/`er-store` cached. `cargo test -p er-domain` runs the pure core in milliseconds.
- **Scales to the team.** When a second contributor takes FHIR work, `er-emr` is an obviously bounded workspace member with its own tests and review surface.
- **Server-ready.** If the web-backend exploration (`approck-web-architecture.md`) ever goes live, an `er-server` crate slots in next to `er-cli` reusing `er-app` unchanged.

### Cons
- **Highest up-front cost.** A day-plus of Cargo.toml authoring, import rewriting across every file, deciding what each crate re-exports, and CI updates. Real risk of a multi-day yak-shave if circular-dependency surprises emerge (e.g., `EventBus` needing `apply_event` means projection logic must live in `er-store` or `er-app`, not `er-domain` — decidable, but it must be decided).
- **Heavier ceremony while solo.** Cross-crate changes (add a `NoteEvent` field → touch er-domain, er-store, er-app, er-cli) now involve four crates' worth of `pub` plumbing. At 5K LOC and pre-Phase-4, some of this structure is paying for passengers who haven't boarded yet.
- **Premature boundary risk.** If a boundary is drawn wrong (does `EncounterDriver` belong to pipeline or app?), moving code between crates is noisier than moving it between modules.
- Workspace versioning/publishing complexity is mild but nonzero (kept internal, never published, it's manageable).

### Best if
You're committed to the iOS/FFI phase (you are — it's the product), want correctness properties enforced rather than reviewed, and can afford one focused migration sprint now to avoid an unavoidable, larger one mid-Phase-4.

---

## Option 3 — Feature-Sliced (Vertical) Modules in One Crate

Keep one crate, but organize by *clinical capability* (bounded context) rather than technical layer. Each slice owns its models, events, logic, and adapters; `kernel/` holds the genuinely shared spine.

```
er-note-app/
├── Cargo.toml
├── docs/                    # same as Option 1
├── src/
│   ├── lib.rs
│   ├── kernel/              # the shared spine — small and stable
│   │   ├── mod.rs
│   │   ├── events.rs        # NoteEvent, EventBus, replay machinery
│   │   ├── store.rs         # Repository trait, SqliteRepository, db.rs, snapshots
│   │   ├── ids.rs           # PatientId, EncounterId newtypes
│   │   └── error.rs
│   ├── noting/              # the note lifecycle bounded context
│   │   ├── mod.rs
│   │   ├── note.rs          # PatientNote aggregate
│   │   ├── state.rs         # phase state machine
│   │   ├── disposition.rs
│   │   ├── sections.rs      # section history / versions
│   │   └── export.rs        # exporting IS a note capability
│   ├── encountering/        # capture: audio → transcript
│   │   ├── mod.rs
│   │   ├── encounter.rs
│   │   ├── audio.rs         # trait + mocks; later real capture
│   │   ├── stt.rs           # trait + mocks; later whisper
│   │   └── driver.rs        # EncounterDriver
│   ├── summarizing/         # AI: transcript → HPI/ROS/Exam/MDM
│   │   ├── mod.rs
│   │   ├── summarizer.rs    # trait
│   │   ├── claude.rs
│   │   ├── deidentify.rs
│   │   └── prompts.rs
│   ├── results/             # labs / imaging / vitals ingestion
│   │   ├── mod.rs
│   │   ├── result.rs
│   │   └── (future) fhir_sync.rs
│   ├── shift/               # multi-patient orchestration
│   │   ├── mod.rs
│   │   ├── shift.rs
│   │   ├── manager.rs       # ShiftManager
│   │   └── patient.rs
│   ├── emr/                 # (Phase 6+) fhir_read, fhir_write, hl7
│   ├── cli/                 # cli.rs, conversions/
│   └── main.rs
├── src/bin/spike_summarizer.rs
└── tests/
```

### Pros
- **Navigation matches how you think about the product.** "Fix the dispo flow" → `noting/`. "Tune the MDM prompt" → `summarizing/`. Roadmap phases map nearly 1:1 to slices: Phase 3 → `encountering/stt.rs`, Phase 5 → `shift/`, Phase 6–8 → `emr/` + `results/`. New work has an obvious home; the flat-`src/` "where does this go?" question disappears.
- **Slices grow independently without restructuring.** FHIR lands as files inside `emr/` and `results/`; whisper lands inside `encountering/`. No future re-org needed as subsystems triple in size.
- **Cheap migration** — same order of effort as Option 1 (moves + mod paths).
- **Each slice is a future crate.** If/when you go workspace, slices lift out cleanly — and you'll draw the crate boundaries *after* seeing where the real seams are, rather than guessing now.
- Keeps related trait + impl together (e.g., `Summarizer` trait next to `ClaudeSummarizer`), which reads well at this codebase size.

### Cons
- **The aggregate fights the slicing.** `PatientNote` is an event-sourced aggregate that spans noting, encountering, and results — `apply_event` must touch all three contexts. You either centralize replay in `kernel/` (pulling aggregate logic away from its slice) or let slices reach into each other (eroding the boundaries). Event-sourced single-aggregate systems are genuinely awkward to vertical-slice; this is the structural mismatch of this option.
- **Boundaries are even softer than Option 1's.** Layer rules ("domain imports no I/O") are mechanical to review; slice rules ("`summarizing` shouldn't know about `shift`") require judgment, and cross-slice imports creep in quietly.
- **Doesn't isolate purity.** AI clients, SQLite, and pure domain logic coexist within slices, so the "replay is deterministic, no I/O" property has no structural home at all.
- **kernel/ tends to swell.** Anything contested lands in kernel; in two phases it can become the new flat-src.
- Same single-crate downsides as Option 1: no compile-time isolation, feature-flag tangle, implicit FFI surface.

### Best if
You optimize for day-to-day navigability and product-shaped thinking, the team stays small, and you accept that the event-sourced core will live in `kernel/` rather than being cleanly sliced.

---

## Comparison at a Glance

| | Opt 1: Layered crate | Opt 2: Workspace | Opt 3: Feature slices |
|---|---|---|---|
| Migration effort | ~half day | 1–2 days | ~half day |
| Boundary enforcement | Convention | **Compiler** | Convention (weakest) |
| Fit with event-sourced aggregate | Good | **Good** | Awkward |
| Phase 4 FFI readiness | Moderate | **Excellent** | Moderate |
| Platform-dep isolation (whisper, SQLCipher) | Feature flags | **Per-crate** | Feature flags |
| Compile/test isolation | None | **Yes** | None |
| Day-to-day navigability (solo) | Good | Good, more ceremony | **Excellent** |
| Future server/web reuse | OK | **Designed for it** | OK |
| Risk of premature structure | Low | Moderate | Low |

## Recommendation

**Option 1 now, Option 2 at the start of Phase 4** — and Option 1's layer folders are deliberately drawn so each becomes a crate with near-zero rework. The deciding factors:

1. The event-sourced aggregate makes Option 3's slicing fight the grain of the system.
2. The compiler-enforced purity of Option 2 is the right end state for medical software, and the FFI phase *requires* most of that work anyway — but pre-Phase-4, while the CLI is the only frontend and you're iterating on Phase 3 summarization, workspace ceremony slows you down for boundaries nothing yet violates.
3. The two moves with zero downside — `docs/` and promoting `src/tests/` to `tests/` — should happen regardless of which option you pick, and can happen today.
