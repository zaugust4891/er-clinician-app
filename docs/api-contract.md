# Application API contract

This document describes the APIs implemented and consumed by this repository as
of 2026-11-04. It covers the client-facing `er-server` HTTP API, the direct audio
upload boundary, and the provider/queue contracts used by the worker. It describes
current implementation behavior, including limitations; planning documents may
describe functionality that is not implemented yet.

The source of truth is [routes.rs](../crates/er-server/src/routes.rs), the
[shared request/response types](../crates/er-api/src/types.rs), the
[remote client](../crates/er-api/src/client.rs), and the
[Postgres repository](../crates/er-server/src/repository.rs). Domain types used on
the wire are specified below. Deployment configuration is covered separately in
the [server README](../crates/er-server/README.md) and
[deployment contract](ops/deployment-contract.md).

## 1. Transport and common conventions

| Item | Contract |
| --- | --- |
| Base URL | Deployment origin, with no `/v1` suffix; local example: `http://localhost:8080`. The CLI uses `ER_SERVER_URL` and `ER_TOKEN` to select remote mode. |
| Version | Application routes start with `/v1`. Health routes are unversioned. |
| Authentication | All implemented `/v1` routes require `Authorization: Bearer <JWT>`. `/healthz` and `/readyz` do not. |
| Request encoding | JSON bodies use `Content-Type: application/json`. Operations marked “none” below require no body. |
| Response encoding | `application/json` unless explicitly documented otherwise. |
| User agent | Send a static, non-identifying `User-Agent`; the Rust client sends `er-note/<crate version>`. The deployed WAF can reject requests without one. |
| Caching | All responses passing through the application router include `Cache-Control: no-store`, including errors and probes. |
| Correlation | The application generates a fresh UUID in the `x-request-id` response header. It does not reuse a caller-supplied correlation ID. |
| JSON body limit | 2,097,152 bytes (2 MiB) at the application JSON extractor. This does not limit the direct S3 audio PUT. |
| Request deadline | 30 seconds at the application router; expiry returns `408` with an empty body. A timeout does not prove that a write had no effect. |
| UUIDs | JSON strings; path parameters are UUIDs. Clients generate stable patient, event, encounter, chunk, and MDM job IDs. The server generates shift IDs. |
| Timestamps | RFC 3339 strings, serialized in UTC; fractional seconds may be present. |
| Integers | JSON numbers. `u32` and `u64` below mean unsigned 32-bit and 64-bit integers; `i32` means signed 32-bit integer. Preserve full integer precision in clients. |
| Nullability | `T?` means `T` or `null`. Serde accepts omitted optional fields as `null`; responses normally include the key with `null`. Other fields are required unless a default is explicitly documented. |
| Unknown fields | Current JSON structs ignore additional object fields. Unknown domain enum variants fail deserialization. Status strings are intentionally open-ended. |

Use HTTPS for deployed origins. The checked-in edge configuration adds an 8 KiB
body restriction except for canonical UUID paths on `POST .../events` and
`PUT .../snapshot` with `application/json` (optionally `; charset=utf-8`). Those
sync requests remain subject to the application's 2 MiB limit and other WAF
rules. The WAF also has a configurable per-IP rate rule, defaulting to 2,000
requests over 300 seconds; this is a deployment setting, not a per-clinician API
quota. Edge rejections may have different headers and bodies from application
responses. See [WAF configuration](../infra/modules/edge/waf.tf) and
[edge variables](../infra/modules/edge/variables.tf).

### Authentication and tenancy

The server verifies an RS256 JWT using the configured JWKS, including `kid`,
issuer, and expiration. In Cognito mode it additionally requires
`token_use: "access"` and the configured `client_id`; an ID token is not accepted.
The token's `sub` must resolve to a provisioned clinician. Organization membership
comes from the database, not a token org claim or request parameter.

The current header parser expects the literal prefix `Bearer `, including case
and space. Any authentication failure returns `401` with an empty body, including
an unknown clinician or failure to resolve membership. Each request requires an
active `physician` membership row (`disabled_at IS NULL`); membership is read on
every request, so revocation takes effect without an API restart. Already admitted
requests and accepted jobs may finish.

Authenticated quotas are shared across API replicas: 60 requests per clinician per
UTC minute, plus 5/min for MDM and 30/min for audio intents. Expensive requests also
consume the general quota; retries count. A denial returns `429` with
`Retry-After: 60`. Fixed-window boundary bursts are possible. Health/readiness are
exempt. Quota storage failure returns `503 {"error":"unavailable"}`.

GET access attempts except `/v1/me` are recorded before releasing data. Audit
storage failure returns the same 503 and prevents the read. Evidence stores the
route template and resource UUID, never raw paths, tokens, query strings or bodies.

Patient, chunk, job, snapshot, and shift lookups are organization-scoped. Missing
and other-organization resources normally return the same `404`. The active
shift lookup is also scoped to the authenticated clinician. Reading or ending a
shift by ID is currently organization-scoped, without a separate clinician-owner
check. ID collisions during writes have specific behavior documented below.

The application exposes no login, token-refresh, or public onboarding endpoint.
Credentials are obtained outside this API; see the
[authentication/onboarding runbook](ops/auth-onboarding.md).

### Errors

Handler errors use this JSON shape:

```json
{"error":"not_found"}
```

| Status | Meaning and body |
| --- | --- |
| `400` | Handler validation failure: `{"error":"<code>"}`. Invalid path/query values or malformed JSON may instead produce framework text. |
| `401` | Authentication failed; empty body. |
| `404` | Missing/inaccessible resource: `{"error":"not_found"}`. An unmatched route uses the framework fallback, not this envelope. |
| `405` | Unsupported method; framework response. |
| `408` | Application request timeout; empty body. |
| `409` | Resource/state conflict: `{"error":"<code>"}`. |
| `413` | JSON body exceeds the application limit; framework rejection. |
| `415` | Missing or unsupported JSON content type on an endpoint that extracts JSON; framework rejection. |
| `422` | Syntactically valid JSON cannot deserialize into the required body, for example a missing required field or negative `chunk_index`; framework rejection. |
| `429` | Clinician quota exhausted: `{"error":"rate_limited"}` and `Retry-After: 60`. |
| `500` | Handler/storage/provider infrastructure failure: `{"error":"internal"}`. Internal details are not returned. |
| `503` | Readiness failure on `/readyz`: `{"status":"unavailable"}`. Authenticated quota/audit storage failure: `{"error":"unavailable"}`. |

Branch primarily on HTTP status. The static error strings below describe current
diagnostics, not a separate versioned error-code API. Clients must tolerate empty,
text, or infrastructure-generated error bodies. A successful status does not
guarantee that an asynchronous job has completed.

## 2. Endpoint inventory

Every path parameter in this table is a UUID. `PatientNote` and other nested types
are defined in section 9. The status column describes success only; common errors
above and endpoint-specific errors below still apply.

| Method | Path | Request body | Success | Response |
| --- | --- | --- | --- | --- |
| GET | `/healthz` | none | `200` | `{"status":"ok"}` |
| GET | `/readyz` | none | `200` | `{"status":"ready"}` |
| GET | `/v1/me` | none | `200` | `{clinician_id: UUID, org_id: UUID}` |
| POST | `/v1/patients` | `RegisterPatientRequest` | `200` | `PatientView` |
| POST | `/v1/patients/{id}/events` | `AppendEventsRequest` | `200` | `AppendEventsResponse` |
| GET | `/v1/patients/{id}/events` | none | `200` | `EventsPage` |
| PUT | `/v1/patients/{id}/snapshot` | `SnapshotUp` | `200` | Empty body |
| GET | `/v1/patients/{id}/snapshot` | none | `200` | `SnapshotDown` |
| GET | `/v1/patients/{id}/note` | none | `200` | `NoteView` |
| GET | `/v1/patients/{id}/note/export` | none | `200` | `text/plain; charset=utf-8` |
| POST | `/v1/patients/{id}/encounters/{eid}/audio` | `AudioUploadRequest` | `200` | `AudioUploadResponse` |
| POST | `/v1/audio/{chunk_id}/complete` | none | `200` | `ChunkView` |
| GET | `/v1/audio/{chunk_id}` | none | `200` | `ChunkView` |
| POST | `/v1/patients/{id}/mdm` | `MdmRequest` | `202` | `MdmAccepted` |
| GET | `/v1/jobs/{job_id}` | none | `200` | `JobView` |
| POST | `/v1/shifts` | none | `200` | `ShiftView` |
| GET | `/v1/shifts/active` | none | `200` | `ShiftView` |
| GET | `/v1/shifts/{id}` | none | `200` | `ShiftView` |
| POST | `/v1/shifts/{id}/end` | none | `200` | `ShiftView` |
| GET | `/v1/stats` | none | `200` | `StatsView` |

Axum also provides HEAD handling for GET routes. There are no dedicated patient
listing, patient editing, encounter creation, note editing, deletion, WebSocket,
or server-sent event routes. Encounter and clinical changes travel as note events;
the shift response supplies the patient roster.

## 3. Patients and shifts

### Register a patient

`POST /v1/patients`

`RegisterPatientRequest`:

```json
{
  "patient_id": "0198f1c0-0000-7000-8000-000000000001",
  "chief_complaint": "Synthetic encounter",
  "display_label": "Room 4"
}
```

`patient_id` is a client-generated UUID; `chief_complaint` is a string that must
contain at least one non-whitespace character; `display_label` is `string?`.
Blank complaints return `400 chief_complaint_required`. Names, ages, and MRNs are
not registration fields. Labels should be non-identifying.

`PatientView`:

```json
{
  "patient_id": "0198f1c0-0000-7000-8000-000000000001",
  "chief_complaint": "Synthetic encounter",
  "display_label": "Room 4",
  "created_at": "2026-10-21T14:00:00Z"
}
```

Registration inserts the patient and attaches it to the caller's active shift,
if one exists, in one transaction. An active shift is not required. Repeating an
ID does not update stored complaint/label fields or duplicate its roster entry.
Repeating registration during a later active shift can attach the patient to that
shift. Starting a shift does not automatically attach previously registered patients.

**Current response caveat:** the handler echoes the submitted complaint/label and
uses the response-time `created_at`, even on a duplicate insert. This response is
not a readback of the stored row. Shift roster entries contain stored values and
the database creation time. Registration does not explicitly reject a globally
colliding patient ID; clients must generate fresh UUIDs for new patients and reuse
an ID only for the same patient registration.

### Shift lifecycle

`ShiftView`:

```json
{
  "shift_id": "0198f1c0-0000-7000-8000-000000000010",
  "started_at": "2026-10-21T14:00:00Z",
  "ended_at": null,
  "status": "active",
  "patients": []
}
```

`patients` is an array of `PatientView` in stored roster-position order.
`status` is currently `active` or `ended`; `ended_at` is `timestamp?`.

| Operation | Behavior |
| --- | --- |
| `POST /v1/shifts` | Creates a server-ID shift for the authenticated clinician. One active shift per clinician. A second start returns `409 shift_already_active`. |
| `GET /v1/shifts/active` | Returns the caller's active shift and roster; `404` means no active shift. |
| `GET /v1/shifts/{id}` | Returns an active or ended shift in the caller's organization; missing/foreign ID returns `404`. |
| `POST /v1/shifts/{id}/end` | Sets `status=ended` and `ended_at` once, then returns the shift and roster. Repeating the call preserves the end timestamp. Missing/foreign ID returns `404`. |

After an uncertain start response, fetch `/v1/shifts/active` to recover the shift
ID. Ending a shift allows a subsequent start to create a new shift.

## 4. Event append and cursor synchronization

### Append

`POST /v1/patients/{id}/events`

`AppendEventsRequest` contains `events: EventUp[]`, where each `EventUp` is
`{event_id: UUID, event: NoteEvent}`. `NoteEvent` is an externally tagged JSON
object: its single key is the exact variant name, and its value is the payload.

```json
{
  "events": [
    {
      "event_id": "0198f1c0-0000-7000-8000-000000000003",
      "event": {
        "AudioStarted": {
          "patient_id": "0198f1c0-0000-7000-8000-000000000001",
          "encounter_id": "0198f1c0-0000-7000-8000-000000000002",
          "capture_mode": "Ambient",
          "clinical_occurred_at": "2026-10-21T14:01:00Z"
        }
      }
    }
  ]
}
```

`AppendEventsResponse` contains acknowledgments in request order:

```json
{
  "results": [
    {"event_id":"0198f1c0-0000-7000-8000-000000000003","sequence_id":41}
  ]
}
```

Rules:

- A batch contains 1–500 events and must also fit the body limit. Empty or larger
  batches return `400 empty_batch` or `400 batch_too_large`.
- Every event's top-level `patient_id` must match the URL; otherwise
  `400 event_patient_mismatch`. These validations run before any batch writes.
  The patient must be registered in the caller's organization, otherwise `404`.
- `event_id` is the idempotency key. A duplicate in the same patient/org stream
  returns the original `sequence_id`; it does not replace the event. Payload
  equality is not checked, so never change an event while retaining its ID.
- Each event is committed separately. The whole batch is **not atomic**: a later
  storage failure can return `500` after earlier events have committed. Retry the
  original batch with the original IDs after an uncertain result.
- An event ID already belonging to a different patient/org currently produces
  `500 internal`, not `409`.
- The append handler persists deserializable events; it does not enforce the note
  state machine, nested medication/result/diagnosis patient IDs, or separate
  worker-only permissions for event variants. Clients must maintain valid domain
  sequences and consistent nested IDs.

### Read a page

`GET /v1/patients/{id}/events?since=0&limit=500`

| Query | Type | Default | Behavior |
| --- | --- | --- | --- |
| `since` | `u64` | `0` | Exclusive sequence cursor: return events with `sequence_id > since`. |
| `limit` | Nonnegative integer (`usize`) | `500` | Clamped to 1–1,000; `0` becomes `1`, values above 1,000 become 1,000. |

Use cursors returned by the server. Storage uses signed 64-bit sequence values;
clients must not manufacture values outside `0..=9223372036854775807` even though
the wire DTO accepts `u64`.

`EventsPage`:

```json
{
  "events": [
    {
      "sequence_id": 41,
      "event_id": "0198f1c0-0000-7000-8000-000000000003",
      "event": {
        "AudioStarted": {
          "patient_id": "0198f1c0-0000-7000-8000-000000000001",
          "encounter_id": "0198f1c0-0000-7000-8000-000000000002",
          "capture_mode": "Ambient",
          "clinical_occurred_at": "2026-10-21T14:01:00Z"
        }
      },
      "occurred_at": "2026-10-21T14:01:00Z"
    }
  ],
  "has_more": false
}
```

Each returned `EventDown` has `sequence_id: u64`, `event_id: UUID`,
`event: NoteEvent`, and `occurred_at: timestamp`. The latter is clinical time,
matching the event's `clinical_occurred_at`, not server receipt time.

Events are ordered by ascending sequence ID. Sequences are shared across patients
and can have gaps. Advance `since` to the last received sequence ID, never by page
length or by adding one. Continue while `has_more` is true. An empty stream or a
cursor at its head returns `{"events":[],"has_more":false}`. A missing/foreign
patient returns `404`. This is a live cursor, not a frozen multi-page snapshot;
poll again for subsequent writes.

## 5. Snapshots, note projections, and export

### Snapshot checkpoint

`PUT /v1/patients/{id}/snapshot` accepts `SnapshotUp`; GET returns `SnapshotDown`.
Both have `{sequence_id: u64, snapshot: PatientNote}`. A fresh checkpoint example:

```json
{
  "sequence_id": 0,
  "snapshot": {
    "id": "0198f1c0-0000-7000-8000-000000000004",
    "patient_id": "0198f1c0-0000-7000-8000-000000000001",
    "phase": "Registered",
    "encounter_count": 0,
    "encounters": [],
    "diagnoses": [],
    "medication": [],
    "data_reviewed": [],
    "results": [],
    "note_sections": {"hpi":null,"ros":null,"exam":null,"mdm":null,"disposition_text":null},
    "section_history": [],
    "disposition": null,
    "version": 0,
    "audit_log": [],
    "created_at": "2026-10-21T14:00:00Z",
    "updated_at": "2026-10-21T14:00:00Z"
  }
}
```

PUT returns `200` with no body. GET returns the saved checkpoint, not a projection
including later events. There is one stored snapshot per patient; another PUT
replaces it. A server-computed SHA-256 checksum guards stored serialization.
Missing snapshots and checksum-rejected snapshots return `404`, as do inaccessible
patients. A client can fall back to event replay from zero when no checkpoint is
available.

The snapshot endpoint currently validates patient access and JSON shape only.
It does not verify that the embedded `patient_id` matches the URL, that the cursor
exists, that the snapshot matches replay, or that the new cursor is newer than the
stored one. Clients must supply a consistent checkpoint and avoid overwriting a
newer snapshot with stale state. There is no ETag or compare-and-swap mechanism.

`PatientNote` is a domain serialization, **not** `NoteView`: it has nested
`note_sections`, singular `medication`, history, encounters, and audit entries.
The Rust-only `state` trait object is omitted. Rust consumers must call `hydrate()`
after deserializing a checkpoint before using domain state methods; the shared
remote repository already does this.

To resume, read a checkpoint, replay events with `since=checkpoint.sequence_id`,
and keep following pages. Without a checkpoint, replay from `since=0`. Note
`version` is a domain version and is not the event-stream cursor.

### Read the current note

`GET /v1/patients/{id}/note` returns a `NoteView` projected from a valid checkpoint
plus subsequent events, or full replay. A registered patient with no note history
returns a fresh `Registered` view; its fresh timestamps are generated at read time.

```json
{
  "patient_id": "0198f1c0-0000-7000-8000-000000000001",
  "phase": "Registered",
  "hpi": null,
  "ros": null,
  "exam": null,
  "mdm": null,
  "disposition": null,
  "medications": [],
  "diagnoses": [],
  "results": [],
  "data_reviewed": [],
  "encounter_count": 0,
  "version": 0,
  "created_at": "2026-10-21T14:00:00Z",
  "updated_at": "2026-10-21T14:00:00Z"
}
```

`hpi`, `ros`, `exam`, and `mdm` are `string?`; `disposition` is `Disposition?`.
The array element types are `Medication`, `Diagnosis`, `ClinicalResult`, and
`string`, respectively. `encounter_count` is `u32`; `version` is `u64`.
Current phases are `Registered`, `Encountering`, `Idle`, `Finalizing`,
`PendingSignOff`, and `Signed`. `NoteView.phase` is a string rather than a closed
wire enum.

### Export and stats

`GET /v1/patients/{id}/note/export` returns formatted note text with
`Content-Type: text/plain; charset=utf-8`. It uses the stored complaint/label and
note sections, results, medication, diagnoses, reviewed data, and disposition.
It does not require the note to be signed. A patient with neither events nor a
usable snapshot returns `404`, unlike the fresh view from `/note`.

`GET /v1/stats` returns organization-wide `StatsView` counters, both `u64`:

```json
{"event_count":12,"snapshot_count":2}
```

## 6. Audio upload and transcription

### Request an upload URL

`POST /v1/patients/{id}/encounters/{eid}/audio`

`AudioUploadRequest`:

```json
{
  "chunk_id": "0198f1c0-0000-7000-8000-000000000005",
  "chunk_index": 0,
  "is_final": true,
  "content_type": "audio/wav"
}
```

`chunk_id` is a stable client UUID. `chunk_index` is a zero-based `u32`, additionally
restricted to `0..=2147483647` by storage. `is_final` is a boolean. `content_type`
must exactly equal one of `audio/wav`, `audio/ogg`, `audio/mp4`, or `audio/mpeg`.
Extra MIME parameters and alternative spellings are not accepted.

`AudioUploadResponse`:

```json
{
  "chunk_id": "0198f1c0-0000-7000-8000-000000000005",
  "upload_url": "https://object-storage.example/presigned-placeholder",
  "expires_in_secs": 900
}
```

The URL in this example is illustrative. Use the returned URL exactly as issued.
The API stores a `pending` chunk before minting a 15-minute presigned PUT URL.
The patient must be registered in the organization. The handler does not verify
that `{eid}` already has a corresponding `AudioStarted` event; the client must use
the same encounter ID across the recording events and all chunks.

| Status/code | Condition |
| --- | --- |
| `400 content_type_unsupported` | MIME type outside the exact allowlist. |
| `400 chunk_index_too_large` | Deserializable `u32` exceeds the signed storage limit. |
| `404 not_found` | Missing/inaccessible patient. |
| `409 chunk_index_taken` | Another chunk occupies the encounter/index. |
| `409 final_chunk_already_set` | Another final chunk exists for the encounter. |
| `409 chunk_id_mismatch` | Same visible chunk ID reused with different patient, encounter, index, final flag, or type. |

Repeating the identical intent reissues a URL for the stored chunk without
creating another ledger row. A chunk ID colliding outside the organization
currently returns `500 internal`. Clients must use contiguous indexes from zero
and exactly one final chunk at the highest index. Intent validation does not
enforce contiguity or that the final index is the highest submitted index.

### Upload bytes directly

Send `PUT <upload_url>` with the raw audio bytes as the body and the exact declared
`Content-Type`. This is an object-storage operation, with its own status/body
format. The URL supplies authorization; do not attach the clinician's bearer
token. Do not log the URL. This is not multipart upload and does not pass audio
through the API server. The shared client accepts any successful 2xx PUT status.

If the URL expires before upload, repeat the same intent to obtain a new URL.
Presigning does not impose a pre-upload byte quota. Completion and the worker
enforce the configured `MAX_AUDIO_BYTES`: 16 MiB by default, configurable from
1 byte through 64 MiB. Metadata checks do not validate the actual audio codec.
Keep a chunk's bytes immutable after successful completion.

### Confirm and poll

`POST /v1/audio/{chunk_id}/complete` takes no body. The server checks the object
with S3 `HeadObject` while the row is pending, binds the first verified ETag and
version ID atomically with `uploaded` status, and enqueues transcription. Cloud
requires bucket versioning. The worker reads the bound version with If-Match;
unversioned development storage uses the ETag condition. Replayed uploads cannot
silently change the bytes later transcribed. A durable outbox recovers a failed
queue send even if the client never retries.
It returns `200 ChunkView`; it does not wait for transcription.

```json
{
  "chunk_id": "0198f1c0-0000-7000-8000-000000000005",
  "encounter_id": "0198f1c0-0000-7000-8000-000000000002",
  "chunk_index": 0,
  "is_final": true,
  "status": "uploaded",
  "duration_ms": null,
  "error": null
}
```

`ChunkView` uses `chunk_index: u32`, `is_final: boolean`, `status: string`,
`duration_ms: i32?`, and `error: string?`. Duration is the provider-measured audio
length, available after successful transcription; error is diagnostic status
text. Per-chunk transcripts and storage keys are not returned.

| Status/code | Completion condition |
| --- | --- |
| `404 not_found` | Missing/inaccessible chunk ledger row. |
| `409 chunk_not_uploaded` | Object storage reports the object missing. |
| `409 chunk_empty` | Object length is zero or negative. |
| `409 chunk_too_large` | Length exceeds the configured audio limit. |
| `409 chunk_length_missing` | Object metadata does not include a length. |
| `409 chunk_content_type_mismatch` | Object content type is absent or differs from the declared supported type. |
| `409 chunk_etag_missing` | Completion metadata has no ETag. |
| `409 chunk_version_missing` | Cloud completion metadata has no non-null version ID. |
| `500 internal` | Object-storage/queue/database failure; the chunk may already be marked uploaded. |

Repeat completion after an uncertain response: a nonterminal chunk can be
enqueued again, and the worker handles duplicate delivery. Repeating completion
does not regress its status. Already `done`/`failed` chunks are not requeued,
and the original binding is retained without verifying a newer object version.
Pending intents older than 24 hours expire to `failed` with `upload_expired`;
re-record with a new chunk ID rather than reusing an expired intent.

`GET /v1/audio/{chunk_id}` returns the same `ChunkView` without object-storage or
queue work. Current statuses progress through `pending`, `uploaded`, and
`transcribing` to terminal `done` or `failed`. Unknown future statuses must be
treated as nonterminal. Polling can skip intermediate states.

### Complete encounter workflow

1. Append `AudioStarted` with a client-generated encounter ID.
2. For each chunk, request its URL, PUT its bytes, and call completion.
3. On stop, append `AudioStopped` and submit exactly one final chunk. For a single
   chunk recording, index `0` is final.
4. Poll chunk status and the patient event cursor. Assembly waits until every
   index from `0` through the final index exists and is `done` or `failed`.
5. The worker appends one deterministic-ID `TranscriptReady` event for the whole
   encounter. Failed chunks appear as explicit gap markers in that transcript.
   It then attempts `EncounterSummarized`, which carries updated note sections.

A terminal chunk is not proof that assembly or summarization is finished. A
summary can fail while the saved transcript remains available. Observe the event
stream for clinical output; there is no separate encounter-summary status API.
The current CLI polls at two-second intervals with a 30-second observation budget
and a five-second timeout per poll request. Exhausting that budget means work is
still unconfirmed, not that the server job failed.

## 7. Asynchronous MDM generation

`POST /v1/patients/{id}/mdm` accepts `MdmRequest`:

```json
{"job_id":"0198f1c0-0000-7000-8000-000000000006"}
```

The patient must be registered in the caller's organization. The API writes a job
receipt before enqueueing and returns `202 MdmAccepted`:

```json
{"job_id":"0198f1c0-0000-7000-8000-000000000006","status":"queued"}
```

Keep the same client-generated `job_id` when retrying the same request. A retry
returns the existing status, including terminal status, with `202`; it can repair
a receipt whose initial queue send did not finish. Queue delivery remains
at-least-once. A repeated terminal job does not start another generation. Reusing
an ID for a different patient, clinician, kind, or organization currently returns
`500 internal`.

`GET /v1/jobs/{job_id}` returns `JobView`, or `404` for missing/foreign jobs:

```json
{
  "job_id": "0198f1c0-0000-7000-8000-000000000006",
  "kind": "mdm",
  "status": "done",
  "error": null,
  "created_at": "2026-10-21T14:03:00Z",
  "updated_at": "2026-10-21T14:03:10Z"
}
```

`kind` and `status` are strings; `error` is `string?`. Current statuses are
`queued`, `running`, `done`, and `failed`. Only `done` and `failed` are terminal;
unknown status values are nonterminal. Jobs can be read by clinicians in the same
organization, even if another clinician submitted them.

Acceptance does not validate generation eligibility. The worker normally requires
an existing note in `Idle` and at least nonblank HPI, assembled objective data, or
a disposition. It reports failures through `JobView.error`, including
`note_not_found`, `note_not_idle`, `nothing_to_generate_from`, `provider_refused`,
`invalid_model_output`, `generation_failed`, `generation_failed_after_retries`,
and `job_deadline_exceeded`.

Successful generation appends `MdmGenerated` before `TriggerMDM` and marks the job
done. Fetch events or `/note` to obtain the MDM content; the status response never
contains it. Existing saved MDM in `Idle`/`Finalizing` can satisfy recovery without
another provider call, so a new job ID is not a force-regeneration option.
Completion does not sign the note: `ReviewReady`, disposition changes, and `Sign`
remain explicit domain events.

## 8. Health and identity

- `GET /healthz`: liveness only; `200 {"status":"ok"}` without dependency checks.
- `GET /readyz`: checks database access and required API table availability/read
  privileges within a bounded check. Returns `200 {"status":"ready"}` or
  `503 {"status":"unavailable"}`. It does not perform live STT/AI calls or prove
  that worker processing is healthy.
- `GET /v1/me`: returns authenticated `{clinician_id: UUID, org_id: UUID}`. Neither
  value is accepted as a caller-selected tenant override.

## 9. Domain wire schemas

These types use Serde's default field names and enum encoding. Fields are required
except those marked `?`. All arrays are JSON arrays, including empty arrays.
Strings and enum tags are case-sensitive.

### Note events

Every event payload requires `patient_id: UUID` and
`clinical_occurred_at: timestamp`. Add the following fields for its variant:

| Variant key | Additional required fields |
| --- | --- |
| `AudioStarted` | `encounter_id: UUID`, `capture_mode: CaptureMode` |
| `AudioStopped` | `encounter_id: UUID` |
| `TranscriptReady` | `encounter_id: UUID`, `text: string` |
| `EncounterSummarized` | `encounter_id: UUID`, `sections: NoteSections`, `source_encounter_ids: UUID[]` |
| `MedicationRecorded` | `medication: Medication` |
| `ResultReceived` | `result: ClinicalResult` |
| `DiagnosisAssigned` | `diagnosis: Diagnosis` |
| `DataReviewed` | `physician_review: string` |
| `TriggerMDM` | None |
| `MdmGenerated` | `mdm_text: string`, `source_encounter_ids: UUID[]` |
| `ReviewReady` | None |
| `DispositionSet` | `disposition: Disposition` |
| `Sign` | None |

`CaptureMode` is the string `Ambient` or `Dictation`. Remote clients normally
leave `TranscriptReady`, `EncounterSummarized`, `MdmGenerated`, and the successful
generation's `TriggerMDM` to the worker. Appending `TriggerMDM` alone changes
replayed state; it does not call the generation endpoint or enqueue work.

See [events.rs](../crates/er-domain/src/events.rs) and
[replay.rs](../crates/er-domain/src/replay.rs) for event definitions and application
rules. Persisting an event is distinct from validating a state transition.

### Clinical values

The following notation lists exact JSON field names and their types.

| Type | Fields |
| --- | --- |
| `NoteSections` | `hpi: string?`, `ros: string?`, `exam: string?`, `mdm: string?`, `disposition_text: string?` |
| `Medication` | `id: UUID`, `patient_id: UUID`, `name: string`, `dose: string`, `route: string?`, `rxnorm_code: string?`, `kind: MedicationKind`, `administered_at: timestamp`, `frequency: string` |
| `Diagnosis` | `id: UUID`, `patient_id: UUID`, `text: string`, `icd10_code: string?`, `is_primary: boolean` |
| `ClinicalResult` | `id: UUID`, `patient_id: UUID`, `source: ResultSource`, `timestamp: timestamp`, `result_type: ResultType` |
| `LabResult` | `fhir_id: string?`, `loinc_code: string?`, `display_name: string`, `value: number`, `unit: string`, `reference_range: [number, number]?`, `is_abnormal: boolean` |
| `ImagingResult` | `modality: string`, `body_part: string`, `impression: string`, `is_critical: boolean` |
| `VitalSign` | `vital_type: VitalType`, `value: number`, `unit: string` |

`MedicationKind` is `Home` or `AdministeredInEd`. `ResultSource` is `Manual`,
`FhirPull`, or `Hl7Feed`; those source tags do not imply a live FHIR/HL7 integration.
`VitalType` is `HeartRate`, `BloodPressureSystolic`, `BloodPressureDiastolic`,
`RespiratoryRate`, `Temperature`, `SpO2`, or `Pain`.

`ResultType` is exactly one of `{"Lab": <LabResult>}`,
`{"Imaging": <ImagingResult>}`, or `{"Vital": <VitalSign>}`. For example:

```json
{
  "id": "0198f1c0-0000-7000-8000-000000000007",
  "patient_id": "0198f1c0-0000-7000-8000-000000000001",
  "source": "Manual",
  "timestamp": "2026-10-21T14:02:00Z",
  "result_type": {"Vital":{"vital_type":"HeartRate","value":72.0,"unit":"bpm"}}
}
```

`Disposition` is one of these JSON shapes:

| Variant | JSON shape |
| --- | --- |
| Discharge | `{"Discharge":{"instructions":"string","follow_up":null}}` (`follow_up: string?`) |
| Admit | `{"Admit":{"service":"string","bed":null}}` (`bed: string?`) |
| Transfer | `{"Transfer":{"destination":"string","reason":null}}` (`reason: string?`) |
| Against medical advice | `"AMA"` |
| Left without being seen | `"LWBS"` |

Sources: [medication](../crates/er-domain/src/medication.rs),
[diagnosis](../crates/er-domain/src/diagnosis.rs),
[results](../crates/er-domain/src/result.rs), and
[disposition](../crates/er-domain/src/disposition.rs).

### Snapshot-only structures

| Type | Fields |
| --- | --- |
| `PatientNote` | `id: UUID`, `patient_id: UUID`, `phase: NotePhase`, `encounter_count: u32`, `encounters: Encounter[]`, `diagnoses: Diagnosis[]`, `medication: Medication[]`, `data_reviewed: string[]`, `results: ClinicalResult[]`, `note_sections: NoteSections`, `section_history: NoteSectionVersion[]`, `disposition: Disposition?`, `version: u64`, `audit_log: AuditEntry[]`, `created_at: timestamp`, `updated_at: timestamp` |
| `Encounter` | `id: UUID`, `patient_id: UUID`, `capture_mode: CaptureMode`, `started_at: timestamp`, `ended_at: timestamp?`, `transcript: string?`, `encounter_number: u32` |
| `NoteSectionVersion` | `section: string`, `content: string`, `version: u64`, `generated_at: timestamp`, `source_encounter_ids: UUID[]` |
| `AuditEntry` | `id: UUID`, `timestamp: timestamp`, `action: AuditAction`, `details: string` |

`NotePhase` is the closed enum of the six phase strings listed in section 5.
`AuditAction` is one of the strings `PatientCreated`, `EncounterStarted`,
`EncounterStopped`, `TranscriptGenerated`, `ResultIngested`, `NoteSectionUpdated`,
`NoteSectionGenerated`, `NoteSectionVersion`, `MdmTriggered`, `MdmGenerated`,
`DispositionSet`, `DiagnosisAssigned`, `DataReviewed`, `MedicationRecorded`,
`NoteReviewed`, `NoteSigned`, or `NoteExported`; or the object
`{"PhaseTransition":{"from":"string","to":"string"}}`.

Sources: [note](../crates/er-domain/src/note.rs),
[encounter](../crates/er-domain/src/encounter.rs), and
[audit](../crates/er-domain/src/audit.rs).

## 10. External service and worker contracts

This section records the integration surface this code uses, not the full vendor
API specification. Provider versions, models, retry rules, and response parsing
below are taken from the checked-in adapters.

### Cognito JWKS

The API server fetches
`GET https://cognito-idp.{region}.amazonaws.com/{pool_id}/.well-known/jwks.json`
without a clinician bearer token. The response is a JSON Web Key Set with a `keys`
array; token `kid` selects the key. Startup requires a successful fetch. Unknown
key IDs can trigger a coalesced refresh, with a five-second request budget,
64 KiB response limit, and 30-second minimum spacing between attempts. Redirects
are disabled. Dev auth reads a local JWKS instead.

Sources: [auth configuration](../crates/er-server/src/config.rs),
[JWT validation](../crates/er-server/src/auth/claims.rs), and
[key loading](../crates/er-server/src/auth/keys.rs).

### S3 object storage

| Caller | Operation | Contract |
| --- | --- | --- |
| API | Presign `PutObject` | Bucket from configuration; key derived from org/patient/encounter UUIDs, chunk index, and chunk UUID; exact declared content type; 900-second expiry. |
| Device/client | Presigned HTTP PUT | Raw bytes and matching content type; URL supplies authorization. |
| API | `HeadObject` | Checks existence, positive length, byte limit, and exact content type before queueing. |
| Worker | `GetObject` | Reads bytes into memory, rechecks metadata and actual streamed length/size; 20-second overall download budget. |

Cloud server/worker requests use AWS task credentials through the SDK. Development
can use the configured S3-compatible emulator. Object-storage errors do not use
the application JSON envelope; the Rust upload helper preserves HTTP status and
replaces the body with `object_storage_rejected_upload` for its own caller.

### SQS background queue

Both job kinds use the configured `STT_QUEUE`. The API uses `SendMessage`; the
worker uses `ReceiveMessage`, `ChangeMessageVisibility`, and `DeleteMessage`,
plus `SendMessage` for durable outbox recovery. The recovery loop leases at most
ten IDs per pass and acknowledges only after SQS accepts a send.
Bodies are JSON with an **internal** `kind` tag, unlike the external tag on
`NoteEvent`:

```json
{"kind":"TranscribeChunk","chunk_id":"0198f1c0-0000-7000-8000-000000000005"}
```

```json
{
  "kind": "GenerateMdm",
  "patient_id": "0198f1c0-0000-7000-8000-000000000001",
  "job_id": "0198f1c0-0000-7000-8000-000000000006"
}
```

Messages carry identifiers, not audio, prompts, or transcripts. The worker
discovers tenancy from the stored receipt/chunk. Unknown job kinds fail decoding.
Delivery is at-least-once; persisted state and stable event IDs support recovery.
Visibility is 300 seconds, renewed every 60 seconds with a five-second renewal
budget. Total job handling is bounded to 270 seconds; shutdown drains at most
100 seconds. Lease loss cancels handling and leaves work for redelivery.

Sources: [queue format](../crates/er-server/src/queue.rs),
[worker](../crates/er-server/src/worker.rs), and
[runtime limits](../crates/er-server/src/config.rs). Queue calls are internal;
application clients use the HTTP endpoints, not these messages directly.

### Deepgram prerecorded transcription

The worker's `cloud-stt` adapter sends:

```http
POST https://api.deepgram.com/v1/listen?model=nova-3-medical&smart_format=true&diarize=true&utterances=true&mip_opt_out=true
Authorization: Token <DEEPGRAM_API_KEY>
Content-Type: <declared audio MIME type>

<raw audio bytes>
```

The adapter prefers `results.utterances[].transcript` and optional numeric
`speaker`, formatted into speaker lines. It falls back to
`results.channels[0].alternatives[0].transcript` when appropriate. Optional
alternative `confidence` must be between 0 and 1. `metadata.duration` supplies
audio duration in seconds; missing/invalid duration defaults to zero. The consumed
response subset looks like:

```json
{
  "metadata": {"duration":1.25},
  "results": {
    "channels": [{"alternatives":[{"transcript":"Synthetic recording.","confidence":0.98}]}],
    "utterances": [{"speaker":0,"transcript":"Synthetic recording."}]
  }
}
```

A present empty transcript is successful silence. Missing/malformed transcript
data without a valid fallback is a failure. Success bodies are capped at 4 MiB.
HTTPS is required and redirects are disabled. Up to three attempts use a
60-second deadline each and 500 ms/1 second backoff. HTTP `429`, `5xx`, and
eligible transport failures are retried locally; other HTTP failures and invalid
complete JSON bodies are not. Worker queue redelivery is a separate retry layer.
Provider error bodies are discarded.

Source: [Deepgram adapter](../crates/er-pipeline/src/deepgram.rs).

### Claude: direct Anthropic and Amazon Bedrock

`ClaudeSummarizer` produces encounter sections and MDM. The direct transport is
used for local development; staging/production worker configuration requires
Bedrock. Model selection comes from `SummarizerConfig` and deployment settings.

| Transport | Request/authentication |
| --- | --- |
| Direct Anthropic | `POST https://api.anthropic.com/v1/messages`; `x-api-key: <ANTHROPIC_KEY>`, `anthropic-version: 2023-06-01`, JSON content type. Body includes `model` and omits `anthropic_version`. |
| Bedrock | AWS SDK `InvokeModel`, IAM authentication, `model_id` as the operation parameter, JSON content/accept types. Body omits `model` and includes `anthropic_version: "bedrock-2023-05-31"`. |

The current common request structure is:

```json
{
  "max_tokens": 4096,
  "system": [
    {"type":"text","text":"<system prompt>","cache_control":{"type":"ephemeral"}}
  ],
  "messages": [{"role":"user","content":"<encounter or MDM input>"}]
}
```

Add the transport-specific `model` or `anthropic_version` field above. Encounter
input contains the transcript, encounter number, and prior `NoteSections`. MDM
input contains note sections, objective data, and disposition text. The consumed
response subset is:

```json
{
  "content": [{"type":"text","text":"<generated content>"}],
  "usage": {
    "input_tokens": 120,
    "output_tokens": 60,
    "cache_read_input_tokens": 0,
    "cache_creation_input_tokens": 0
  }
}
```

The adapter selects the first content block containing text and strips optional
Markdown JSON fences. Encounter output must deserialize as `NoteSections`; MDM
output is a string. Missing usage counters default to zero. Missing text or invalid
section JSON is a generation failure. Direct HTTP `429` is rate-limited; `5xx`
is a transport failure; other non-success statuses are refused. Bedrock maps
throttling/quota errors to rate limits and access/not-found/validation errors to
permanent refusals. Provider bodies are not forwarded to application clients.
Direct requests have a 45-second timeout with redirects disabled; the worker
also imposes a 45-second generation budget, including transport retries.

Sources: [transport and wire types](../crates/er-ai/src/transport.rs),
[summarizer adapter](../crates/er-ai/src/claude.rs), and
[model configuration](../crates/er-ai/src/model.rs).

## 11. Compatibility and verification

Changes to field names, required fields, externally tagged enum shapes, endpoint
statuses, or replay/idempotency semantics affect the shared contract. Status
strings deliberately permit future values, while adding a domain or queue enum
variant can break older deserializers. Snapshot compatibility also depends on
the serialized domain structures, not just `er-api` DTOs.

The repository's existing checks for this contract include:

- [DTO wire tests](../crates/er-api/src/types.rs): audio/MDM field names,
  nullability, unsigned indexes, and terminal statuses.
- [Sync gate](../crates/er-server/tests/sync_gate.rs) and
  [snapshot gate](../crates/er-server/tests/snapshot_gate.rs): deduplication,
  paging, replay, tenant isolation, and checkpoint behavior.
- [Read gate](../crates/er-server/tests/read_gate.rs) and
  [shift gate](../crates/er-server/tests/shift_gate.rs): note/export responses and
  shift lifecycle behavior.
- [Audio upload gate](../crates/er-server/tests/audio_upload_gate.rs),
  [completion gate](../crates/er-server/tests/audio_complete_gate.rs), and
  [AI gate](../crates/er-server/tests/ai_gate.rs): upload conflicts, completion,
  asynchronous receipt handling, and generation recovery.
- [Auth gate](../crates/er-server/tests/auth_gate.rs),
  [Cognito token gate](../crates/er-server/tests/cognito_token_gate.rs), and
  [edge gate](../crates/er-server/tests/edge_gate.rs): authentication, tenancy,
  health probes, and cache headers.

Update this document alongside route, DTO, domain serialization, adapter, and
edge-policy changes. The examples contain synthetic data and placeholder
credentials/URLs.
