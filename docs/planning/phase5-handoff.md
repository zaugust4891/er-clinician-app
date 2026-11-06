# Phase 5 server handoff

Updated 2026-11-05. Repository contracts are available; live staging acceptance is
still pending. Phase 4 is not fully accepted until the literal checklist in
[Step 10](phase4-step10-implementation.md) has live evidence.

## Environment and authentication

Get the actual staging origin, AWS region, Cognito pool ID and app client ID from
the accepted foundation manifest/operator release record. No verified staging URL
or test account is published here; do not build against an invented hostname.
Development defaults to `http://127.0.0.1:8080` with isolated dev keys.

Cognito access token → `Authorization: Bearer ...` → `/v1/me`. ID tokens are rejected.
The database resolves `sub` to an active physician/org each request; no org claim or
client body establishes tenancy. Disabled/revoked membership denies the next auth
check. Already accepted requests and jobs can finish. Configure Amplify against the
existing pool rather than creating another backend. See [Amplify Swift sign-in](https://docs.amplify.aws/swift/frontend/auth/sign-in/)
and the [existing onboarding procedure](../ops/auth-onboarding.md) for provisioning,
smoke user eligibility and revocation. `scripts/dev-token.sh` is local-only;
staging must use actual Cognito access tokens from an approved synthetic user.
No provider keys belong on the device.

## Contract and device persistence

The committed [OpenAPI 3.1 snapshot](../api/openapi.json) contains schemas generated
from actual Rust serde types, including externally tagged event enums, nullable
fields and nested snapshots. [The API contract](../api-contract.md) documents
semantics. Both `/api-docs/openapi.json` and `/api-docs` require a current Bearer
token. To open Swagger in a browser use an authenticated development proxy/header
injection, then enter the access token to load the spec; it stays in memory and
is not persisted. CDN assets have exact version/SRI pins in `../api/swagger-assets.json`.

Stamp patient, encounter, event, chunk and job UUIDv7 identities at creation;
persist them with the offline intent. Serialize NoteEvent as `{ "AudioStarted":
{ ... } }`, not as an internal `kind` tag. Preserve 64-bit event cursor precision
in Swift. Queue locally in SQLite before uploading; keep pending actions until
acknowledgments are persisted, including a timed-out or partially committed batch.
Batch limit is 500. Reuse IDs and bodies on retries; an ID is not a request nonce.
Persist server sequence IDs and apply cursor pages atomically in order; page while
`has_more`, tolerate global sequence gaps, and never advance a cursor just because
HTTP returned success. Snapshot checksums are server generated; a missing/corrupt
snapshot falls back to full replay. Remote mode currently has no offline queue:
Phase 5 must add one rather than assuming the CLI supplies it.

## Audio and generation

Use validated 30–60 second synthetic chunks during integration. Supported MIME
containers are `audio/wav`, `audio/ogg`, `audio/mp4`, `audio/mpeg`; Opus is a codec
inside an accepted container, not an accepted bare `audio/opus` MIME type. Validate
actual iOS encoder/provider support before promising Opus. Default cap is 16 MiB.
Use stable contiguous indices starting at zero and exactly one final chunk.

Append AudioStarted, mint the intent, PUT with the exact signed content type, then
complete. On stop append AudioStopped and submit the final chunk. URLs expire in
15 minutes; re-request the same intent if necessary. A lost completion response
requires a retry, not a new chunk identity. Completion fixes version/ETag so
replayed PUTs cannot change the completed bytes. Poll chunk status, and read the
assembled transcript/summary through patient events. Pending intents expire after
24 hours; failed work needs a visible recovery action, never silent completion.

POST `/v1/patients/{id}/mdm` with a stable `job_id` returns 202. Poll
`/v1/jobs/{id}` until done/failed, then sync events; receipt status is not generated
note content. Unknown future status strings are nonterminal. Timeouts are uncertain
writes, not proof of no effect. Display failures and retain local recovery state.
Review/sign remain clinician actions represented by events, after generated content.

## Polling and remaining product decisions

Honor 429 `Retry-After: 60`, refresh authentication on 401, distinguish 404 from
transport loss and back off on 5xx. Health endpoints are not proof that a particular
write succeeded. All responses are no-store. General quota is 60/minute/clinician,
MDM 5/minute, audio intent 30/minute. The planned eighteen-patient ten-second
polling model exceeds the general limit; choose/adopt a reviewed multiplexed sync
or quota policy before implementing that model. Request latency excludes provider
work; MDM is asynchronous. Real latency/availability measurements are pending.

Patient assignments, MFA policy, transcript retention/compaction, clinical recovery
and legal/privacy approval remain open gates. UI must not suggest that locally
captured material has synced or generated content is ready until durable server
acknowledgment and clinician review establish it.
