# S8 compute and HTTPS edge

S8 implementation is complete locally; AWS acceptance remains pending. No application
resources were applied for this work. Follow the
[live AWS checklist](step8-live-aws-checklist.md) before enabling a release.

## Ownership and activation

| Owner | Implementation | Default |
|---|---|---|
| Foundation | `modules/cluster`: immutable/scanned ECR and Fargate cluster | Staging authored; production disabled |
| Foundation | `modules/edge`: ACM DNS validation, Route53 alias, ALB HTTPS target/listener, regional WAF | `edge = null`: no edge |
| Foundation operator output | `modules/task`: bootstrap/onboarding task JSON and private network configuration | No templates without `operator_image_digest`; never registers/runs tasks |
| Release | `modules/compute`: API/worker services and API/worker/migration task JSON | No candidate without `image_digest`; `services_enabled = false` |

Foundation optionally accepts an exact hostname and an existing public hosted zone:

```hcl
edge = {
  hostname       = "api.staging.example.com" # replace with the owned hostname
  hosted_zone_id = "Z_REPLACE_WITH_OWNED_PUBLIC_ZONE"
  # ingress_enabled = false is the default
}
```

Keep `network.client_ipv4_cidrs = []` during provisioning. ACM's DNS validation and
renewal records remain managed by Terraform; the certificate uses the ALB region.
There is no registrar purchase or zone-delegation automation. Listeners depend on
WAF association. While closed, WAF also blocks every request, including when an
ALB security group was previously opened.

Only when synthetic validation is ready, explicitly set `edge.ingress_enabled = true`
and a reviewed `network.client_ipv4_cidrs` set. The foundation rejects CIDRs without
an enabled WAF edge. It never discovers runner IPs or expands the allowlist. Port 80
is absent by default; `network.http_redirect_enabled = true` creates only an HTTPS
redirect. Sensitive clients must start with HTTPS.

An image candidate requires a nonempty `manifest_version_id`. Rendering a release
candidate also requires the complete S7/S8/S9 manifest, including existing log groups
named `/ecs/<project>-<environment>/{api,worker,migration}`. S9 must create them and
publish their names; S8 deliberately does not fabricate the missing telemetry contract.
Production remains disabled and its foundation still needs the separate production
Cognito wiring before it can supply a complete release manifest.

With the image supplied and services still disabled, `terraform output -json compute`
contains `task_templates.migration` and `migration_network_configuration`. S11 must
register and run that exact-digest migration privately, verify its exit, and then
apply the release with `services_enabled = true`. Terraform does not run migrations.
After activation, keep the digest, pinned manifest, and activation inputs in subsequent
plans: removing them proposes removing the services. Use the WAF ingress gate to close
traffic, not removal of release inputs.

Private setup templates are exposed through foundation's `operator_task_templates`
and `operator_task_networks` after supplying `operator_image_digest`. Bootstrap uses
no task role, its own execution role and the migration SG; onboarding uses its own
role pair and SG. Neither privilege is added to the release manifest. Extract a
bootstrap template for the existing S6 tool:

```sh
terraform -chdir=infra/envs/staging/foundation output -json operator_task_templates \
  | jq '.["db-bootstrap"]' > /tmp/ernote-bootstrap-template.json
```

Follow [identity verification](identity-verification.md) to stage credentials and run
`db-secrets.py render` before registration; the raw template uses AWSPENDING references
that must be replaced with exact version IDs. S9 must create the `db-bootstrap` and
`onboarding` log groups first. Onboarding's reviewed membership input is a one-shot
RunTask override described in [auth onboarding](auth-onboarding.md), not a Terraform
variable. Use Fargate platform `1.4.0`, disabled public IP and the provided private
network configuration for all one-shots.

## Runtime contract

| Setting | API | Worker | One-shots |
|---|---|---|---|
| Image | Same ECR digest | Same ECR digest | Same reviewed candidate digest |
| Binary | `er-server` | `er-worker` | `er-migrate`, `er-db-bootstrap`, `er-onboard` |
| Platform | Linux/X86_64, Fargate 1.4.0 | Same | Same when run |
| CPU / memory | 512 / 1024 MiB per task | 512 / 1024 MiB | 512 / 1024 MiB |
| Desired tasks | 2, AZ rebalancing enabled | 1, AZ rebalancing disabled | No service |
| Rolling min / max | 100% / 200% | 0% / 100% | Not applicable |
| DB pool cap | 5 | 5 | 1 |
| Stop timeout | 60 seconds | 120 seconds; application drain 100 seconds | 60 seconds |
| Listener | TLS on foundation's port (8443 default) | None | None |

All definitions set UID/GID 65532, drop all Linux capabilities, disable privilege,
and use a read-only root with no writable mounts. TLS keys and audio processing stay
in memory. ECS Exec and worker autoscaling are absent. Services use private subnets,
no public IP, separate S6 role pairs and process-specific Secrets Manager injections.
API receives only its DB credentials; worker also receives Deepgram. Worker model
settings match S6's approved US Sonnet inference profile. No static AWS keys, dev
endpoints, database URLs, or direct-provider credentials enter task environment.

The initial connection budget is 15 steady-state, up to 25 during a normal API rollout,
plus 10 reserved for migration/bootstrap/onboarding/admin use: **35 available connections**
after RDS/system reserves. Lower `db_connection_budget` is rejected. Verify the real
RDS `max_connections` and measure observed connection/memory peaks before activation;
the budget and task sizes are assumptions, not benchmarks. Stop-before-start settings
avoid intentional overlapping worker deployments. Prove drain, lease and replay
behavior under replacement; ECS stability alone does not demonstrate job progress.
[ECS deployment parameters](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/service_definition_parameters.html).

Service task definitions retain prior active revisions (`skip_destroy`) and ECR never
expires tagged images. S11 must tag each candidate before use, record accepted/rollback
revisions and digest, and push a single-platform manifest without attestation/index
children. Cleanup of tagged images and active revisions is a later explicit operation
that checks references first.

The ALB public policy is `ELBSecurityPolicy-TLS13-1-2-2021-06`. HTTPS IP targets use
`/readyz`, a five-second probe timeout, two healthy/unhealthy observations, and a
45-second deregistration delay. The API has a 30-second request timeout and a 120-second
startup health grace. Configuration, DB/JWKS/queue and TLS initialization precede bind.
Readiness allows three seconds for checkout/query/schema access; liveness has no DB
or provider dependency. All router responses, including rejected bodies and request
timeouts, have `Cache-Control: no-store`.

Each API task generates its own self-signed certificate in memory. ALB encrypts the
backend hop but does not validate target certificates; SGs, target registration and
IAM determine which targets/peers are authorized. This is not mutual TLS.
[ALB target behavior](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/load-balancer-target-groups.html),
[listener policy](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/create-https-listener.html).

## WAF body boundary

The regional ACL enforces AWS Common Rule Set and 2,000 requests per 300 seconds per
source IP. This initial rate is a hypothesis; hospital NAT can aggregate clinicians.
The native API client sends a static versioned User-Agent to satisfy the common
[rule that rejects a missing User-Agent](https://docs.aws.amazon.com/waf/latest/developerguide/aws-managed-rule-groups-baseline.html). No user/device identifiers are included.
Request sampling and raw WAF/ALB request logging remain off; S9 owns privacy-aware
telemetry. WAF connectivity failure is configured to fail closed.

ALB sends WAF only the first **8 KiB** of a body. The managed `SizeRestrictions_BODY`
action alone is overridden to Count. A custom oversize rule blocks all oversized
bodies except these exact method/path pairs with a UUID patient id and content type
`application/json` (optionally `; charset=utf-8`):

- `POST /v1/patients/<uuid>/events`
- `PUT /v1/patients/<uuid>/snapshot`

A nonterminating Count rule labels those requests; it never skips the managed or rate
rules. The application still authenticates, checks tenancy, parses JSON and enforces
its 2 MiB cap. WAF cannot inspect the remaining body bytes. Other managed rules can
still reject clinical-looking free text, so synthetic valid-body tests are a live gate;
do not add a blanket `/v1` exclusion to fix a false positive.
[ALB inspection limit](https://docs.aws.amazon.com/waf/latest/developerguide/web-acl-setting-body-inspection-limit.html),
[local label matching](https://docs.aws.amazon.com/waf/latest/developerguide/waf-rule-label-match-examples.html).

## Required live acceptance

Record only configuration, assertion outcomes, synthetic identifiers and release
references. Keep tokens, secret values, request content and credentials out of records.

- [ ] Complete S9 telemetry and private credential bootstrap; publish/pin the complete
  manifest. Verify each execution role can pull/inject/log only within its scope.
- [ ] Register/run migration with the candidate digest; require successful STOPPED/exit 0.
- [ ] Verify both API tasks run across the intended AZs, the one worker has no listener,
  and task roles, entrypoints, image digest, platform and container restrictions match
  the templates. Measure RSS, writable-file failures and DB connection peaks.
- [ ] With ingress closed, confirm no clinical request reaches the API. Open only the
  reviewed synthetic reachability path after WAF association and telemetry exist.
- [ ] Verify public DNS, ACM issued status and validation records; negotiate TLS 1.2 and
  1.3 with hostname/chain validation and reject older versions. Never use `curl -k`
  for the public endpoint. Check the backend TLS hop and SG source restriction.
- [ ] Verify `/readyz` returns 200 and minimal JSON. A broken startup must not pass health;
  controlled DB loss/schema denial must produce 503 while `/healthz` stays 200. Restore
  DB access and confirm recovery. Test target health and deployment rejection explicitly.
- [ ] Send legitimate synthetic small and >8 KiB event batches and snapshot writes,
  plus a request above 2 MiB. Expect success for valid permitted bodies and application
  413 above the cap. Check WAF metrics without retaining bodies.
- [ ] Send >8 KiB bodies on other paths, wrong methods/content types, and controlled
  managed-rule/rate probes. Verify WAF blocks them. Confirm rule sampling/logs remain
  disabled. Tune false positives only with a documented exact rule/path case.
- [ ] Roll the API while retrying idempotent accepted events; preserve every event.
  Replace/drain the worker during a synthetic job; verify heartbeat, lease/retry,
  completion and no duplicate effects. Test previous accepted revision rollback.

## Local validation

`./scripts/tf-data-check.sh` includes the edge/task/compute modules, foundation wiring,
release activation guards and disabled production. `./scripts/tf-network-check.sh`
checks the network/foundation dependency closure. Both use mocked providers and copy
only source/tests and committed locks, not backend state, local variables or credentials.
Pass explicit targets (for example `./scripts/tf-data-check.sh modules/compute`)
for a focused rerun. An optional `TF_DATA_PLUGIN_DIR` / `TF_NETWORK_PLUGIN_DIR` uses cached provider binaries.

The Rust workspace suite includes `edge_gate`: TLS 1.2/1.3, plaintext rejection, fresh
identities, DB/schema readiness failure, bounded pool saturation, no-store responses,
and a valid 32 KiB sync body versus a rejected >2 MiB body. These checks do not establish
AWS certificate negotiation, WAF behavior, Fargate runtime limits or replacement safety;
the live assertions above remain open.

Validation recorded 2026-10-16 (local date):

- Rust workspace: **349 passed**, none failed or ignored, using compose emulators
  and `SQLX_OFFLINE=true` for cached compile-time queries.
- Clippy across workspace/all targets with warnings denied: passed; Rust format: passed.
- Terraform data/identity/auth/compute/edge/root gate: **81 runs passed**. After final
  rollback-retention and WAF regex adjustments, focused compute **8/8** and edge **6/6**
  passed (the edge suite adds one additional regression case).
- Terraform network/foundation regression gate: **32 runs passed**.
- Terraform formatting, shell syntax and diff whitespace checks: passed.

All AWS provider operations in these test gates were mocked. Local tests do not close
any live acceptance checkbox above.
