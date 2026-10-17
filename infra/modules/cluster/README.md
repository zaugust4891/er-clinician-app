# `modules/cluster` — image repository and Fargate cluster (Step 8 S8)

Foundation-owned and long-lived: one ECR repository named `<project>-<environment>`
(the exact name S6's execution roles may pull from) and one Fargate-only ECS cluster.
Task definitions and services belong to the release root, not here.

## Tag convention and retention

Tags are immutable and every push is scanned. The deploy workflow (S11) must:

1. push each build as `sha-<git-sha>`;
2. add `release-<git-sha>` to that same image **before** registering any task definition
   that references its digest, including a rollback redeploy of an older image.

ECR cannot see which task definitions are running, and a lifecycle rule's only
action is expire. Any rule that selects tagged images therefore has a count or age
threshold past which it deletes one, possibly the running or rollback image. This
includes a "shield" rule with a large count. The policy has exactly one rule:
untagged images expire 7 days after push. Every tagged image, whether a candidate,
an accepted release, a failed promotion or a rollback target, is kept indefinitely.
A precondition on the lifecycle policy refuses any rule that selects tagged images.

Two consequences:

- The build must push a **single-platform image manifest**, with no image index
  and no provenance/attestation children (for example `docker buildx build
  --provenance=false --sbom=false`). Untagged children of an index would match
  the untagged rule.
- Tagged images accumulate until reference-aware cleanup exists. That cleanup must
  compare against every running and designated rollback task definition before
  it deletes anything. Storage cost is the accepted trade-off.

`tests/simulate/` evaluates the policy JSON the way ECR documents. The module test
uses it to check a history of 200 failed promotions, a 4,000-day-old accepted image,
a 40,000-day-old rollback target and stale candidates; only the stale untagged push
expires. A control run shows that a count rule over released images would evict
both protected images.

## Not here

ECS Exec, task sizing, and deployment settings are per service, in the release root.
Container Insights stays `disabled` until S9 decides telemetry cost.
