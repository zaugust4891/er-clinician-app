# ER Clinician App (Backend)

`er-clinician-app` is a Rust-based ambient clinical documentation system built for Emergency Department workflows.

Its core goal is to capture fragmented, non-linear encounters across a patient’s full ED stay and turn them into one coherent, billable note.

> **Status:** This project is a work in progress.

## What the application does

- Manages multi-patient ED shifts
- Tracks repeated encounters per patient over time
- Stores note activity as immutable clinical events
- Rebuilds note state from event replay (+ snapshots for speed)
- Generates HPI/Exam summaries and MDM with AI
- Captures objective chart data (labs, meds, diagnoses, disposition, reviewed data)
- Exports note text for downstream systems
- Supports both local and remote/cloud-backed operation

## Why this is different

Most ambient tools focus on one conversation. This app is designed for **multi-encounter merging** in the ED, where a single visit can involve many short interactions over hours.

## Technology stack

- **Language:** Rust
- **Architecture:** Cargo workspace with layered crates
- **Local persistence:** SQLite (`rusqlite`)
- **Server persistence:** Postgres (`sqlx`, migrations, offline `.sqlx` cache)
- **Server framework:** `axum`
- **Runtime/concurrency:** `tokio`
- **Auth:** JWT + JWKS (dev keys or Cognito)
- **AI:** Anthropic Claude integration
- **Sync protocol:** Event idempotency + cursor paging over HTTP

## Workspace structure

All core code lives under:

`/home/runner/work/er-clinician-app/er-clinician-app/crates`

- `er-domain` — pure domain model and rules (events, state transitions, replay)
- `er-store` — repository abstraction + SQLite implementation
- `er-ai` — summarizer abstraction, Claude client, de-identification
- `er-pipeline` — audio/STT interfaces and mocks
- `er-app` — orchestration layer (EventBus, ShiftManager, load/replay, export)
- `er-api` — shared API DTO contract + remote client adapter
- `er-server` — HTTP server, auth middleware, Postgres repository, worker
- `er-cli` — interactive CLI and standalone helper binaries

## Architectural principles

- **Event sourcing first:** clinical note state is derived from event history
- **Snapshots for performance:** periodic checkpoints speed reload
- **Layered dependencies:** crates point inward toward domain
- **Shared wire contract:** server and client compile against the same DTO crate
- **Mode swap without app rewrite:** local and remote modes use the same app-level flows

## Runtime modes

### Local mode

- Uses local SQLite database (`er_notes.db`)
- Uses local shift metadata (`shift.json`)
- Runs entirely on one machine

### Remote mode

- Enabled with `ER_SERVER_URL` + `ER_TOKEN`
- Uses server as source of truth
- Supports cross-device shift resume
- Uses authenticated, org-scoped APIs backed by Postgres

## Core flows

1. Clinician starts/stops encounters for the active patient.
2. Events are persisted first, then projected to in-memory state.
3. On stop, transcript is summarized into note sections (best effort).
4. Objective data is accumulated through structured commands/events.
5. `mdm` triggers AI-assisted MDM generation and state transition.
6. Notes can be reloaded/replayed reliably after crash or restart.

## Security features

### 1) JWT/JWKS validation

- RS256 signature verification
- Required issuer and expiration checks
- Cognito-specific access token and client checks
- Unknown/missing key-id (`kid`) rejection with refresh support

### 2) Tenant isolation by construction

- Repositories are created per authenticated organization
- No unscoped “read all orgs” request path for normal handlers

### 3) Trusted org resolution model

- Token proves identity (`sub`)
- Org membership is resolved server-side from clinician records
- Token org claim is non-authoritative

### 4) Existence-oracle resistance

- “Missing” and “other org” cases intentionally collapse to `404`
- Prevents cross-tenant resource discovery by probing IDs

### 5) PHI-minimal wire types

- Registration/request DTOs omit direct identity fields by design
- Reduces accidental PHI exposure in network payloads

### 6) De-identification before external AI calls

- Structured PHI scrubbing (e.g., SSN/phone/MRN/date patterns)
- Patient alias replacement in app flow
- Worker-safe scrub path for server processing

### 7) PHI-safe error boundary

- Client-facing error codes are terse and static
- Internal diagnostic details are kept in server logs

### 8) Secure audio upload flow

- Short-lived S3 presigned upload URLs
- Audio MIME type allowlist
- Verification of object existence and non-empty upload before queueing
- UUID-based storage keys to avoid human identifiers in object paths

### 9) Idempotency and concurrency controls

- Event writes are idempotent via stable event IDs
- Per-patient advisory transaction locking for concurrent append safety
- Deterministic worker transcript event IDs for dedupe behavior

### 10) Snapshot integrity checks

- SHA-256 checksums on persisted snapshots
- Checksum mismatch triggers safe replay fallback instead of trusting corrupted state

### 11) Defensive API limits

- Batch size caps and pagination limits
- Patient/event consistency validation at API boundaries

### 12) Security-focused automated tests

- Auth rejection coverage (tamper, expiry, wrong issuer/use/client)
- Tenancy and replay/snapshot correctness tests across implementations

## What is currently implemented

- Multi-encounter note lifecycle and state machine
- Event sourcing + snapshot load path
- CLI for shift/patient/encounter and note workflows
- AI summary + one-button MDM flow
- Local SQLite mode
- Remote API + Postgres mode with auth and tenancy
- Worker and queue-oriented async processing foundations

## What is not finished yet

- Full iOS platform integration (real microphone capture + app layer)
- Full production STT/audio pipeline integration in all paths
- EMR/FHIR integration
- Complete cloud deployment/hardening phases
- Device offline queueing behavior in remote mode

## Repository root

`/home/runner/work/er-clinician-app/er-clinician-app`

