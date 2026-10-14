# Staging cost review — 2026-10-14

Prepared for account `657615279057`, application Region `us-east-2`, against
application HEAD `329f4ed` and the S8 task sizing in the implementation guide.
This is a scenario estimate, not an approved budget or a billing forecast.

## Existing budget

Live API verification: `My Monthly Cost Budget`, **$50 USD/month**, primary billing
view, unblended cost excluding credits and refunds. This is the organization management
account; the budget has no staging-service/Region/linked-account restriction.
The budget health is `HEALTHY`.
Alerts are actual spend **greater than 85% ($42.50)**, actual spend **greater than
100% ($50)**, and forecast spend **greater than 100% ($50)**. Each has one email
subscriber; the same recipient is recorded in private local evidence. The owner
confirmed control, monitoring and prior delivery on **2026-10-14**. This is owner
evidence; the assistant did not send a test notification.
The displayed current/forecast spend was $0.50 at inspection; the approved small
Sonnet probe may appear later. Budgets are notifications, not a hard spending cap.

## Always-on scenario

730 hours/month; the guide's **two API tasks and one worker, each 0.5 vCPU/1 GiB,
Linux/x86**, one NAT, two-AZ public ALB, single-AZ RDS PostgreSQL `db.t4g.small`,
50 GiB gp3, two new KMS keys and six proposed secrets. Other rows explicitly state
assumed usage/counts. No credit or introductory free-tier deduction is applied.

Rates were retrieved on **2026-10-14** from the official AWS Price List Query API,
filtered to `regionCode=us-east-2` (Route 53 hosted-zone pricing is global).
SKUs identify the exact price dimensions used; private raw responses are under
`/tmp/er-note-audit-20261014/pricing/`. See the
[AWS Price List documentation](https://docs.aws.amazon.com/awsaccountbilling/latest/aboutv2/price-changes.html).

| Component | Monthly calculation | USD/month | Price-list SKU |
| --- | --- | ---: | --- |
| NAT gateway | 730 × $0.0450000000 | $32.85 | `SRB9S9CM9RDPW97X` |
| NAT public IPv4 | 730 × $0.0050000000 | $3.65 | `8HFJK44D9234XNWA` |
| RDS db.t4g.small | 730 × $0.0320000000 | $23.36 | `JBDFT6PN9ME2D9FK` |
| RDS gp3 50 GiB | 50 × $0.1150000000 | $5.75 | `JQQGK6D789TWJF8B` |
| Fargate vCPU: 3 tasks x 0.5 | 1095.0 × $0.0404800000 | $44.33 | `UGSJ89BUWB7SMJ6A` |
| Fargate memory: 3 tasks x 1 GiB | 2190 × $0.0044450000 | $9.73 | `HH5EE3ZFWSFKGKWM` |
| ALB hours | 730 × $0.0225000000 | $16.43 | `HCVPZW9WEWSJKJWJ` |
| ALB public IPv4: 2 AZs | 1460 × $0.0050000000 | $7.30 | `8HFJK44D9234XNWA` |
| ALB usage assumption: 1 LCU average | 730 × $0.0080000000 | $5.84 | `S3YDM2U7G69S4W5C` |
| KMS: state + data keys | 2 × $1.0000000000 | $2.00 | `FEMX7ZQ4DJ6QE2KP` |
| Secrets: six proposed secrets | 6 × $0.4000000000 | $2.40 | `HHK3AZN6HN5CFDJM` |
| Route53: existing hosted zone | 1 × $0.5000000000 | $0.50 | `KVPGEJE88UW8S779` |
| WAF: one ACL | 1 × $5.0000000000 | $5.00 | `2R9CM7WJ2KQ6AWDQ` |
| WAF: three rules | 3 × $1.0000000000 | $3.00 | `D2BYWPHKSXKTQSXA` |
| CloudWatch log ingestion: 5 GB | 5 × $0.5000000000 | $2.50 | `5T6Z6V26DSR7SZ49` |
| CloudWatch stored logs: 5 GB-month | 5 × $0.0300000000 | $0.15 | `X7X4WD4A95NDR9U5` |
| CloudWatch: eight alarm metrics | 8 × $0.1000000000 | $0.80 | `28VSMYRVQXUWCTMA` |
| ECR: 5 GB | 5 × $0.1000000000 | $0.50 | `NA4VZMZUEQGMXP2Y` |
| S3 Standard: 10 GB | 10 × $0.0230000000 | $0.23 | `YPGKVRB2EKTVDJDT` |
| **Scenario subtotal** | | **$166.32** | |

This subtotal exceeds the $50 budget before provider usage, traffic/request charges,
additional operational choices, tax, or domain registration. RDS autoscaling to the
100 GiB cap adds $5.75/month in storage; burstable CPU surplus credits, extra retained
backups/snapshots and additional KMS key versions can add charges. The ALB LCU,
secret count, WAF rule count and log/storage volumes are scenario assumptions to
replace with the final S6–S9 configuration.

## Variable charges and unresolved scope

- **Sonnet US geographic profile:** $3.30 input, $16.50 output, $0.33 cache read,
  $4.125 standard cache write and $6.60 one-hour cache write per million tokens.
  Verified against `AmazonBedrockFoundationModels`, Ohio `Claude Sonnet 4.6`,
  on-demand **Regional CRIS** price dimensions. The application's current input,
  output, cache-read and standard cache-write constants match these rates.
  [Bedrock pricing](https://aws.amazon.com/bedrock/pricing/).
- **Deepgram:** the application requests `nova-3-medical`; verify that model's
  account-specific rate and plan before adding a minutes estimate. The public
  pricing page lists Nova-3 Monolingual pre-recorded PAYG at $0.0043/minute but
  does not separately identify the medical model; that row is not established as
  this application's contracted medical-model price. No real Deepgram request was
  made. [Deepgram pricing](https://deepgram.com/pricing).
- **Traffic:** NAT processing is $0.045/GB in Ohio, plus applicable inter-AZ/egress
  charges. Include the chosen audio duration/volume and deployment/image-pull traffic.
- **Requests and telemetry:** include S3/SQS/KMS/Secrets API usage, DNS queries,
  CloudTrail destinations/events, any paid WAF managed groups, custom metrics,
  alarms/dashboards beyond the assumed eight alarm metrics, image builds/scanning,
  and actual log volumes. These are not zero merely because usage is undecided.
- **Identity:** Cognito tier, sign-in volume and any paid federation/advanced features
  remain S7 inputs. An existing `er-note-dev` pool was found; adoption is not decided.
- **Domain:** owner confirms `zamos.co` has not been registered. Registration/renewal
  is an additional registrar charge, and public delegation is blocked until registration.

The owner selected **planning for the $166+ always-on scenario, with deployment left
pending**, on **2026-10-14**. No AWS budget was changed; its configured limit is still $50.

### Selected planning path

Use the two-API/one-worker scenario above for S6–S9 implementation planning. Replace
assumed secret/WAF/log/alarm counts with the final foundation configuration; confirm
Deepgram's medical-model price and expected minutes; add Bedrock input/output/cache
allowances and traffic/request usage. The proposed monthly limit should cover
**$166.32 + provider allowances + variable AWS usage + a stated contingency**.
The final limit and actual budget update need approval before foundation deployment.
Domain registration, availability choices and dependency completion also remain pending.
Selecting this planning direction does not authorize an application apply.

## Bootstrap-only review

The reviewed bootstrap plan adds **32 Terraform resources**, with **no updates or
deletions**, for two protected/versioned S3 buckets, one rotating KMS key/alias,
five state-scope policies/roles/attachments and the account guard. Only the exact
verified SSO IAM role is trusted. Ignored operator inputs are prepared in
`infra/bootstrap/terraform.tfvars`; the private saved plan and summary are in
`/tmp/er-note-audit-20261014/terraform-validation/bootstrap/` and
`/tmp/er-note-audit-20261014/bootstrap-plan-summary.json`.

Bootstrap's new KMS key baseline is **$1/month**, plus S3 storage/requests and KMS
requests (existing DNS cost is separate). This step does not create NAT, RDS, ECS,
ALB or application workloads. The owner approved this exact plan and state verification
on **2026-10-14**. It applied successfully; bootstrap state is now encrypted in S3 and
no-change plans pass. All five scoped backends initialized. See the
[bootstrap acceptance evidence](step8-audit-2026-10-14.md). The full-stack budget
remains unchanged and application foundation/release are unapplied.
