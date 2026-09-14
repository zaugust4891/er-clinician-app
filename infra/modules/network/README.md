# S4 network boundary

Shared by both foundation roots. Staging provisions this module; production calls it
only with `enabled = true`. No cloud apply has been performed. Run
`scripts/tf-network-check.sh` from the repository root for mocked provider checks.

## Placement and connections

The module splits a canonical IPv4 /16–/20 into eight equal ranges. Indexes 0–1
are public ALB/NAT subnets, 2–3 private task subnets, and 4–5 isolated database
subnets. Each pair spans the same two AZs; ranges 6–7 are reserved. Every subnet
disables automatic public IP assignment. Explicit route associations keep database
subnets off both NAT and the internet gateway. The default security group is empty.

| Source | Destination | TCP rule |
|---|---|---|
| Explicit `client_ipv4_cidrs` (empty by default) | ALB SG | 443; optionally 80 for S8's redirect listener |
| ALB SG | API SG | `api_https_port`, default 8443, both ingress and egress |
| API, worker, migration, onboarding SGs | Database SG | 5432, both ingress and egress |
| Each task SG | AWS services, Cognito, Deepgram | Outbound 443 |
| API/worker SGs | Optional SQS endpoint SG | 443 |
| Worker SG | Optional Bedrock endpoint SG | 443 |
| All four task SGs | Optional ECR, Secrets, logs endpoint SGs | 443 for task execution/startup |

No task-to-task rule, inbound SSH, bastion, or public database ingress is created.
SGs are stateful, so database/endpoint SGs need no outbound rule for replies.
Security groups cannot constrain HTTPS by hostname, and cannot block the VPC's
AmazonProvidedDNS resolver. IAM and fixed/configured provider URLs remain necessary.
[Security-group rules](https://docs.aws.amazon.com/vpc/latest/userguide/security-group-rules.html).

The first AZ hosts the single staging NAT. Tasks in the second AZ pay cross-AZ
transfer when using it, and a first-AZ outage interrupts both AZs' public egress.
`nat_per_az = true` gives each task subnet its own AZ's NAT. Interface endpoints
never disable NAT: Deepgram and public Cognito JWKS still need it.

## Endpoint switches and policy scope

The S3 gateway endpoint always covers both task route tables. It initially permits
only `GetObject` on `prod-<region>-starport-layer-bucket/*`, which ECR image pulls
need even without ECR interface endpoints. S5 must populate `s3_bucket_arns` with
the real audio bucket ARN; object Get/Put and bucket List/location actions then
become available. Gateway policy permission is an additional boundary and does
not grant a task role access. No bucket policy or `aws:SourceVpce` deny is created,
so external signed client uploads remain possible.
[ECR endpoint requirements](https://docs.aws.amazon.com/AmazonECR/latest/userguide/vpc-endpoints.html).

`interface_endpoints = {}` purchases no ENIs. Enable a service by adding its key
and `resource_arns`, for example (synthetic identifiers only):

```hcl
network = {
  availability_zones = ["us-east-2a", "us-east-2b"]
  s3_bucket_arns     = ["arn:aws:s3:::ernote-staging-audio-example"]
  interface_endpoints = {
    sqs = {
      resource_arns = ["arn:aws:sqs:us-east-2:000000000000:ernote-staging-jobs"]
    }
  }
}
```

Supported keys: `sqs`, `bedrock-runtime`, `secretsmanager`, `ecr.api`, `ecr.dkr`,
`logs`. Each endpoint has private DNS, one ENI per task AZ and a service-specific
SG. Its fixed action allowlist is in `endpoints.tf`; resources must be explicit
service-matching ARNs. Bare or embedded resource wildcards are rejected, except
`:*` at the end of a logs ARN to include streams. Secret ARNs are references only.

Enable both ECR keys together and supply the same repository ARNs to each.
`ecr:GetAuthorizationToken` alone uses `Resource = "*"` because AWS does not
support resource scoping for that action; image pulls remain repository-scoped.
Precreate log groups in S9 and include every API, worker and one-shot log group's
stream scope. Include all required injected secrets in the Secrets endpoint.
For Bedrock include the reviewed US inference profile and model ARNs in every
permitted destination Region, alongside the corresponding S6 IAM permissions.
This module permits only `InvokeModel`, matching the current non-streaming client.

## Interfaces and later-step obligations

| Consumer | Module output/input | Required integration |
|---|---|---|
| S5 data | `db_subnet_group_name`, `security_group_ids.database` | Reuse the subnet group; set RDS `publicly_accessible = false`, verified TLS and an explicit Multi-AZ decision. Two-AZ subnet placement alone is not Multi-AZ RDS |
| S5 storage | `s3_bucket_arns` input | Add the actual audio bucket ARN before any application S3 call; keep external presigned PUTs permitted |
| S6 one-shot tasks | `private_subnet_ids`, migration/onboarding SG IDs | Separate approved roles/credentials; `assignPublicIp = DISABLED`; no runner-to-RDS route |
| S8 edge | `public_subnet_ids`, ALB SG ID, `api_https_port` | HTTPS target group and actual server TLS on 8443; the existing dev HTTP listener is insufficient |
| S8 release | `private_subnet_ids`, API/worker SG IDs | `assign_public_ip = false` on every ECS service; S4 subnet settings alone do not enforce the ECS flag |
| S8/S9 execution | endpoint resource scopes | Image pulls, secret injection, and log delivery must work before the process starts |

Outputs are available as `terraform output -json network` in foundation. The
manifest publishes VPC/task subnet IDs, four task SG IDs, and the HTTPS port.
The existing schema is `map(string)`, so `private_subnet_ids` is a **JSON-encoded
array string** and the port is a decimal string. Release uses `jsondecode` and
`tonumber` respectively; required-key guards now include these interfaces.
ALB/DB interfaces stay within foundation instead of broadening release ownership.
The manifest waits for the complete network, including routing, SGs and endpoints.

AZ selection defaults to the first two sorted available standard zones in the
chosen region. Pin the reviewed ordered pair in `network.availability_zones`
before applying; changing order or CIDR later can replace subnets and NATs.
The production example's single NAT is only a dormant starting value; select its
availability requirements before enabling that root.

Live checks and dated cost assumptions: [network verification](../../../docs/ops/network-verification.md).
