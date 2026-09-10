# Phase 4 Step 6 S6 review

Reviewed 2026-09-09. Commit: `6b9ebb9216e94122531b20e9d32eeb7ddd9fe7a9`.
Original verdict: changes required before integrating the Deepgram implementation.
After remediation: the S6 library meets its locally verifiable contract, with the
documented timeout deviation; live-vendor and whole-Step-6 acceptance remain open.
This is a scoped engineering review, not a determination of HIPAA compliance.

## Baseline and scope

- `git status --short` was empty at entry. The complete commit changed four files:
  `er-pipeline/Cargo.toml`, `src/deepgram.rs`, `src/lib.rs`, and `src/speech_to_text.rs`.
  The commit adds the vendor implementation and dispatch arm; it does not add a worker.
- No applicable `AGENTS.md` was found in the repository or ancestor directories.
  Read `CLAUDE.md`, the tracked Phase 4 guide, local detailed Step 6 notes, and relevant
  Phase 5/later-roadmap and hardening requirements. The detailed Step 6 notes are a
  pre-existing **ignored local file** (`.gitignore:29`), not part of this commit;
  their retrospective S6 narrative is supporting context, not versioned evidence.
  They are preserved. Other existing ignored data/configuration was not modified.
- The tracked guide was unchanged by the commit: its blob in both `HEAD` and `HEAD^`
  is `fd1e4880473d9dab4543a041388bc90dc4674126`. Its §6.4 says **30s**, not 300s.
  The review adds a clearly labeled deviation note without replacing that baseline.
- All current tracked changes and the two new files are review remediations. No commit,
  push, deployment, vendor request, real patient data, or secret inspection was performed.

## Requirement coverage

References below use repository-relative paths; line numbers refer to the remediation
unless explicitly marked “original.” `pipeline` means `crates/er-pipeline/src/deepgram.rs`.

| Requirement | Original assessment | Evidence after review |
|---|---|---|
| §6.1 / S6: async `SpeechToText`, shared `TranscriptResult`, dispatch, cloud feature fence | Met | `lib.rs:12`, `speech_to_text.rs:174`; default dependency tree has neither reqwest nor tokio; feature-enabled tests exercise the real enum arm |
| §6.4: raw audio POST, model, MIME, key header, formatted/diarized speech | Implemented, HTTP behavior untested | `pipeline:155`; loopback test checks the exact query, header, bytes, and parsed result |
| S6: utterances, flat fallback, silence, missing-transcript failure | Partial: malformed turns could discard speech or masquerade as silence | `pipeline:372`, parser regressions; complete flat fallback or explicit error; valid silence remains successful |
| §6.4: two retries with backoff on 429/5xx | Partial: interrupted 2xx bodies were always terminal | `pipeline:245`, `:318`; real HTTP tests prove retry limits, permanent failures, response interruptions, and timeout behavior |
| §6.4: 30s timeout | Deviation | Retain existing 60s per attempt, maximum STT allowance 181.5s; document discrepancy. This is not a full-job lease guarantee |
| §C5 / §6.4: minimal metadata, PHI-safe diagnostics, vendor privacy controls | Partial: metadata/opt-out present, error bodies unsafe | `pipeline:18`, `:198`; errors now contain fixed descriptions/status only; redirects refused, key marked sensitive |
| §6.4: duration and cost in `usage_log` | Not implemented; explicitly deferred by local detailed plan | Result carries duration; no STT billing row or durable per-attempt usage exists. Finish in Step 7; do not claim full §6.4 completion |
| S6 checks, device seam unchanged | Original 26 tests passed but excluded HTTP | Final all-features workspace run: 218 passed, including 40 pipeline tests; all-features check and strict crate lint pass; `audio.rs`, `stt.rs`, `error.rs` untouched |
| §6.5: real synthetic audio, ordered assembly, poison-to-DLQ gate | Not yet applicable to S6 alone | Worker/CLI/assembly are S7–S9; no live Deepgram request was made. These gates remain unverified |

## Findings, ordered by severity

All five implementation findings below originate in this commit. Their server impact
is latent until S7 enables `cloud-stt`; the library itself is callable now.

1. **High — vendor diagnostics could expose PHI or credentials.** Original
   `deepgram.rs:191` read the entire error body and retained its first 200 bytes in
   `PipelineError`. A vendor/proxy that echoes identifying content would pass it to
   callers. The planned worker persists failures through
   `er-server/src/repository.rs:663`; `routes.rs:227` returns `ChunkRow.error` to clients,
   and generic server errors are logged at `error.rs:48`. Truncation is not redaction.
   **Fixed:** discard error bodies and raw transport/JSON diagnostics; keep status and
   fixed classifications. HTTP tests cover synthetic PHI/secret sentinels and delayed
   error bodies (`deepgram/http_tests.rs:205`, `:223`, `:284`). This is a demonstrated
   unsafe data path, not evidence that an actual PHI disclosure occurred.

2. **High — redirects could forward raw audio outside the approved vendor boundary.**
   Original `deepgram.rs:123` used reqwest's default redirect policy. A 307/308 response
   could replay the POST body to its Location; removing authorization on cross-host
   redirects would not protect the audio itself. **Fixed:** fixed production endpoint,
   HTTPS-only client and no redirects (`pipeline:89`); synthetic sink tests cover
   301/302/303/307/308 and reject plain HTTP (`deepgram/http_tests.rs:246`, `:270`).
   There is no public endpoint override; the loopback override is compiled only for tests.

3. **High — malformed diarization could silently lose clinical content.** Original
   `deepgram.rs:379` preferred any nonempty utterance array; `:437` silently filtered
   malformed elements. `[{}]` produced a successful empty result even alongside a
   complete flat transcript. A mixed valid/invalid array produced a partial transcript.
   **Fixed:** validate every turn, use a nonblank flat fallback for malformed arrays,
   otherwise fail; preserve blank, well-formed silence. Three new regression tests
   failed on the original parser before remediation, including the numeric case below.

4. **Medium — unbounded response buffering and incomplete transport retry handling.**
   Original `deepgram.rs:195`/`:206` buffered arbitrary error/success bodies; the error
   truncation ran only after allocation. Large or interrupted vendor/proxy responses
   could exhaust worker memory or turn a recoverable interruption into an outer retry.
   **Fixed:** discard non-success bodies; bound successful wire content to 4 MiB before
   parsing, with and without Content-Length; retry body transport failures but not
   timeouts, oversized responses, or malformed complete JSON (`pipeline:208`, `:318`).
   The bound is sized for the documented ~60s chunks, not arbitrary multi-hour files.

5. **Medium — invalid vendor numbers could corrupt downstream metrics.** Original
   `deepgram.rs:409`/`:419` cast negative or enormous JSON numbers directly to `f32`,
   including infinity. **Fixed:** confidence must be in [0,1]; duration must be
   nonnegative and finite in the result representation. Unavailable/invalid confidence
   becomes None, duration uses the existing 0.0 sentinel (`pipeline:426`). S7 still
   needs checked seconds-to-INT-milliseconds conversion and missing-meter handling.

Additional hardening: validate key header syntax at construction, mark its HeaderValue
sensitive, and retain redacted Debug. Correct comments that implied metadata-free audio
was anonymous, that Serde structs reject unknown fields by default, that S4 accepted
empty objects, or that the server already enabled the cloud feature.

### Pre-existing dependency findings exposed by the review

Installed `cargo-audit 0.22.2` under the session's temporary directory after finding it
absent. RustSec snapshot `b50980aad8b8f14f77e25a97b32dd94bf008b0af`, updated
2026-09-09, initially reported **10 vulnerability/version matches**, three unsoundness
warnings and three yanked-package warnings. These dependency versions already existed
before this commit. The new cloud transcriber makes the reqwest → h2 path relevant to S6.

- Updated `h2` 0.4.13 → 0.4.16 (`Cargo.lock:1542`). A malicious HTTP/2 peer can exhaust
  resources with empty DATA frames when bodies are not actively drained; upstream rates
  this Low. A JSON byte cap alone does not fix a protocol-level frame queue.
  [Upstream advisory](https://github.com/hyperium/hyper/security/advisories/GHSA-q83h-524g-xf6h).
- Removed the S3/SQS SDKs' legacy `rustls` feature (`Cargo.toml:34`), which enabled
  Hyper 0.14 / h2 0.3 / rustls 0.21 / webpki 0.101 in addition to the active modern
  client. All other previous defaults, including S3 SigV4a and HTTP 1.x support, remain.
  Installed `aws-smithy-runtime-1.14.0/src/client/http.rs:85` confirms that the retained
  `default-https-client` uses Hyper 1.x with rustls/AWS-LC. TLS was not disabled.
- Updated the remaining `rustls-webpki` 0.103.9 → 0.103.13 (`Cargo.lock:2758`) for
  RUSTSEC-2026-0049/0098/0099/0104. The constraint-validation flaws require certificate
  misissuance; CRL flaws depend on CRL use. No exploit was attempted or observed.
  [RustSec constraint advisory](https://rustsec.org/advisories/RUSTSEC-2026-0099.html).
- Applied compatible patches for `anyhow` 1.0.102 → 1.0.103 and `event-listener`
  5.4.1 → 5.4.2 (`Cargo.lock:42`, `:1294`), resolving RUSTSEC-2026-0190/0221.
  Cargo also normalized compatible socket2/windows-sys dependency selections; its
  `--locked` check rejected manually retaining the old selections, so the generated
  resolution was retained. No AWS SDK version or public application API changed.

The final scan has **one unsuppressed vulnerability**: `rsa` 0.9.10,
RUSTSEC-2023-0071 (`Cargo.lock:2666`), with no published patch. Server authentication
verifies signatures with public keys (`auth/claims.rs:35`); private-key signing in this
repository is the offline development-token tool (`bin/mint_dev_token.rs:19`) and tests.
The network-observable private-key operation required for the advisory was not found in
the production request path. Keep this condition documented and reassess before adding
server-side RSA signing/decryption; the audit still exits 1.
[RustSec RSA advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).

Remaining warnings: `lru` 0.16.4 (RUSTSEC-2026-0253), and yanked `js-sys` 0.3.87,
`spin` 0.9.8, `wasm-bindgen` 0.2.110. LRU comes from S3 Express identity caching;
its installed source uses `CacheKey(String)` and `get_or_insert_mut`, not the advisory's
`pop` plus panic-in-key-Drop/catch-unwind trigger. Exploitability was not demonstrated;
an SDK/crypto/target-dependency maintenance update remains a follow-up. No advisory was
suppressed to manufacture a passing security gate.

## Decisions about additions and deviations

Retain `utterances=true`, `mip_opt_out=true`, injected credentials, optional Tokio for
async backoff, genuine-silence handling, and the pure parser. These have concrete value
without moving server dependencies onto standalone device builds. The vendor currently
documents `mip_opt_out=true` and processing-only retention for opted-out requests;
actual project configuration/contract execution remain unverified.
[Deepgram MIP documentation](https://developers.deepgram.com/docs/the-deepgram-model-improvement-partnership-program).

Retain the existing 60s deadline instead of changing behavior without latency evidence.
The original 30s guide remains visible. Its 181.5s STT ceiling fits the *documented*
300s lease, but the copied unit-test constant does not inspect the live queue or bound
S3/DB/assembly work. Worker integration must test the whole budget and duplicate delivery.

Remove vendor-body truncation and replace its two tests with safe-diagnostic tests.
Add loopback HTTP coverage despite the local notes' choice to defer all HTTP testing:
the application owns query/privacy flags, redirect policy, deadlines, body limits, and
retry decisions. The fixture adds no dependency and contacts no external service.

## HIPAA review and PHI trace

Current official sources checked on the review date: eCFR §164.312, displayed as current
through 2026-09-08, requires access control, audit controls, integrity, authentication,
and transmission security. Its encryption implementation specifications are addressable;
HHS explains that addressable does not mean optional. The stricter encryption/MFA changes
in the 2024 NPRM remain proposals in the sources checked, not effective requirements
used by this review. [eCFR §164.312](https://www.ecfr.gov/current/title-45/subtitle-A/subchapter-C/part-164/subpart-C/section-164.312),
[HHS Security Rule summary](https://www.hhs.gov/hipaa/for-professionals/security/laws-regulations/index.html),
[HHS NPRM fact sheet](https://www.hhs.gov/hipaa/for-professionals/security/hipaa-security-rule-nprm/factsheet/index.html).

HHS cloud guidance requires the applicable BAA and risk-management arrangements; encryption
or a training opt-out does not substitute for them. Removing metadata or using regexes is
not evidence of either Safe Harbor or Expert Determination de-identification.
[HHS cloud guidance](https://www.hhs.gov/hipaa/for-professionals/special-topics/health-information-technology/cloud-computing/index.html),
[HHS de-identification guidance](https://www.hhs.gov/hipaa/for-professionals/special-topics/de-identification/index.html).

| Surface | Repository evidence and remaining limitation |
|---|---|
| Authentication and tenant boundary | `auth/claims.rs:21` checks signed JWT/issuer/expiry and Cognito client/token type; `auth/resolver.rs:24` derives org from the database. Audio routes are behind `routes.rs:53`; upload checks patient org at `:150`, completion/status use scoped chunk lookup at `:300`/`:366`. Existing negative tests pass. Same-org care-team/role limits, resolver-cache revocation delay (300s), and emergency access need policy/integration work; no claim of patient-specific least privilege |
| Upload, storage and queue | Device/file bytes go directly to a 15-minute presigned PUT; S3 keys use linked UUIDs (`routes.rs:123`), queue messages carry chunk ID (`queue.rs`). These identifiers and URLs still require access protection. `verify_chunk_object` checks existence/positive length. Size ceilings, object version/checksum pinning, and encounter validation are pre-existing gaps for S7; a still-valid PUT can replace bytes after completion |
| Vendor | Planned worker fetches S3 bytes, calls this library, and receives PHI text/duration. No local file writes, external telemetry, or request logging in this module. HTTPS and request minimization are verified in code/tests; TLS negotiation with the real vendor, egress/proxy controls, memory dumps, project retention, key scope/rotation, and BAA are externally unverified |
| Persistence and down-sync | `audio_chunks.transcript`/`error` and subsequent `TranscriptReady` become durable PHI in Postgres, events and snapshots. Scoped repository methods exist (`repository.rs:422`, `:495`, `:613`, `:663`); the worker that calls them does not. ChunkView excludes transcript but includes error, which motivated sanitization at the vendor boundary |
| Logs and audit | S6 diagnostics now exclude content. Existing `ApiError::Internal` logs arbitrary causes; HTTP TraceLayer logs request context (`main.rs:18`). Event history and upload rows do not establish complete read/export/access auditing or log tamper protection. Audit events need actor, target, action/outcome and restricted retention; linked UUIDs are not automatically non-PHI |
| AI and exports | Pre-existing CLI `shift_manager.rs:171` applies regex reduction before `claude.rs:73`; previous sections also enter the prompt. `deidentify.rs:29` does not cover all identifying free text. `claude.rs:89` still retains raw vendor error bodies, which `shift_manager.rs:126` can log. Server AI is deferred to Step 7. Note/export routes are org-guarded; clipboard/CLI output, local SQLite events/WAL/snapshots (`er-store/src/db.rs`) remain PHI surfaces |
| Encryption, lifecycle and backups | No application encryption in SQLite; local compose services use development settings. Production TLS, S3/KMS, RDS encryption, IAM, secrets injection, backup protection and restore tests belong to Steps 8–9 and lack deployment evidence here. S3 Glacier transition is not deletion. Phase 5 compaction must cover original events, chunk transcripts, snapshots, object versions, replicas, exports and backups; a tombstone alone does not erase payloads |

The organizational risk assessment, BAAs (Deepgram/AWS/AI providers as applicable), workforce
access/training, recording-consent policy, incident procedures, retention schedule, backup
restoration, device controls and vendor assurances are **unverified**. Production use with PHI
must remain gated on that evidence. This review made no attempt to inspect real records.

## Verification

Commands ran outside the filesystem sandbox because its initialization failed with
`bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`; escalation succeeded.
Tests used synthetic fixtures and existing localhost Postgres/MinIO/ElasticMQ services.

| Command | Actual outcome |
|---|---|
| `cargo test -p er-pipeline --features cloud-stt` before changes | 26 passed |
| Same command with first three parser regressions, before fixes | 26 passed, 3 failed as expected |
| Same command after transcriber changes | **40 passed**, including 11 HTTP tests |
| `cargo test --workspace` with local test environment below | **186 passed**, including auth, tenant isolation, audio upload/completion, persistence and remote lifecycle |
| `cargo test --workspace --all-features --locked` with the same environment, after dependency patches | **218 passed**, including all 40 pipeline tests; zero failures or ignored tests |
| `SQLX_OFFLINE=true cargo check --workspace --all-features --locked` | Passed using checked-in SQL cache, including after dependency patches |
| `cargo check -p er-pipeline --locked`; `cargo tree -p er-pipeline -e normal --locked` | Passed; no reqwest or tokio in the default normal dependency tree |
| `cargo clippy -p er-pipeline --features cloud-stt --all-targets --no-deps --locked -- -D warnings` | Passed for the changed crate, including after dependency patches |
| `cargo clippy -p er-pipeline --features cloud-stt --all-targets` (exact S6 gate) | Passed with one pre-existing `er-domain` warning |
| Strict Clippy with `-D warnings` and without `--no-deps` | Fails on pre-existing `er-domain/src/note.rs:307` (`let_and_return`); untouched by this commit or remediation |
| `cargo fmt --all -- --check` | Fails on widespread pre-existing formatting, starting in `er-ai/src/claude.rs:1`; no broad reformat performed |
| `cargo fmt -p er-pipeline -- --check` | Fails only on the pre-existing import order in untouched `src/audio.rs:1` |
| `rustfmt --edition 2024 --check crates/er-pipeline/src/deepgram.rs crates/er-pipeline/src/deepgram/http_tests.rs`; `git diff --check` | Passed |
| `cargo audit --version` | Initially absent; subsequently installed version 0.22.2 into `/tmp/er-note-s6-review.KYzahQ/tools` |
| Temporary `cargo-audit audit --file Cargo.lock --db /tmp/er-note-s6-review.KYzahQ/advisory-db --json` | Initial scan: 10 vulnerability matches; final `--no-fetch` scan: 1 RSA advisory, 1 LRU warning, 3 yanked-package warnings; exit 1, detailed above |
| Real Deepgram / worker / deployment drills | Not run; S7–S9 and external vendor gates remain open |

The workspace invocation was:

```bash
env -u DEEPGRAM_API_KEY -u ANTHROPIC_KEY SQLX_OFFLINE=true \
  DATABASE_URL=postgres://postgres:dev@localhost:5433/ernote \
  AWS_ACCESS_KEY_ID=dev AWS_SECRET_ACCESS_KEY=devdevdev AWS_REGION=us-east-1 \
  AWS_EC2_METADATA_DISABLED=true S3_ENDPOINT_URL=http://localhost:9000 \
  SQS_ENDPOINT_URL=http://localhost:9324 cargo test --workspace --all-features --locked
```

Credentials above are the repository's public compose development values. SQLx creates
test databases and audio fixtures create unique synthetic buckets/queues; the existing
helpers do not clean up audio buckets/queues. Verification logs are under
`/tmp/er-note-s6-review.KYzahQ/` for this session. The final diff was reviewed; unchanged
device interfaces and existing local work were preserved.

## Concrete follow-ups outside S6

1. **S7–S9 worker:** authenticate the queue producer, derive org/patient/encounter from the
   persisted chunk, re-check encounter ownership/existence, pin fetched object identity/size,
   budget/renew the whole job lease, test duplicate/crash recovery, and publish transcripts
   idempotently. Keep provider-local speaker IDs scoped to their chunk; speaker 0 must not
   imply a stable clinician/patient role across separate requests.
2. **Step 7 AI and metering:** sanitize the pre-existing Claude error path, validate PHI
   handling for every prompt field, gate real PHI on the vendor/transport arrangement,
   and store per-attempt usage. Final chunk duration alone cannot reconstruct vendor charges
   from retries or a crash after a billable call. Handle unknown duration explicitly and
   check conversion before writing integer milliseconds.
3. **Steps 8–9 / pilot gate:** verify production identity configuration, least-privilege
   IAM/DB roles, care-team policy, audit coverage, TLS/KMS/secrets, vendor retention/BAAs,
   deletion across copies, restore/incident drills, and dependency scanning. Keep mock STT
   visibly development-only; absent credentials must not generate plausible mock notes in
   production. Record consent and organizational decisions with the responsible owners.
4. **Maintenance:** repair baseline fmt/Clippy failures separately and add an explicit
   cloud-feature CI lane; ordinary workspace tests still do not enable S6 until S7.
   Resolve/reassess the remaining RSA, LRU and yanked-package audit findings without
   assuming that a lockfile match alone proves exploitability in the application.
