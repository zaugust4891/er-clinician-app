A read-only reviewer examined your Phase 4 Step 8 S5 implementation and marked it NOT_READY.

Reviewer summary: The batch substantially implements S5 and all reported gates pass, but it is not ready. A confirmed S3 policy error rejects the headerless presigned uploads used by the application, production can silently inherit synthetic audio-retention values despite the unresolved production policy, and required live recovery/security acceptance evidence remains pending. The implementation guide itself was unchanged from HEAD.

Confirmed blockers to fix:
[
  {
    "severity": "high",
    "file": "infra/modules/data/audio.tf:138",
    "problem": "Confirmed availability defect: both PutObject deny statements use StringNotEqualsIfExists. In an explicit Deny with a negated IfExists operator, a missing condition key still satisfies the deny. The application's presigner sends only Content-Type (crates/er-server/src/routes.rs:286), so ordinary headerless uploads are denied instead of receiving bucket-default SSE-KMS. This breaks every audio-upload workflow. The test at infra/modules/data/tests/data.tftest.hcl:115 merely asserts the mistaken policy shape and therefore validates the opposite of externally meaningful behavior.",
    "required_fix": "Express each wrong-header denial so it applies only when that header is present, for example by combining an appropriate Null condition with StringNotEquals, while allowing both SSE headers to be absent. Add a behavioral/live policy test proving a current headerless presigned PUT succeeds and wrong algorithm/key requests fail."
  },
  {
    "severity": "high",
    "file": "infra/envs/prod/foundation/variables.tf:92",
    "problem": "Confirmed retention/data-loss risk: production defaults current and noncurrent audio retention to the synthetic seven-day values, while validation only requires explicit database availability and backup decisions. Thus the example production configuration at infra/envs/prod/foundation/terraform.tfvars.example:30 can be enabled and automatically expire production audio under an unapproved policy. This contradicts the comment at lines 118-119 and the unresolved production retention/hold requirement at docs/ops/deployment-contract.md:45.",
    "required_fix": "Require explicit production audio current-version and noncurrent-version retention decisions before enabled=true, sourced from the approved production retention/hold policy. Update the example and tests to prove production enablement fails when those decisions are omitted."
  },
  {
    "severity": "medium",
    "file": "docs/ops/data-verification.md:175",
    "problem": "S5 acceptance remains unverifiable: the document records regional RDS class/storage and minor-version verification, applied-resource inspection, LatestRestorableTime, signed upload behavior, object encryption metadata, and runtime-role negative checks as pending. It also states that no database, bucket, queue, or key has been applied. Therefore the guide's cloud-inspection acceptance criteria are not yet satisfied despite the offline gates passing.",
    "required_fix": "After correcting the policy and completing the dependent S6 permissions, apply to the approved synthetic staging account and record the required cloud/API evidence: supported RDS combination/minor, private/encrypted resources, advancing restore time, successful headerless signed upload, rejected public/insecure/wrong-encryption requests, expected object KMS metadata, queue/DLQ settings, and negative runtime-role control mutations."
  }
]

Current gate results:

### fmt: pass (0s)

### clippy: pass (1s)

### test: pass (23s)

### tf-manifest: pass (80s)

### tf-network: pass (70s)

Fix the confirmed correctness, integration, security, and documentation problems within this batch's scope. Add missing functionality or safeguards required to complete S5. Add or update meaningful tests for corrected behavior including authorization failures, invalid inputs, and other negative paths. Retain useful additions outside the original plan when they provide a concrete security, reliability, or maintainability benefit; remove additions whose risk outweighs their benefit.

If you believe a reported blocker is wrong, do not silently ignore it — explain precisely why the reviewer is mistaken, with evidence from the code.

Re-run the checks appropriate to the affected workflows and inspect the final diff. Do not repeatedly broaden testing once appropriate checks pass. Keep docs/planning/phase4-step8-implementation-steps.md's implementation record accurate with whatever you changed. Do not commit, push, stage, unstage, stash, or revert anything. Leave every change in the working tree for human review.