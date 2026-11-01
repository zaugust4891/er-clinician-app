# Step 8 S10 — pull-request CI

Implemented **2026-10-21**. The repository now defines the PR gates in
[ci.yml](../../.github/workflows/ci.yml). GitHub execution and required-check protection
on `zaugust4891/er-clinician-app`'s `master` branch remain acceptance items until this
change is published and an actual run is observed. S11 deployment/OIDC and S12 cloud
acceptance remain separate; this workflow creates no AWS resources.

## Running the same gates locally

Use Linux x86_64, Docker with Compose v2, Python 3, OpenSSL, curl, ripgrep, unzip,
and the Rust version in `rust-toolchain.toml`. The hosted image is `ubuntu-24.04`;
Rust, SQLx CLI, cargo-audit, Terraform, scanners, actions and service images are pinned.
Host utilities and vulnerability databases receive upstream updates.

```bash
rustup component add rustfmt clippy
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres
cargo install cargo-audit --version 0.22.2 --locked
scripts/ci-install-tools.sh scanners
scripts/ci-install-tools.sh terraform
export PATH="$PWD/target/ci-tools:$PATH"

scripts/ci-services.sh
python3 scripts/ci-security.py
python3 scripts/ci-audit.py
scripts/ci-terraform.sh
scripts/ci-image.sh
```

Do not source `.env`. `ci-services.sh` starts a new environment with only build-tool
locations and dummy emulator credentials. It disables metadata resolution and points
shared AWS config/credentials at `/dev/null`; Bedrock's endpoint is closed localhost.
The tests inject mock AI/STT providers. These are source-level protections, not a
network sandbox for arbitrary PR code; GitHub supplies no cloud/provider authority.

The application runner copies source into a private temporary directory, generates
one RSA keypair and `dev-key-1` JWKS using `mint_dev_token`, and runs all test binaries
against that same pair. It never changes the developer's keys. Each run creates a
separate Compose project, random localhost ports, and disposable database/object
storage. Postgres's disposable `ci` superuser supplies SQLx's `CREATEDB` privilege;
this is not the cloud runtime account. Readiness probes Postgres, MinIO and ElasticMQ
protocols with a deadline. Cleanup removes only that run's containers, volumes and
private directory, including after failure or interruption. The normal dev stack can
remain running. Build outputs stay in `target/` and are never uploaded or cached by CI.

The runner checks formatting, locked workspace/all-target Clippy with warnings denied,
the complete locked workspace suite, standalone `er-ai` tests and its normal dependency
tree. An AWS dependency in the standalone tree fails the feature boundary. Builds use
`SQLX_OFFLINE=true`. Then `er-migrate` initializes the disposable metadata database and
`SQLX_OFFLINE=false cargo sqlx prepare --workspace --check -- --all-targets --locked`
compares the root `.sqlx` cache against the migrated schema. See the
[SQLx 0.8.6 CLI contract](https://github.com/transact-rs/sqlx/blob/v0.8.6/sqlx-cli/README.md).
After the positive freshness check, the runner temporarily removes one query from
its disposable `.sqlx` copy and requires SQLx's specific missing-query diagnostic.
It also stops each disposable emulator in turn and requires readiness to fail before
restoring it. No integration target is skipped. Existing config, audio, auth, worker/lease/drain,
telemetry, readiness/TLS, migration, tenancy and repository/sync tests provide coverage;
The existing migration regression now opens the same configured connection as the
entrypoint, so it also covers a startup failure that direct test pools had missed.

Test output published by the wrapper contains names/totals only; failure panic text,
container logs, DB contents and synthetic clinical fixture output remain in the private
temporary directory and are deleted. No upload-artifact step exists.

## Infrastructure and image policy

`ci-terraform.sh` copies Terraform source without local state/backend inputs, runs
format checks and backend-disabled init/validate on all five roots with read-only
provider locks, then bootstrap, manifest, network, data, identity, auth, compute,
edge and telemetry tests. The Terraform-native mock assertions are the pinned IaC
policy suite: they check private/encrypted RDS, blocked public bucket access,
restricted trust, encryption, retention/protection, closed ingress and cloud task
configuration. These are evaluated mocked plans, not cloud plans. A private per-run
provider cache avoids repeated downloads; it is never shared with another workflow.
The PostgreSQL logging canary uses the same pinned database image as the application
runner. Remote state/locking acceptance remains in the previously recorded S3 gate.

After application, security and infrastructure succeed, `ci-image.sh` builds one
linux/amd64 candidate with offline SQLx metadata from the same checkout. The runtime stage is
rebuilt and upgrades packages within the pinned Debian generation so local Docker
cache cannot retain superseded OS security packages. It checks the
non-root user, native libraries and CA bundles, exercises API/worker/migration fail-closed
startup, and runs the actual migration entrypoint twice against an isolated database.
A deliberately changed migration checksum must fail. Trivy scans that exact local
image ID. The image is not pushed/exported and is removed after the check. S11's separate protected deployment workflow now calls the same script with
`CI_IMAGE_ID_FILE` to retain the exercised image for publication, then scans/verifies
the immutable registry digest. PR CI still publishes nothing. See
[staging deployment](staging-deploy.md). All workflow YAML is linted, and the security
job also runs `scripts/test_deploy.py` with fake AWS/Terraform responses.

## Migration prerequisite fixed during verification

The first real `er-migrate` run failed before applying schema changes: the shared
connection configured `search_path=pg_catalog,public`, making unqualified SQLx ledger
creation target the protected system schema. Migration connections now set `public`;
PostgreSQL still searches `pg_catalog` implicitly first, while unqualified creation
uses `public`. Other processes retain their existing explicit path. The migration
regression checks both lookup order and creation schema using the configured pool,
then verifies first migration, no-op rerun and checksum refusal. See PostgreSQL's
[search-path and system-catalog semantics](https://www.postgresql.org/docs/16/ddl-schemas.html#DDL-SCHEMAS-PATH).

## Scan decisions and exceptions

- Actionlint 1.7.7 validates the workflow; seven policy regression cases check that
  unknown findings, expired exceptions and scanner failures cannot pass.
- Gitleaks 8.24.3 scans the tracked working-tree snapshot with full value redaction.
  Only rule/path/line evidence is printed on failure. The S0-confirmed public dev
  private key is allowed only while its SHA-256 matches the reviewed fixture; a
  changed key fails before scanning. The exact fake security-group ID
  `sg-0123456789abcdef0` is also allowed as a non-secret. Neither is a blanket test-file
  exclusion. Untracked developer files and Git history are not scanned or exported;
  history provenance is covered by the [S0 record](repository-artifact-provenance.md).
- `cargo audit` findings are evaluated by `ci-audit.py`; all new vulnerability IDs
  block. `ci/security-exceptions.json` contains the existing `rsa` Marvin advisory
  `RUSTSEC-2023-0071`, with impact, mitigation, owner and **2026-11-20** expiry. There
  is no patched RSA version in the reviewed graph. Cloud auth performs public-key
  verification; private signing is confined to the local mint utility and tests.
  Expiry fails the gate, even if the exception has become unused. This is a scoped
  technical exception, not approval for a network signing/decryption service.
- Trivy 0.69.3 blocks every HIGH/CRITICAL container finding, including unfixed ones.
  There are no container exceptions or `ignore-unfixed` switch. Any necessary future
  exception must name the exact package/advisory, impact, mitigation, owner and expiry,
  with automated expiry enforcement before adoption. Scanner/download/database errors
  fail the gate. Tool downloads are checked against committed SHA-256 values. This
  release avoids the versions identified in the
  [upstream Trivy incident advisory](https://github.com/aquasecurity/trivy/security/advisories/GHSA-69fq-xp46-6x23).
- The S10 dependency scan required patched `rustls` 0.23.46, `rustls-webpki` 0.103.16
  and `webpki-roots` 1.0.10. Cargo.lock records only these targeted TLS updates.
  Remaining maintenance warnings are `lru` 0.16.4 (unsound; prior S6 review found
  the affected API outside this usage), and yanked `js-sys` 0.3.87, `spin` 0.9.8 and
  `wasm-bindgen` 0.2.110. Owner: repository maintainer; follow-up: dependency update
  with standalone/device feature verification. These warnings remain visible and do
  not silently become vulnerability exceptions.

## GitHub activation and remaining acceptance

The workflow runs on every PR, pushes to `master`, and manual dispatch. Actions use
full commit SHAs; checkout does not persist credentials. Token permissions are
`contents: read`, with no `id-token`, secrets, protected environments or deployment
steps. Only hosted ephemeral runners are eligible. There are no path filters, shared
caches, artifact publication or `continue-on-error` settings. `Required CI` runs even
when a dependency is skipped/cancelled and passes only if all four jobs succeed.

After publishing this change, the repository administrator should require **Required CI**
from GitHub Actions on **master**, require reviewed PRs and review workflow/policy
changes, then verify an ordinary and a fork PR. Preserve existing protections when
editing the branch rule. The check must have run before it can be selected reliably.
No branch rule or GitHub setting was changed by the local implementation.

- [ ] Observe a clean hosted PR run, including image scan, without cloud credentials.
- [ ] Require `Required CI` on `master` and verify failing/skipped gates block merge.
- [ ] Verify a fork PR has no OIDC/provider secret access and uses only hosted runners.
- [ ] Maintain the expiring RSA exception and resolve remaining dependency warnings.

## Local evidence

Verification results are recorded below after running the implementation. Live AWS
acceptance and GitHub branch protection must not be inferred from local checks.
