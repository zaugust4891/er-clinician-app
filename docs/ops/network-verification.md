# Step 8 S4 network verification

Written 2026-09-14; status updated **2026-10-14**. **Network remains unapplied.**
Bootstrap alone is applied and verified; current network gate passes **25 mocked runs**.
AZ availability and existing-VPC CIDR overlap were inspected; funding and the NAT
availability decision remain open. See [audit evidence](step8-audit-2026-10-14.md). These live
checks require the S5–S9 foundation, an approved diagnostic image, and S6 roles and
secret injection. Their status remains pending. This record does not mark the live
S4 gate complete. Use synthetic data only.

## Default choice and idle cost

Two AZs, one zonal NAT in the first public subnet, no paid interface endpoints,
and an S3 gateway endpoint on both task route tables. Deepgram and public Cognito
JWKS retain their HTTPS route through NAT. Second-AZ tasks incur cross-AZ NAT
transfer; loss of the NAT's AZ breaks both AZs' public egress. Per-AZ NAT is an
explicit module switch and a production availability decision.

Estimate for `us-east-2`, checked 2026-09-14, assuming 730 hours/month:

| Component | Calculation | Estimated monthly idle cost |
|---|---|---|
| One NAT | 730 × $0.045/hour | $32.85 |
| NAT's one public IPv4 address | 730 × $0.005/hour | $3.65 |
| S3 gateway endpoint | No hourly/data-processing endpoint fee | $0 |
| Default network subtotal | NAT + IPv4 | **$36.50** |
| Optional single interface service in two AZs | 730 × 2 × $0.01/ENI-hour | +$14.60 |
| All six optional services in two AZs | 730 × 12 × $0.01/ENI-hour | +$87.60 |
| Optional second NAT and IPv4 | 730 × ($0.045 + $0.005) | +$36.50 |

NAT processing adds $0.045/GB; interface endpoint processing starts at $0.01/GB.
Cross-AZ/internet transfer, ALB and its public addresses, compute, RDS, storage,
logs, security services, and providers are additional. Rates are estimates, not
billing evidence; recheck before apply. The default network alone uses most of the
reported $50 testing budget. The full-stack estimate and budget decision remain
open. [AWS VPC pricing](https://aws.amazon.com/vpc/pricing/),
[AWS PrivateLink pricing](https://aws.amazon.com/privatelink/pricing/).

Adding endpoints does not remove NAT's fixed charge in this application. Compare
their per-AZ fixed fees and measured traffic savings, including cross-AZ effects,
before enabling them. ECR's two endpoints cost twice the single-service row.

## Local gates

```bash
terraform fmt -check -recursive infra/
./scripts/tf-network-check.sh
```

The script copies only Terraform source, tests and root lockfiles into a temporary
directory. It excludes operator tfvars/state/backend settings and runs mocked AWS
provider tests. Provider installation may need network access; tests need no AWS
credentials. Set `TF_NETWORK_PLUGIN_DIR` to an existing provider filesystem mirror
for offline installation. Tests cover placement, route/SG references, closed
initial ingress, endpoint scopes and ECR dependencies, invalid inputs, manifest
wiring, wrong-account guard, and production both disabled and explicitly enabled.
Mocked `apply` never creates cloud resources.

Verified locally on 2026-09-14 with Terraform 1.10.4 and AWS provider 6.64.0:
17 test runs passed (13 module, 2 staging, 2 production); Terraform formatting,
all five root validations, and shell syntax checks passed.

## Inspect the first reviewed plan and deployed configuration

Follow the [staging runbook](staging-deploy.md) for SSO and backend setup. Verify
account identity, subnet CIDR overlap and the AZ pair before planning. Keep client
CIDRs empty until S8 has configured WAF, the HTTPS listener and HTTPS targets.
Use a complete foundation plan; do not use targeted applies to bypass dependencies.

After the complete foundation is applied, collect only non-secret network metadata:

```bash
terraform -chdir=infra/envs/staging/foundation output -json network
# Set NETWORK_VPC_ID to that output's vpc_id (a non-secret identifier).
aws ec2 describe-subnets --region us-east-2 --profile ernote-staging-admin \
  --filters "Name=vpc-id,Values=$NETWORK_VPC_ID" \
  --query 'Subnets[].{id:SubnetId,az:AvailabilityZone,cidr:CidrBlock,publicIp:MapPublicIpOnLaunch}'
aws ec2 describe-route-tables --region us-east-2 --profile ernote-staging-admin \
  --filters "Name=vpc-id,Values=$NETWORK_VPC_ID" \
  --query 'RouteTables[].{id:RouteTableId,subnets:Associations[].SubnetId,routes:Routes}'
aws ec2 describe-security-groups --region us-east-2 --profile ernote-staging-admin \
  --filters "Name=vpc-id,Values=$NETWORK_VPC_ID" \
  --query 'SecurityGroups[].{id:GroupId,name:GroupName,inbound:IpPermissions,outbound:IpPermissionsEgress}'
aws ec2 describe-vpc-endpoints --region us-east-2 --profile ernote-staging-admin \
  --filters "Name=vpc-id,Values=$NETWORK_VPC_ID" \
  --query 'VpcEndpoints[].{id:VpcEndpointId,service:ServiceName,type:VpcEndpointType,dns:PrivateDnsEnabled,subnets:SubnetIds,routes:RouteTableIds,groups:Groups,policy:PolicyDocument}'
```

Confirm two task routes via the first-AZ NAT; public routes via the IGW; DB routes
local-only; all subnets disable auto public IP assignment. Confirm SG references
against the [connection matrix](../../infra/modules/network/README.md). Inspect
every task ENI for **no public-IP association**, including migration/onboarding.
S8 must set `assign_public_ip = false` explicitly. Inspect the actual RDS instance
for `PubliclyAccessible = false` and the expected subnet group/SG. Do not infer
those service settings merely from the subnet settings.

## Disposable private diagnostic task

S6/S8 must supply a reviewed, digest-pinned diagnostic task definition with CLI,
TLS and PostgreSQL tools, scoped task/execution roles, secret injection and safe
logging. It is ephemeral and requires no inbound SG rule or ECS Exec. The shipped
application image does not currently contain those diagnostic tools. Pull the
approved diagnostic image from the allowed ECR repository so its startup exercises
the same repository endpoint policy. Do not use a task with administrator authority.

Run once **in each private task subnet**, using the relevant API, worker,
migration or onboarding SG/role for the path under test. The following shows task
submission after setting the named variables to reviewed non-secret identifiers:

```bash
aws ecs run-task --region us-east-2 --profile ernote-staging-admin \
  --cluster "$NETWORK_CLUSTER_ARN" \
  --task-definition "$NETWORK_DIAGNOSTIC_TASK_DEFINITION_ARN" \
  --launch-type FARGATE --platform-version 1.4.0 \
  --network-configuration "awsvpcConfiguration={subnets=[$NETWORK_TASK_SUBNET_ID],securityGroups=[$NETWORK_TASK_SG_ID],assignPublicIp=DISABLED}" \
  --query '{tasks:tasks[].taskArn,failures:failures[].{arn:arn,reason:reason}}'
```

Submission is not success. Reject returned failures, wait with a bounded timeout,
and inspect task/container exit codes, image digest and ENI metadata. Missing exit
code or image/secret/log initialization failure fails the check. Stop a timed-out
task; never leave a diagnostic service running. Report assertion names and exit
status only, not full task environments, command output or credential material.

The task's reviewed diagnostic command must perform these assertions:

| Path | Positive proof | Required negative/control |
|---|---|---|
| DNS | Resolve RDS, Cognito JWKS, `api.deepgram.com`, S3, SQS, Bedrock, ECR, Secrets and logs names; optional interfaces resolve privately | No custom request-supplied URL or resolver bypass |
| Cognito JWKS | Verified HTTPS GET to the configured pool's `/.well-known/jwks.json`; parse a valid key set | Reject an untrusted certificate and a trusted certificate with a mismatched hostname in an isolated fixture; no `-k` or verification bypass |
| Deepgram | Verified TLS connection to fixed `api.deepgram.com:443`; S12 proves actual synthetic transcription | No token/audio in diagnostic output; an HTTP status alone does not prove authenticated transcription |
| Bedrock | Verified TLS to regional runtime endpoint; S7/S12 invoke the approved US profile with the worker role | Unapproved model/profile remains denied; include all approved destination model ARNs when an endpoint is enabled |
| ECR / S3 layers | Fresh task pulls the approved digest and reaches RUNNING in each AZ | Wrong repository denied when interface policy is enabled; S3 layer access remains required |
| Secrets / logs | Required secrets inject before startup, and a static `s4-network-ok` marker arrives in the intended log group | No secret value printed; unrelated secret denied; startup failure fails the gate |
| Queue | Scoped `GetQueueUrl` and `GetQueueAttributes` succeed for the actual queue | Unrelated queue denied; no purging, receiving or deleting application jobs during a connectivity check |
| RDS | `SELECT 1` plus `pg_stat_ssl` shows this session encrypted, using runtime CA trust and `sslmode=verify-full` | Wrong CA and wrong hostname rejected; API/worker/one-shot identities tested separately; no credentials in CLI arguments/logs |
| S3 | Signed synthetic Get/Put reaches the real audio bucket through the gateway | Anonymous reads denied; no blanket VPC-only bucket condition |

For PostgreSQL pass credentials by approved secret injection, never a connection
URI echoed to logs. Have the diagnostic process compare query results and print
only PASS/FAIL. Certificate negative fixtures must be isolated synthetic resources,
never a live endpoint whose certificate verification is weakened.

From outside the VPC, attempt a bounded connection to the actual private task IPs
on 8443 and the RDS address on 5432; require failure. Corroborate this with ENI,
route, RDS and SG configuration, since a timeout alone is ambiguous. Never add a
developer IP to database ingress to make diagnostics work. Once S8 is ready,
public access goes through the ALB on 443. S12 also verifies a normal presigned PUT
from outside the VPC, altered signature/key/content-type failure and natural expiry.

## Outbound TLS audit and remaining evidence

Source inspected for S4: `auth/keys.rs` uses reqwest's certificate/hostname checking;
`state.rs` builds JWKS URLs from administrator Cognito configuration. Cloud config
rejects `JWKS_URL` and AWS endpoint overrides. Deepgram's production client uses a
fixed HTTPS URL, `https_only(true)`, no redirects and normal certificate validation;
its HTTP URL override is compiled only for tests. Bedrock uses the AWS SDK default
HTTPS client, with cloud endpoint overrides rejected. No request DTO supplies a
provider fetch URL. This is source evidence, **not a live certificate test**.
The server's incoming TLS listener belongs to S8 and is not implemented by S4.

| Evidence | Status / owner |
|---|---|
| Terraform module, foundation wiring, offline network gates | Implemented locally; engineering |
| Verified account, pinned AZ order, reviewed CIDRs and full-stack budget | Pending; operator |
| Cloud route/SG/ENI and RDS public-access inspection | Pending S5–S9 apply; operator |
| Per-AZ startup, DNS, ECR, Secrets, logging, queue and DB probes | Pending private diagnostic task; engineering/operator |
| Real outbound TLS plus wrong-CA/hostname rejection | Pending isolated fixtures and private diagnostic task; engineering |
| External direct-access denial and signed S3 upload | Pending deployed resources; engineering |
| Actual NAT/endpoint charges and cross-AZ traffic | Pending billing/metrics; operator |

Record date, region, release/diagnostic digest, task ARN, tested path and assertion
result. Keep raw logs, credentials, tokens and presigned URLs out of this file.
