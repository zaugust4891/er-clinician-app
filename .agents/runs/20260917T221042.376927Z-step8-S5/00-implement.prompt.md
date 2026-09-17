Analyze the codebase and implement Phase 4, Step 8, substep S5 (Provision encrypted storage, queues, and recoverable data) exactly as defined in docs/planning/phase4-step8-implementation-steps.md.

The substep section begins at docs/planning/phase4-step8-implementation-steps.md:880. Read it in the file — the text below is a copy for grounding, and the file is authoritative.

Baseline: HEAD is c55c2bcb490ba501a6546ea11d6055a364d66393. These paths were already modified before this run; they are NOT yours to revert, and some may be unrelated to S5:
  (clean tree)

--- S5 (verbatim from the guide) ---
## S5 — Provision encrypted storage, queues, and recoverable data

**Objective and scope:** create the exact durable resources the current application consumes,
with explicit lifecycle and recovery settings.

**Where:** `infra/modules/data/`, environment inputs, the retention section of the deployment
contract; any additive grants/schema changes required by S6.

**Depends on:** S4 network and S0 data-policy defaults. IAM grants are completed in S6.

**Tasks:**

1. Provision RDS PostgreSQL 16 at a currently supported minor version; verify regional class and
   storage combinations before applying. Start with the plan's `db.t4g.small`, single-AZ,
   50 GiB gp3 if supported and within the approved estimate. Cap storage autoscaling, schedule
   maintenance/backups, enable encryption, and require final snapshots for controlled removal.
   Enable deletion protection for persistent staging as well as production; deliberate teardown
   is a separate reviewed operation.
2. Set an explicit automated-backup window/retention (S0 proposes 7 days in synthetic staging)
   and verify `LatestRestorableTime` once backups exist. Resolve the old plan's 7-versus-35-day
   discrepancy here; production recovery objectives decide production retention. Encryption
   keys must remain available for as long as their retained backups are needed.
3. Set `rds.force_ssl=1` and configure clients for CA and hostname verification. Server-side
   encryption enforcement alone does not verify the server's identity. Include CA rotation in
   the image/operations procedure. [RDS PostgreSQL TLS guidance](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/PostgreSQL.Concepts.General.SSL.html).
4. Create an audio bucket with all public-access blocks, bucket-owner-enforced ownership,
   versioning, default SSE-KMS using the environment data key, and TLS-only policy. Enable S3
   Bucket Keys if appropriate to the cost/key-policy design. Current PUTs rely on bucket-default
   encryption: allow missing SSE headers but reject an explicitly wrong encryption algorithm or
   key. If requiring encryption headers instead, update the presign contract and client together.
5. Define current and noncurrent-version expiry separately, plus abandoned multipart cleanup.
   Do not configure Deep Archive as a catch-all: the worker has no restore workflow and cannot
   transcribe an archived pending object. No 7-year compliance-mode Object Lock on audio without
   an approved retention/hold decision. Lifecycle expiry is asynchronous, not an exact deletion
   deadline or an end-to-end erasure guarantee.
6. Track the presigned overwrite race explicitly: the same valid URL can overwrite its key, and
   `HeadObject` followed by an unversioned `GetObject` does not pin the approved bytes. Versioning
   alone does not fix that. Before PHI use, add a persisted version/checksum binding at completion
   and worker reads of that version, or a tested conditional write-once contract. Defer that
   protocol extension to Step 9 while staging is synthetic; prohibit claims of immutable audio.
   [S3 presigned upload behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/PresignedUrlUploadObject.html).
7. Create one **standard** job queue and `<name>-dlq`. Set main visibility to 300 seconds to
   match S1, long-poll wait to 20 seconds, `maxReceiveCount=3`, and explicit retention—propose
   4 days main/14 days DLQ. Restrict DLQ redrive permission to this source queue and an operator
   role. Use SSE-KMS; deny insecure transport. No transcripts, names, or provider responses in
   message bodies, attributes, queue names, or alarm text.
8. Keep CloudTrail/security log storage separate from audio lifecycle. Use distinct key
   boundaries for state/security evidence versus application data; one application-data key
   for RDS/S3/SQS is adequate initially if grants remain scoped. Record how disabling or deleting
   a key can make data unrecoverable. Avoid per-tenant keys until a contract or isolation need
   justifies their operational cost.
9. Do not provision an application export bucket yet. `note/export` returns the existing HTTP
   export; nothing currently writes exports to S3. Reserve an interface only when a consumer is
   designed. This does not remove the separate audit/log buckets that do have consumers.

**Privacy/security:** backups, S3 historical versions, transcripts in `audio_chunks`, raw
`TranscriptReady` events, snapshots, and exported notes are separate copies. A future deletion
or hold workflow must account for all of them. Ordinary runtime roles must not delete buckets,
purge queues, change retention, disable keys, or read backups.

**Done when / validation:** cloud API inspection confirms private RDS, encryption, backups,
public-access blocks, queue/DLQ settings, and lifecycle rules. A correctly signed client upload
works without extra SSE headers; public/unsigned GET and insecure transport fail. Inspect a
synthetic object's encryption/key metadata. Runtime roles cannot change these controls. Complete
restore rehearsal belongs to Step 9, but a disabled/unavailable backup is already a Step 8 failure.

---
--- end S5 ---

Complete the implementation, verification, and documentation in this task. Make reasonable decisions using repository conventions; ask for clarification only when a material ambiguity prevents correct progress.

1. Understand the application and S5
- Read CLAUDE.md, the implementation guide, and relevant architecture documentation.
- Inspect the codebase broadly to understand structure, existing capabilities, and patterns. Trace S5's dependencies and affected workflows in detail.
- Identify S5's requirements, acceptance criteria, prerequisites, and its relationship to preceding and subsequent substeps.
- Determine what already exists, what is missing, and what needs modification. Reuse suitable existing functionality.
- Briefly state your understanding of S5 and the intended implementation before editing.

2. Implement S5
- Complete all applicable S5 requirements and integrate them into the application's actual workflows.
- Follow established conventions for interfaces, data models, security, error handling, and configuration.
- Address relevant edge cases, failure paths, and compatibility concerns.
- Resolve prerequisite gaps necessary for S5 where feasible. Clearly identify any larger dependency that blocks completion.
- Keep changes focused on S5 and its necessary integration work. Account for documented future substeps without implementing speculative functionality.
- Preserve unrelated changes already present in the working tree.

3. Verify the implementation
- Validate S5 against each acceptance criterion and trace the affected workflow end to end.
- Add or update meaningful tests for new behavior and relevant failure cases.
- Run repository checks appropriate to the change.
- Inspect the final diff for regressions and inconsistencies between code and documentation.
- Report actual verification results and any limitations.

4. Update the guide's S5 section in docs/planning/phase4-step8-implementation-steps.md
Preserve the guide's structure and other substeps. Add an "## Implementation record — S5 (2026-09-17)" section explaining: what was implemented with real file and function references; why it is needed; where it fits including upstream inputs and downstream consumers; how it behaves on normal and failure paths; a concrete walkthrough with synthetic example data; a concise Mermaid diagram consistent with that walkthrough; how it was verified and what limitations remain.

Clearly distinguish implemented behavior from planned future behavior. Document and justify any necessary deviation from S5's original requirements.

Finish with a concise summary of the implementation, files changed, verification results, documentation updates, and any unresolved blockers. Do not commit, push, stage, unstage, stash, or revert anything. Leave every change in the working tree for human review.