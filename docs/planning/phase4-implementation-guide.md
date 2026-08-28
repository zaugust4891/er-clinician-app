# Phase 4 — Implementation Guide (Step-by-Step Playbook)

*2026-06-11 | Companion to `phase4.md` (the plan: decisions D1–D11, architecture, costs). This document is the **how**: every step broken into ordered sub-steps with commands, code skeletons, and external-service walkthroughs. Follow it top to bottom; each step ends with a gate you must pass before moving on.*

**Conventions used below**

- `$` = run in your shell from the repo root.
- Crate versions are "current major as of writing" — always take the latest patch (`cargo add` without a version pin does this).
- Code blocks are skeletons: enough to be unambiguous, not copy-paste-complete. You write the bodies.
- 🌐 boxes = external-service actions (things done in a browser/console, not in this repo).
- Estimates assume solo work, evenings/weekends pace. Steps 1–7 need **zero AWS spend** (everything runs against docker-compose).

```
Sequence & rough effort

Part 0  Accounts, tooling, BAAs (start procurement NOW — it's the long pole)   0.5 day + waiting
Step 1  Workspace split + error decomposition                                  1–2 days
Step 2  er-server skeleton + local dev environment                             1 day
Step 3  Async Repository + PostgresRepository + event_id                       2–3 days
Step 4  Auth middleware + tenancy                                              1–2 days
Step 5  Core API + sync protocol + CLI-as-client                               2–3 days
Step 6  Audio upload + SQS worker + Deepgram STT                               2–3 days
Step 7  Server-side AI: transports, routing, cached prompts, metering          2–3 days
Step 8  Terraform + CI/CD + first staging deploy                               3–4 days
Step 9  Hardening, alarms, drills                                              2 days
Step 10 Load test, cost dashboard, docs, OpenAPI handoff                       1–2 days
```

---

## Part 0 — Day-Zero Setup (do this first; some items have multi-week lead times)

### 0.1 Local tooling

```
$ rustup update                              # edition 2024 needs a current toolchain
$ cargo install sqlx-cli --no-default-features --features rustls,postgres
$ cargo install cargo-chef cargo-audit
$ docker --version                           # Docker Desktop or engine + compose v2
$ brew install awscli terraform k6           # or your distro's packages; terraform >= 1.9
$ aws --version && terraform version && k6 version
```

### 0.2 🌐 AWS account

1. Create a **fresh AWS account** dedicated to this product (don't reuse a personal account — it will be in scope for every security review). Use an email alias like `aws-root+ernote@…`.
2. Root user: enable **MFA immediately**, then stop using root.
3. **IAM Identity Center** (Console → IAM Identity Center → Enable): create yourself an `AdministratorAccess` permission-set user. From now on, `aws configure sso` and log in as that user:
   ```
   $ aws configure sso        # session name: ernote; start URL from Identity Center
   $ aws sso login --profile ernote-admin
   $ export AWS_PROFILE=ernote-admin
   ```
4. **Billing guardrails** (Console → Billing → Budgets): create a monthly budget of $100 with email alerts at 50/80/100%. Raise it when Step 8 starts (staging ≈ $150/mo).
5. Region: **us-east-1** everywhere (matches `infrastructure-cost-model.md` §1.1; Bedrock model availability is best there).

### 0.3 🌐 AWS BAA — sign it now (free, instant)

Console → **AWS Artifact** → Agreements → Account agreements → **AWS Business Associate Addendum** → review → accept. This is self-service and free. Nothing PHI-shaped touches AWS before this is accepted. Keep the acceptance PDF — hospital privacy officers ask for it.

> The BAA covers *HIPAA-eligible services only*. Everything in this plan (ECS/Fargate, RDS, S3, SQS, Cognito, KMS, Secrets Manager, CloudWatch, CloudTrail, ALB, Bedrock) is on the eligible list — verify the current list at signing (search "AWS HIPAA eligible services reference").

### 0.4 🌐 Bedrock model access

Console → Amazon Bedrock (us-east-1) → **Model access** → Manage model access → request the **Anthropic** models (Claude Sonnet 4.6, Claude Haiku 4.5). Access for Anthropic models is typically granted in minutes after submitting the use-case form. Note the exact **model IDs / inference profile IDs** shown in the console (expected: `anthropic.claude-sonnet-4-6` and `anthropic.claude-haiku-4-5`; some models route via `us.`-prefixed cross-region inference profiles — use whatever the console's "API request" example shows). You won't call Bedrock until Step 7, but get access granted now.

### 0.5 🌐 Deepgram — start procurement now (longest external lead time)

1. Sign up at deepgram.com (free credit tier is fine for Steps 6 dev work **with synthetic/non-PHI audio only**).
2. Console → create a Project (`er-note-staging`) → API Keys → create a key. Save it; it goes in `.env` for dev and Secrets Manager for staging.
3. **Contact Deepgram sales about the HIPAA/BAA tier** (enterprise agreement). Ask explicitly for: (a) signed BAA, (b) the no-storage / no-training data-retention configuration, (c) HIPAA-tier pricing for Nova-3 Medical (cost model assumed ~$0.009/min — confirm current). This negotiation can take weeks; until the BAA is signed, **only synthetic audio** goes to Deepgram.
4. Fallback if procurement stalls: AWS **Transcribe Medical** is already under your AWS BAA with zero paperwork (≈8× the per-minute cost; acceptable for a 10-doc pilot). The `SpeechToText` trait in Step 6 makes this a swap, not a rewrite.

### 0.6 🌐 Domain + GitHub

1. Buy a domain (Route 53 → Registered domains, ~$13/yr, keeps DNS in-account) or delegate an existing one to a Route 53 hosted zone. You'll use `api.staging.<domain>` and `api.<domain>`.
2. GitHub repo: enable branch protection on `master` (require PR + CI green). Settings → Actions → General → set workflow permissions to "Read repository contents" (the deploy role comes from OIDC in Step 8, not from secrets).

### 0.7 Gate — Part 0

- [ ] AWS account with SSO admin user, MFA on root, budget alarm set
- [ ] AWS BAA accepted in Artifact (PDF saved)
- [ ] Bedrock access to Sonnet 4.6 + Haiku 4.5 granted; exact model IDs recorded
- [ ] Deepgram account + dev API key; BAA conversation started (email thread exists)
- [ ] Domain in Route 53
- [ ] All local tools installed

---

## Step 1 — Workspace Split (Option 2) + Error Decomposition

**Objective:** convert the single crate into a Cargo workspace whose dependency graph *is* the architecture, so `er-server` (Step 2) can depend on domain/application without dragging in rusqlite, the CLI, or mocks. One real refactor hides inside the mechanical move: **splitting `AppError`**, which currently embeds `rusqlite::Error` and would otherwise poison `er-domain`'s purity.

### 1.1 Create the workspace shell

```
$ mkdir crates
$ git mv src/domain          crates/er-domain/src     # then add lib.rs per crate
$ git mv src/application     crates/er-app/src
$ git mv src/infrastructure/persistence crates/er-store/src
$ git mv src/infrastructure/ai          crates/er-ai/src
$ git mv src/infrastructure  crates/er-pipeline/src   # what remains: audio.rs, stt.rs
$ git mv src/interface src/main.rs src/bin crates/er-cli/src
```

Root `Cargo.toml` becomes a virtual manifest:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.dependencies]        # single source of version truth
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "v7", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
thiserror = "2"
tracing = "0.1"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
# crate-specific deps (rusqlite, reqwest, axum, sqlx…) declared per-crate
```

Per-crate manifests (dependency rule enforced by the compiler now):

| Crate | Internal deps | External deps (only) |
|---|---|---|
| `er-domain` | — | serde, serde_json, uuid, chrono, thiserror, tracing |
| `er-store` | er-domain | rusqlite, sha2, serde_json, async-trait, tokio (rt for Step 3) |
| `er-ai` | er-domain | reqwest, serde, tokio, regex (deidentify) |
| `er-pipeline` | er-domain | (traits + mocks only — keep it nearly dep-free) |
| `er-app` | er-domain, er-store, er-ai, er-pipeline | dashmap, tokio, tracing |
| `er-cli` | er-app (+ everything transitively) | dotenvy, rusqlite (for open_db) |

> `er-store` holding the `Repository` *trait* and the SQLite impl together is fine for now; Step 3 adds `PostgresRepository` to `er-server` rather than here, so the device build never compiles sqlx. If that feels lopsided later, move the trait to `er-domain` — it has no I/O in its signature.

### 1.2 The error split (the only non-mechanical part — budget half a day)

Current `error.rs` is one `AppError` with variants wrapping `rusqlite::Error`, `reqwest::Error`, etc. Decompose:

1. **`er-domain/src/error.rs`** — `DomainError` { `InvalidTransition{from, event}`, `NoteImmutable`, `ParseError{context, source: Box<dyn Error + Send + Sync>}` … }. Domain functions (`ingest_result`, state handlers) change `AppResult<T>` → `Result<T, DomainError>`.
2. **`er-store`** — `StoreError` { `Db(rusqlite::Error)`, `Serde(serde_json::Error)`, `ChecksumMismatch`, … }; `Repository` methods return `Result<T, StoreError>`.
3. **`er-ai`** — `AiError` { `Api(reqwest::Error)`, `Summarization(String)`, `EmptyResponse`, … }.
4. **`er-app/src/error.rs`** — the composite, preserving today's public surface:
   ```rust
   #[derive(Debug, thiserror::Error)]
   pub enum AppError {
       #[error(transparent)] Domain(#[from] er_domain::DomainError),
       #[error(transparent)] Store(#[from] er_store::StoreError),
       #[error(transparent)] Ai(#[from] er_ai::AiError),
       #[error("{0}")] Other(String),
   }
   pub type AppResult<T> = Result<T, AppError>;
   ```
   `?` keeps working everywhere in `er-app`/`er-cli` thanks to the `#[from]`s. Expect ~15 signature touch-ups; the compiler is the worklist.

### 1.3 Move the integration tests into their owning crates

```
tests/persistence_integration.rs → crates/er-store/tests/persistence.rs
tests/crash_recovery.rs          → crates/er-app/tests/crash_recovery.rs
```

The `execute_raw` corruption helper on `SqliteRepository` gets the planned `#[cfg(feature = "test-util")]` gate now that the crate boundary exists; `er-store/Cargo.toml` declares the feature, the test enables it via `[dev-dependencies] er-store = { path = ".", features = ["test-util"] }`… (self-dev-dependency trick) or simpler: keep it `#[doc(hidden)] pub` as today and move on.

### 1.4 Gate — Step 1

```
$ cargo build --workspace
$ cargo test  --workspace        # every pre-existing test passes
$ cargo run -p er-cli            # interactive CLI behaves exactly as before
$ cargo tree -p er-domain | grep -E "rusqlite|reqwest|tokio"   # → no matches
```
Commit as a single `Restructure: workspace split` commit (mirrors the Step 1–9 restructure series you already did for layers).

---

## Step 2 — `er-server` Skeleton + Local Dev Environment

**Objective:** a containerized axum binary with `/healthz`, structured JSON logging, config from env, graceful shutdown — plus a `docker-compose` that emulates every AWS dependency locally (Postgres, S3→MinIO, SQS→ElasticMQ), so Steps 3–7 never need an AWS connection.

### 2.1 Crate

```
$ cargo new crates/er-server --bin --vcs none
$ cd crates/er-server
$ cargo add axum tokio tower tower-http --features tower-http/trace,tower-http/timeout
$ cargo add tracing tracing-subscriber --features tracing-subscriber/env-filter,tracing-subscriber/json
$ cargo add serde serde_json uuid chrono thiserror async-trait
$ cargo add er-app --path ../er-app    # pulls er-domain/er-store/er-ai transitively
```

`src/main.rs` skeleton:

```rust
mod config;      // Config::from_env(): bind addr, database_url, s3, sqs, auth mode…
mod routes;      // Router assembly
mod state;       // AppState { pool, s3, sqs, cfg } — the "composition root" pattern
mod error;       // ApiError: maps AppError → (StatusCode, problem+json); never leaks internals

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();                                   // JSON in prod, pretty locally (RUST_LOG)
    let cfg = config::Config::from_env()?;
    let state = state::AppState::build(&cfg).await?;
    let app = routes::router(state)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(30)));
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;  // SIGTERM for ECS
    Ok(())
}
```

Two approck-borrowed patterns land here (per the feasibility analysis): every request gets a tracing span tagged `request_id` + later `org_id`/`patient_id`; `ApiError` separates external message (`"problem+json"` body) from internal detail (log line with correlation id).

### 2.2 Dockerfile (multi-stage, cargo-chef)

```dockerfile
FROM rust:1-slim AS chef
RUN cargo install cargo-chef && apt-get update && apt-get install -y pkg-config libssl-dev
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release -p er-server --recipe-path recipe.json   # cached deps layer
COPY . .
RUN cargo build --release -p er-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/er-server /usr/local/bin/
USER 65532:65532
ENTRYPOINT ["er-server"]
```

### 2.3 docker-compose for local AWS-alikes

`docker-compose.yml` at repo root:

```yaml
services:
  postgres:
    image: postgres:16
    environment: { POSTGRES_PASSWORD: dev, POSTGRES_DB: ernote }
    ports: ["5432:5432"]
  minio:                                # S3-compatible
    image: minio/minio
    command: server /data --console-address ":9001"
    environment: { MINIO_ROOT_USER: dev, MINIO_ROOT_PASSWORD: devdevdev }
    ports: ["9000:9000", "9001:9001"]
  elasticmq:                            # SQS-compatible
    image: softwaremill/elasticmq-native
    ports: ["9324:9324"]
```

The AWS SDKs honor endpoint overrides — wire `Config` so `S3_ENDPOINT_URL` / `SQS_ENDPOINT_URL`, when set, are passed to the SDK builders (`aws_sdk_s3::config::Builder::endpoint_url`) with `force_path_style(true)` for MinIO. Locally those point at the compose services; in AWS they're unset.

`.env.example` (commit it; `.env` is gitignored):

```
DATABASE_URL=postgres://postgres:dev@localhost:5432/ernote
S3_ENDPOINT_URL=http://localhost:9000
SQS_ENDPOINT_URL=http://localhost:9324
AUTH_MODE=dev            # Step 4: dev | cognito
ANTHROPIC_KEY=...        # dev-only; prod uses Bedrock IAM
DEEPGRAM_KEY=...
```

### 2.4 Gate — Step 2

```
$ docker compose up -d
$ cargo run -p er-server &      # then:
$ curl -s localhost:8080/healthz          # → 200 {"status":"ok"}
$ docker build -t er-server . && docker run --rm -p 8080:8080 --env-file .env er-server
$ curl -s localhost:8080/healthz          # container also healthy
```

---

## Step 3 — Async `Repository` + `PostgresRepository` + `event_id`

> 📖 **Deep dive:** `phase4-step3-deep-dive.md` explains every concept in this step from the ground
> up (sync vs async I/O, `#[async_trait]`/dyn-compatibility, idempotency & UUIDv7, sqlx, generic test
> suites) and maps each change to the exact file/line in this repo. Read it once before coding.

**Objective:** make the persistence trait async (the load-bearing refactor of the whole phase), implement it on Postgres with sqlx, and introduce the client-stamped `event_id` that makes sync idempotent (plan D10).

### 3.1 Why async, and the exact ripple list

`Repository` is sync today (fits rusqlite); sqlx is async, and calling `block_on` inside axum handlers panics. Converting the trait keeps **one trait, N impls** instead of a parallel async hierarchy:

```rust
#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn append_event(&self, patient_id: PatientId, event_id: Uuid, event: NoteEvent) -> Result<u64, StoreError>;
    async fn get_events(&self, patient_id: PatientId) -> Result<Vec<StoredEvent>, StoreError>;
    async fn get_events_since(&self, patient_id: PatientId, seq: u64) -> Result<Vec<StoredEvent>, StoreError>;
    async fn save_snapshot(&self, patient_id: PatientId, seq: u64, snapshot: &PatientNote) -> Result<(), StoreError>;
    async fn get_latest_snapshot(&self, patient_id: PatientId) -> Result<Option<(u64, PatientNote)>, StoreError>;
    // event_count / snapshot_count: keep, they're test-support
}
```

`#[async_trait]` (not RPITIT) because the codebase uses `Arc<dyn Repository>` — dyn-compatibility required. Ripples, in dependency order:

1. **`SqliteRepository`**: method bodies unchanged (no `.await` inside — the mutex+rusqlite calls are fast and sync); just add `async` + signature change. The `std::sync::Mutex` stays correct *because* nothing awaits while holding it (the existing invariant, now compiler-adjacent).
2. **`EventBus::publish` → `async fn`** (it awaits `repo.append_event` / `save_snapshot`). DashMap projection code unchanged — but keep the existing rule: clone out of the map before any `.await`; never hold a DashMap guard across an await point.
3. **`load_patient_note` → `async fn`.**
4. **`ShiftManager`**: already owns `runtime: Arc<Runtime>` (Phase 3 Step 0). Sync command methods wrap the now-async publishes exactly the way `summarize_encounter` already wraps Claude calls: `self.runtime.block_on(self.bus.publish(...))`. Net diff is small.
5. **`NoteEvent` identity**: do **not** add a field to every enum variant. The id lives in the envelope: `EventBus::publish` stamps `Uuid::now_v7()` and passes it to `append_event`; `StoredEvent` gains `pub event_id: Uuid`. SQLite schema (`er-store/db.rs`): add `event_id TEXT NOT NULL UNIQUE` to `events` (no prod data exists — just change the `CREATE TABLE`). Insert uses `INSERT OR IGNORE` and treats a duplicate as success returning the existing row's sequence (idempotency on-device too).
6. **Tests**: `#[test]` → `#[tokio::test]` where they touch the repo/bus; the crash-recovery suite gets `async fn` plumbing.

### 3.2 sqlx setup in `er-server`

> 📖 **Hands-on walkthrough:** `phase4-step3-postgres-guide.md` implements this whole sub-step
> end to end — dependency flags, the `0001` migration (with the `org_id`-FK and `TEXT`-vs-`JSONB`
> snapshot decisions explained), the `StoreError` change for sqlx errors, every `PostgresRepository`
> method with full code + the *why*, the `.sqlx` offline-cache workflow, and `#[sqlx::test]` testing.

```
$ cargo add -p er-server sqlx --features postgres,runtime-tokio,tls-rustls,uuid,chrono,json,migrate
$ mkdir crates/er-server/migrations
$ sqlx migrate add events_and_snapshots     # 0001_…
```

Migration `0001` = the `events` + `snapshots` DDL from `phase4.md` §4 (tenancy columns included now, FK constraints added in 0002 when `orgs` exists — or simply order 0001=orgs/clinicians, 0002=events/snapshots; pick one and keep FKs). Subsequent migrations land with their steps: `0003 shifts/patients` (Step 5), `0004 audio_chunks` (Step 6), `0005 usage_log` (Step 7).

`PostgresRepository` sketch:

```rust
pub struct PostgresRepository { pool: sqlx::PgPool, org_id: Uuid }   // org-scoped ctor — D9
impl PostgresRepository {
    pub fn for_org(pool: PgPool, org_id: Uuid) -> Self { … }
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn append_event(&self, patient_id: PatientId, event_id: Uuid, event: NoteEvent) -> Result<u64, StoreError> {
        // ON CONFLICT (event_id) DO NOTHING ⇒ idempotent retries (sync protocol, D10).
        // On conflict, SELECT the existing sequence_id so the caller still gets a stable answer.
        let rec = sqlx::query!(
            r#"INSERT INTO events (event_id, org_id, patient_id, event_type, payload, clinical_occurred_at)
               VALUES ($1,$2,$3,$4,$5,$6)
               ON CONFLICT (event_id) DO NOTHING
               RETURNING sequence_id"#,
            event_id, self.org_id, patient_id, event.event_type(),
            serde_json::to_value(&event)?, event.get_clinical_event_occurred_at(),
        ).fetch_optional(&self.pool).await?;
        match rec { Some(r) => Ok(r.sequence_id as u64),
                    None => self.sequence_for(event_id).await }     // duplicate ⇒ look up
    }
    // get_events / get_events_since: WHERE org_id = $1 AND patient_id = $2 ORDER BY sequence_id
    // save/get snapshot: jsonb + the same SHA-256 hex checksum logic, then note.hydrate()
}

```

Run `cargo sqlx prepare --workspace` and commit `.sqlx/` so CI type-checks queries without a live DB.

### 3.3 Make the persistence test suite generic, run it against both impls

Refactor `er-store/tests/persistence.rs` so the assertions live in `async fn suite(repo: Arc<dyn Repository>)`, then:

- SQLite entry point: in-memory connection as today.
- Postgres entry point (in `er-server/tests/`): `#[sqlx::test(migrations = "./migrations")] async fn postgres_suite(pool: PgPool)` — the `sqlx::test` macro creates an isolated DB per test against `DATABASE_URL` (your compose Postgres). Same for the **crash-recovery suite**: that one passing against Postgres is the proof that replay/snapshot/checksum semantics survived the port.

### 3.4 Gate — Step 3

```
$ cargo test --workspace                          # SQLite paths still green (now async)
$ docker compose up -d postgres && sqlx migrate run --source crates/er-server/migrations
$ DATABASE_URL=… cargo test -p er-server          # generic suite + crash recovery vs Postgres
# Idempotency proof: a test appends the same (event_id, event) twice → event_count == 1,
# both calls return the same sequence_id.
```

---

## Step 4 — Auth Middleware + Tenancy

> 📖 **Deep dive:** `phase4-step4-deep-dive.md` explains every concept in this step from the ground
> up (stateless auth & JWTs, RS256/asymmetric signing, JWKS & key rotation, the validation pipeline
> and the attack each check stops, axum middleware/extractors, dev-vs-cognito modes, tenancy by
> construction, and why cross-tenant access returns 404 not 403) and maps each change to the exact
> file/line in this repo. Read it once before coding.

**Objective:** JWT-validating middleware with two modes — `dev` (local keypair, no AWS needed) and `cognito` (JWKS) — plus the `orgs`/`clinicians` tables and the rule that org scoping happens in the repository constructor, never in handlers.

### 4.1 Claims, extractor, middleware

```
$ cargo add -p er-server jsonwebtoken   # v9
```

```rust
#[derive(Deserialize)]
struct Claims { sub: String, 
                #[serde(rename = "custom:org_id")] 
                org_id: Option<Uuid>,
                token_use: Option<String>, 
                exp: usize, 
                iss: String, 
                client_id: Option<String> 
            }

pub struct AuthedClinician { pub clinician_id: Uuid, pub org_id: Uuid }   // axum extractor
```

Middleware flow: `Authorization: Bearer` → decode header → pick key by `kid` from a cached JWKS → validate signature + `exp` + `iss` (+ `token_use == "access"` and `client_id` in cognito mode) → map `sub` → `clinicians` row (in-process moka/`HashMap` cache, 5-min TTL) → insert `AuthedClinician` into request extensions. 401 on any failure, with the reason only in logs.

- **dev mode:** generate an RS256 keypair once (`openssl genrsa`/`openssl rsa -pubout`), serve a static local JWKS JSON from disk, and add a tiny `er-server --mint-dev-token --sub test-doc --org <uuid>` subcommand (or `scripts/dev-token.sh`) that signs a 24-h token. CI and local curl use this; zero AWS.
- **cognito mode:** JWKS URL is `https://cognito-idp.us-east-1.amazonaws.com/{POOL_ID}/.well-known/jwks.json`; fetch at boot, refresh on unknown `kid`.

### 4.2 Tenancy tables + enforcement shape

Migration `000X_tenancy`: `orgs`, `clinicians` per `phase4.md` §4. Enforcement rule (write it as a doc-comment on `AppState`): *handlers never see a `PgPool` — they see a per-request `PostgresRepository::for_org(pool, authed.org_id)` (and org-scoped query helpers for non-event tables). There is no code path that queries without an org.*

### 4.3 🌐 Sandbox Cognito user pool (manual, 20 minutes — Terraform replaces it in Step 8)

Console → Cognito → Create user pool:
1. Sign-in: email. MFA: optional (turn required later for prod).
2. **Custom attribute**: `org_id` (string, immutable).
3. App client: *public client, no secret*; enable auth flow `ALLOW_USER_PASSWORD_AUTH` (so the CLI can log in without a browser).
4. Create a test org row in your DB, then a test user, and set the attribute + a permanent password:
   ```
   $ aws cognito-idp admin-create-user --user-pool-id $POOL --username drjoey@example.com \
       --user-attributes Name=email,Value=drjoey@example.com Name=custom:org_id,Value=$ORG_UUID
   $ aws cognito-idp admin-set-user-password --user-pool-id $POOL --username drjoey@example.com \
       --password '…' --permanent
   $ aws cognito-idp initiate-auth --auth-flow USER_PASSWORD_AUTH --client-id $CLIENT \
       --auth-parameters USERNAME=drjoey@example.com,PASSWORD='…' \
       --query 'AuthenticationResult.AccessToken' -r    # → token for curl
   ```

### 4.4 Gate — Step 4

- [ ] No/garbage token → 401; dev token → 200 on `GET /v1/me`
- [ ] Cognito access token (sandbox pool) → 200, correct clinician/org resolved
- [ ] **Cross-tenant test fails closed:** seed two orgs; org-A token requesting an org-B patient id → 404 (not 403 — don't leak existence)

---

## Step 5 — Core API + Sync Protocol + CLI-as-Client

**Objective:** the endpoint set from `phase4.md` §5 (shifts, patients, event sync, note projection, export), and a `RemoteRepository` so the **existing CLI becomes the API's first client** — the contract is proven before Swift exists.

### 5.1 Handlers (order of implementation)

1. `POST /v1/patients` + `GET /v1/shifts/{id}` + `POST /v1/shifts` / `…/end` — plain CRUD on the new tables (migration `shifts/patients/shift_patients`). `shift.json` semantics move here; `position` column preserves the ShiftManager stable-listing invariant.
2. `POST /v1/patients/{id}/events` — **the up-sync.** Body: `[{event_id, event}]` (the `NoteEvent` serde JSON is already the wire format). Per batch, in one transaction: validate patient belongs to org → for each, `append_event` (idempotent) → project through the server-side `EventBus` instance (persist-first ordering and snapshot triggers identical to Phase 3) → respond `[{event_id, sequence_id}]`.
3. `GET /v1/patients/{id}/events?since=N` — **the down-sync**: `get_events_since`, paginated (`limit` 500, return `next_since`). Server-authored events (STT/AI, Steps 6–7) flow through this same cursor — no special cases.
4. `GET /v1/patients/{id}/note` — `load_patient_note` (snapshot fast-path) → a `NoteDto` (don't serialize the aggregate raw; the DTO is flatter and hides `section_history` internals — the approck-analysis DTO rule, applied to HTTP a phase early).
5. `GET /v1/patients/{id}/note/export` — reuse `er-cli`'s `export::format_note` (move `export.rs` into `er-app` or a small `er-export` so both CLI and server call it; it's a presentation function with no CLI deps).

Add `utoipa` annotations as you write each handler (`$ cargo add -p er-server utoipa utoipa-swagger-ui`) — retrofitting OpenAPI later is the kind of chore that never happens.

### 5.2 `RemoteRepository` in `er-cli`

```rust
pub struct RemoteRepository { http: reqwest::Client, base: Url, token: String, patient_org_ctx: () }

#[async_trait]
impl Repository for RemoteRepository {
    async fn append_event(&self, pid, event_id, event) -> … {
        // POST /v1/patients/{pid}/events  [{event_id, event}] → take sequence_id from response
    }
    async fn get_events_since(&self, pid, seq) -> … { /* GET ?since= */ }
    // get_events = get_events_since(0); snapshots: server-side concern → save_snapshot is a
    // no-op Ok(()), get_latest_snapshot returns Ok(None) (forces full replay client-side — fine)
}
```

CLI wiring: `er-cli --server https://localhost:8080 --token $(scripts/dev-token.sh)` constructs `ShiftManager` with `RemoteRepository` instead of `SqliteRepository`; **everything else is untouched** — that's the whole point of the trait.

### 5.3 Gate — Step 5 (scripted demo, keep it as `scripts/demo-remote.sh`)

1. `docker compose up -d`, run server, mint token.
2. CLI #1: `new "chest pain"` → `switch 1` → `start ambient` → `stop` → `lab Troponin 0.04 ng/mL` → `dispo discharge "f/u PCP"`.
3. CLI #2 (separate process, same token): `reload` → `note` shows the same note — projected from **server** events. That's Phase 2's crash-recovery story with the cloud as the disk.
4. Kill the server mid-batch and re-run the upload: no duplicate events (idempotency at the protocol level, not just the DB level).
5. `cargo test -p er-server` includes a round-trip API test (axum `Router` + `tower::ServiceExt::oneshot`, or full `TestServer`).

---

## Step 6 — Audio Upload + SQS Worker + Deepgram STT

**Objective:** presigned S3 uploads, an SQS-driven worker service, and a `DeepgramTranscriber` — with the trait seam that keeps Transcribe Medical / on-device Whisper swappable.

### 6.1 The async STT seam

The existing `Transcriber` trait is sync, PCM-oriented, and device-shaped — leave it alone for `er-pipeline` mocks. Server-side, define in `er-pipeline` (feature `cloud-stt`) or `er-server`:

```rust
#[async_trait]
pub trait SpeechToText: Send + Sync {
    async fn transcribe(&self, audio: bytes::Bytes, mime: &str) -> Result<TranscriptResult, SttError>;
}
```

`TranscriptResult` is reused from `er-pipeline::stt` (move it next to the trait). Impls: `DeepgramStt`, later `TranscribeMedicalStt`, and `MockStt` for tests.

### 6.2 Upload endpoints + S3

```
$ cargo add -p er-server aws-config aws-sdk-s3 aws-sdk-sqs
```

- `POST /v1/patients/{id}/encounters/{eid}/audio` → insert `audio_chunks` row (`status='uploaded'` pending) with a `chunk_index` from the client → presign a PUT (`PresigningConfig::expires_in(15 min)`, key `org/{org}/patient/{pid}/enc/{eid}/{index}.ogg`, enforce `content-type: audio/ogg`) → return `{chunk_id, upload_url}`.
- `POST /v1/audio/{chunk_id}/complete` → verify object exists (`head_object`) → `status='queued'` → send SQS message.
- Job message enum (serde-tagged, one queue `stt-jobs`):
  ```rust
  #[derive(Serialize, Deserialize)] #[serde(tag = "kind")]
  enum Job { TranscribeChunk { chunk_id: Uuid },
             GenerateMdm     { org_id: Uuid, patient_id: Uuid, job_id: Uuid } }   // Step 7
  ```
- Local dev: create the queue once against ElasticMQ — `aws --endpoint-url http://localhost:9324 sqs create-queue --queue-name stt-jobs` (put it in a `scripts/dev-bootstrap.sh` together with the MinIO bucket: `aws --endpoint-url http://localhost:9000 s3 mb s3://ernote-audio-dev`).

### 6.3 Worker service (same crate, second binary)

`crates/er-server/src/bin/worker.rs`: long-poll loop — `receive_message(wait_time=20, max=5)` → dispatch → `delete_message` on success; on error, **don't delete** (visibility timeout returns it; after `maxReceiveCount=3` it lands in the DLQ → alarm in Step 9). Handler for `TranscribeChunk`:

1. Fetch object from S3 → `DeepgramStt::transcribe`.
2. Append `{chunk_index, text}` to the encounter's transcript assembly (a small `encounter_transcripts` working table, or accumulate in `audio_chunks.text` and concatenate by index — choose the table; it keeps chunks queryable for debugging).
3. If the encounter is stopped and all chunks for it are `done` → publish `NoteEvent::transcript_ready(...)` through the server `EventBus` (which then triggers Step 7's summarize path) → mark chunks `done`.
4. Failures: chunk → `status='failed'`, transcript proceeds with a `[audio gap]` marker — Phase 3's "best-effort, never lose the encounter" rule, extended to chunks.

### 6.4 `DeepgramStt`

```rust
// POST https://api.deepgram.com/v1/listen?model=nova-3-medical&smart_format=true&diarize=true&punctuate=true
// headers: Authorization: Token {DEEPGRAM_KEY}; Content-Type: audio/ogg
// body: raw audio bytes
// response: results.channels[0].alternatives[0] → { transcript, confidence }
// With diarize=true, words[] carry speaker indices — format "Speaker 0: … / Speaker 1: …"
// (the summarizer prompt benefits from speaker turns).
```

Wrap with: 30s timeout, 2 retries with backoff on 429/5xx, and **log duration + cost into `usage_log`** (`kind='stt'`, `stt_seconds`, `cost_micros = seconds/60 * rate`).

> 🌐 **Deepgram checklist before any real-patient audio:** BAA signed (Part 0.5) · project configured for no-storage/no-training · key stored in AWS Secrets Manager (`/ernote/staging/deepgram`) and injected into the worker task as an ECS secret (Step 8) — never baked into the image.

### 6.5 Gate — Step 6

- [ ] `scripts/demo-audio.sh`: mint token → register patient → `start` → request upload URL → `curl -T sample.ogg "$URL"` → `complete` → watch worker logs → `GET /v1/patients/{id}/events?since=0` shows `TranscriptReady` with real Deepgram text (a recorded *synthetic* clinical vignette — write one, ~60s, two speakers)
- [ ] Out-of-order chunk upload (2 then 1) → transcript assembled in index order
- [ ] Poison test: upload garbage bytes → 3 receives → message in DLQ; note untouched except `[audio gap]`

---

## Step 7 — Server-Side AI: Transports, Routing, Cached Prompts, Metering

**Objective:** move generation from the CLI process into the worker, behind a transport that swaps Direct-API ↔ Bedrock by config; expand prompts past the cache minimum; meter every call.

### 7.1 `ClaudeTransport` in `er-ai`

```rust
pub enum ClaudeTransport {
    DirectApi { client: reqwest::Client, api_key: String },          // dev (today's claude.rs)
    Bedrock   { client: aws_sdk_bedrockruntime::Client },            // staging/prod (IAM, no key)
}
impl ClaudeTransport {
    pub async fn messages(&self, model: &ModelId, req: MessagesRequest) -> Result<MessagesResponse, AiError> { … }
}
```

- **Direct path:** today's `call_claude` body, plus: `system` becomes a *block array* so the system block can carry `cache_control: {"type":"ephemeral"}`; read `usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`) from the response instead of discarding it.
- **Bedrock path:** `invoke_model` with the Anthropic-native JSON body — same Messages schema plus `"anthropic_version": "bedrock-2023-05-31"`, **no `model` field in the body** (the model is the `modelId` request parameter, e.g. `anthropic.claude-sonnet-4-6`). Use the IDs you recorded in Part 0.4. `cargo add -p er-ai aws-sdk-bedrockruntime aws-config`.
- `ModelId` resolves per transport: `("claude-sonnet-4-6", "anthropic.claude-sonnet-4-6")` etc., from config.

### 7.2 Routing + prompt expansion

- `SummarizerConfig { summary_model: ModelId /* haiku */, mdm_model: ModelId /* sonnet */ }` threaded into `ClaudeSummarizer`. **Ship the pilot all-Sonnet**; flip summaries to Haiku only after the A/B in plan D3.
- Move prompts out of string literals into `er-ai/prompts/{summarize_system.md, mdm_system.md}` via `include_str!`. Expand `mdm_system.md` with: the 2021 E/M MDM level table (problems/data/risk × straightforward→high), explicit "cite administered medications and assigned diagnoses" instruction (Phase 3 Step 6 note), and 2–3 de-identified exemplar MDMs. Target: **≥ 2,300 tokens** — past Sonnet 4.6's 2,048-token minimum cacheable prefix with margin (Haiku's minimum is 4,096: do the same for the summary prompt if/when summaries move to Haiku, or accept uncached Haiku — it's the cheap model anyway). Volatile content (transcript, objective data, prior sections) stays in the *user* message, after the cached prefix.
- Worker handlers: `TranscriptReady` consequence → de-identify → `summarize_encounter` → publish `EncounterSummarized`; `GenerateMdm` job (enqueued by `POST /v1/patients/{id}/mdm`, which returns `202 {job_id}`) → assemble objective data (Phase 3 Step 6 helper, unchanged) → de-identify → `generate_mdm` → publish `MdmGenerated` **then** `TriggerMDM` (crash-ordering rule preserved) → mark job done. `GET /v1/jobs/{id}` reads a small `jobs` table.

### 7.3 Metering

After every transport call, insert `usage_log` (migration `usage_log` + `jobs`): model, all four token counts, computed `cost_micros` from a hardcoded `price_table.rs` (`sonnet-4-6: $3/$15 per MTok, haiku-4-5: $1/$5, cache read 0.1×, cache write 1.25×` — with a comment to re-verify quarterly). This table is the COGS dashboard *and* the future billing meter.

### 7.4 Gate — Step 7

- [ ] Full server-side lifecycle test (the Phase 4 flagship test): register → upload 2 encounter audios (MockStt in CI) → summaries land as `EncounterSummarized` events → `lab/med/dx/dispo` via event sync → `POST mdm` → poll job → `MdmGenerated` + phase `Finalizing` → `sign` → `export` returns a complete note. Runs in CI with `MockStt` + `MockSummarizer`; runs manually with the real stack.
- [ ] **Cache proof:** two consecutive real MDM calls → second row in `usage_log` has `cache_read_tokens > 0`. If 0: prefix below minimum or a byte-level difference in the prefix — fix before proceeding (this is plan D4's whole point).
- [ ] Bedrock path proven manually once: `TRANSPORT=bedrock cargo run -p er-server --bin worker` with SSO creds → one summarize call succeeds (needs Part 0.4 access).
- [ ] De-id assertion test moved server-side: patient name/label never appears in the string handed to the transport (Phase 3 criterion #10).

---

## Step 8 — Terraform + CI/CD + Staging Deploy

**Objective:** the whole `phase4.md` §6 table as code, two environments, and a pipeline that goes commit → image → staging without console clicks. AWS spend starts here (~$150/mo staging).

### 8.1 Terraform bootstrap (state backend)

```
infra/
├── bootstrap/        # state bucket + lock table (applied once with local state)
├── modules/
│   ├── network/      # VPC 2-AZ, public+private subnets, 1 NAT GW, VPC endpoints (S3 gw, SQS, Bedrock, Secrets, ECR, CW Logs interface)
│   ├── data/         # RDS PG16 (db.t4g.small, PITR, deletion_protection=prod), S3 buckets (SSE-KMS, versioning, lifecycle 30d→Deep Archive), SQS+DLQs, KMS CMK
│   ├── auth/         # Cognito pool + app client + custom:org_id  (replaces the Step 4 sandbox)
│   ├── compute/      # ECS cluster, 2 services (api: ALB target; worker: queue-depth autoscaling), task defs, task roles, ECR repos
│   ├── edge/         # ALB, ACM cert (DNS-validated), Route53 records, WAF (AWSManagedRulesCommonRuleSet + rate limit 2000 req/5min/IP)
│   └── observability/# CloudWatch log groups (90d), dashboards, alarms, CloudTrail
└── envs/
    ├── staging/      # main.tf composing modules; terraform.tfvars
    └── prod/
$ cd infra/bootstrap && terraform init && terraform apply     # S3 state bucket + DynamoDB lock
$ cd ../envs/staging && terraform init && terraform plan
```

IAM task roles (least privilege, the security-review centerpiece): `api-task-role` → S3 presign-scoped `s3:PutObject/GetObject` on the audio bucket prefix, `sqs:SendMessage`; `worker-task-role` → S3 `GetObject`, `sqs:ReceiveMessage/DeleteMessage`, `bedrock:InvokeModel` on the two model ARNs, `secretsmanager:GetSecretValue` on `/ernote/*`. RDS credentials: Secrets Manager with rotation, injected as ECS secrets.

> **Deployment-safety guard for AWS endpoint overrides:** Step 6 S1 uses the presence of both `S3_ENDPOINT_URL` and `SQS_ENDPOINT_URL` as the local-emulator switch; when present, startup is allowed to create the MinIO bucket and ElasticMQ queues. During Step 8, make `APP_ENV=dev|staging|prod` explicit and authoritative: `dev` may set both overrides, while `staging` and `prod` must fail startup if either override is present. With both overrides absent, the SDK must use normal AWS endpoint/task-role resolution and the application must only resolve Terraform-created resources. This prevents an accidental endpoint variable in an ECS task definition from enabling bootstrap behavior against a non-production endpoint. Keep the existing all-or-neither endpoint validation as an additional configuration check.

### 8.2 🌐 GitHub Actions OIDC (no AWS keys in GitHub)

```
$ aws iam create-open-id-connect-provider --url https://token.actions.githubusercontent.com \
    --client-id-list sts.amazonaws.com
```
Then (in Terraform, `modules/cicd/`) a role `gh-deploy` whose trust policy restricts `token.actions.githubusercontent.com:sub` to `repo:<you>/er-note-app:ref:refs/heads/master`, with permissions: ECR push, `ecs:UpdateService`/`DescribeServices`, `iam:PassRole` on the task roles.

### 8.3 Workflows

`.github/workflows/ci.yml` (PRs): `cargo fmt --check` · `clippy --workspace -D warnings` · `cargo test --workspace` (services: postgres:16 — `sqlx::test` uses it; `SQLX_OFFLINE=true` for build) · `cargo audit`.

`.github/workflows/deploy.yml` (push to master): `aws-actions/configure-aws-credentials` (role-to-assume = `gh-deploy`, OIDC) → docker build (api + worker share one image, different entrypoint/command) → push ECR `:sha` → `aws ecs update-service --force-new-deployment` staging api+worker → smoke step: `curl -fsS https://api.staging.<domain>/healthz` → **manual approval environment gate** → same for prod (prod stays empty until pilot).

### 8.4 Migration strategy in deploys

Run `sqlx migrate run` as a one-shot ECS task (same image, command override `er-server migrate`) triggered by the workflow **before** `update-service`. Write migrations to be backward-compatible with the previous app version (additive first; destructive only after the old tasks drain).

### 8.5 Gate — Step 8

- [ ] `terraform apply` on staging from a clean checkout creates everything; `terraform plan` is then empty (no drift)
- [ ] Merge to master deploys staging automatically; `scripts/demo-remote.sh --server https://api.staging.<domain>` passes end-to-end (Cognito mode, real Bedrock, real Deepgram with synthetic audio)
- [ ] ALB is HTTPS-only (HTTP→301), TLS policy ≥ TLS1.2; WAF attached; RDS not publicly accessible (verify: `aws rds describe-db-instances --query '…PubliclyAccessible'` → false)
- [ ] No long-lived AWS credentials exist in GitHub (`Settings → Secrets` is empty of AWS keys)
- [ ] ECS task definitions set `APP_ENV=staging|prod`, omit `S3_ENDPOINT_URL`/`SQS_ENDPOINT_URL`, and a startup test proves either override is rejected outside `dev`

---

## Step 9 — Hardening, Observability, Drills

**Objective:** the controls a hospital security review and a 2 a.m. incident both need.

### 9.1 Sub-steps

1. **Postgres RLS (belt #2 on tenancy):** `ALTER TABLE events ENABLE ROW LEVEL SECURITY;` + policy `USING (org_id = current_setting('app.org_id')::uuid)`; the repository sets `SET LOCAL app.org_id = …` per transaction. Repeat for snapshots/patients/audio_chunks/usage_log. Keep the application-level scoping too — RLS is the backstop, proven by re-running the cross-tenant test with the app-level filter deliberately commented out (then restore it).
2. **Rate limiting:** `tower_governor` keyed by clinician id (e.g. 60 req/min general, 5/min on `/mdm`) in addition to WAF's IP-level rule.
3. **Alarms** (Terraform `observability`): ALB 5xx > 5/5min · either DLQ depth > 0 · RDS free storage < 20% · RDS CPU > 80% 15min · api/worker `RunningTaskCount` < desired · p95 latency > 500ms · **Budgets**: monthly forecast > $400 staging. Route via SNS → email (pilot) — Slack webhook later.
4. **Dashboards:** one CloudWatch dashboard — req rate, p50/p95, 5xx, queue depth+age, DLQ, RDS connections/CPU, job durations (emit `tracing` fields → CW Logs metric filters: `stt_seconds`, `mdm_duration_ms`).
5. **PITR restore drill (do it, write it down):** `aws rds restore-db-instance-to-point-in-time` into a `-drill` instance → point a local server at it → verify a known patient note replays identically → destroy. Save the runbook as `docs/ops/runbook.md` (restore, deploy-rollback = previous image tag, DLQ redrive, key person contacts).
6. **Secrets rotation:** enable Secrets Manager rotation for the RDS secret; calendar a quarterly manual Deepgram key rotation.
7. **Pen-test prep checklist** (run yourself before paying anyone): authz matrix test per endpoint (no token / wrong org / wrong role) · presigned-URL scope check (can a URL for chunk A write chunk B? content-type enforced?) · SSRF surface (none expected — no user-supplied URLs) · dependency audit green · error responses leak nothing (no stack traces, no SQL).

### 9.2 Gate — Step 9

- [ ] RLS active; cross-tenant test passes with app-level filter disabled (then restored)
- [ ] Kill the worker mid-job → message returns, retries, DLQ alarm email arrives
- [ ] Restore drill performed once; runbook committed
- [ ] Checklist in 9.1.7 committed with date + outcomes

---

## Step 10 — Load Test, Cost Validation, Docs, Handoff to Phase 5

### 10.1 k6 load profile (`load/shift.js`)

Model the pilot's worst hour: **10 virtual clinicians**, each: register 18 patients, per patient 8 encounter cycles (start → upload 1–2 chunks → stop), labs/meds/dx interleaved, 1 MDM per patient, constant `?since=` polling every 10s per active patient. Use MockStt/MockSummarizer mode via a staging env flag for the *load* run (you're testing your system, not Deepgram's), then a second small run (1 clinician, 3 patients) fully real for end-to-end latency numbers.

**Targets (from `phase4.md` Step 10):** API p95 < 300 ms (excluding job endpoints) · MDM job completion p95 < 30 s (real run) · zero DLQ entries · zero idempotency violations (`SELECT event_id, count(*) … HAVING count(*) > 1` → empty).

### 10.2 Cost validation

- `usage_log` totals for the real mini-run vs Bedrock + Deepgram console usage: within 5%.
- Cost Explorer: tag every Terraform resource (`project=ernote`, `env=staging`) and confirm the monthly run-rate lands in the `phase4.md` §7.2 pilot band (~$510–700 + staging idle).
- Save a `usage_log` → "per-clinician COGS" SQL query in `docs/ops/cogs.sql` — this is the number that feeds `pricing-strategy.md` margin checks.

### 10.3 Docs + Phase 5 handoff package

- [ ] `CLAUDE.md`: workspace layout, server commands, "What's Working" truth-up
- [ ] `ARCHITECTURE.md`: add the server architecture diagram + sync protocol section
- [ ] `phase4.md`: mark steps complete / note deviations
- [ ] OpenAPI: `GET /api-docs/openapi.json` committed snapshot + Swagger UI on staging (behind auth)
- [ ] `docs/planning/phase5-handoff.md`: auth flow (Cognito + Amplify Swift pointers), sync contract (UUIDv7 stamping, batch upload, `?since=` paging), audio contract (Opus 30–60s chunks, presign, complete), polling contract (`/v1/jobs/{id}`), staging URLs + how to mint a test user

### 10.4 Final Phase 4 acceptance

Re-run the 12 acceptance criteria in `phase4.md` §10 as a literal checklist. All green = Phase 4 done; Swift work can start against staging on day one.

---

## Appendix A — External-Service Quick Reference

| Service | When | Action | Lead time | Cost |
|---|---|---|---|---|
| AWS account + SSO + budget | Part 0 | Console setup | hours | $0 |
| **AWS BAA (Artifact)** | Part 0 | Self-service accept | minutes | $0 |
| Bedrock model access | Part 0 | Console request form | minutes–days | $0 (pay per token) |
| **Deepgram BAA / HIPAA tier** | Part 0 → needed by pilot | Sales conversation | **weeks** | contract |
| Deepgram dev key | Part 0 | Console | minutes | free credits |
| Domain + Route 53 | Part 0 | Register/delegate | hours | ~$13/yr |
| Cognito sandbox pool | Step 4 | Console (manual) | 20 min | $0 |
| GitHub OIDC provider + role | Step 8 | CLI + Terraform | 30 min | $0 |
| ACM certificate | Step 8 | Terraform (DNS validation) | minutes | $0 |
| Hospital BAA/DUA/pilot agreement | before any real patient | Lawyer + privacy officer (see `pricing-strategy.md` §9) | **months — start now** | ~$1.5k legal |

## Appendix B — Local Dev Cheat Sheet

```
docker compose up -d                         # PG + MinIO + ElasticMQ
scripts/dev-bootstrap.sh                     # create bucket + queues + seed org/clinician
sqlx migrate run --source crates/er-server/migrations
cargo run -p er-server                       # API on :8080
cargo run -p er-server --bin worker          # job consumer
scripts/dev-token.sh                         # print a Bearer token (AUTH_MODE=dev)
cargo run -p er-cli -- --server http://localhost:8080 --token $TOKEN
cargo test --workspace                       # everything; sqlx tests need PG up
```

*End of implementation guide. Work the steps in order; don't start a step until the previous gate is green. The two clocks that run regardless of your coding pace: the Deepgram BAA and the hospital pilot agreement — keep both moving in the background from day one.*
