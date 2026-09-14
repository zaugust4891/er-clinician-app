# `manifest` module

Publishes one environment's **foundation manifest**: a small JSON object naming the
resources a release needs, stored in the versioned deployment-artifact bucket created
by `infra/bootstrap`.

Why this exists instead of `terraform_remote_state`: reading a foundation state file
gives the reader everything in it, including values that are only in state because
Terraform had to store them. A manifest is an explicit, reviewable allowlist, and the
release pipeline's IAM policy can be scoped to exactly one object key.

## Shape

```json
{
  "schema_version": "1.0",
  "project": "ernote",
  "environment": "staging",
  "account_id": "000000000000",
  "region": "us-east-2",
  "generated_by": "local:zach@2026-09-14",
  "resources": { "logical_name": "arn-or-id-or-url" },
  "secret_refs": { "logical_name": "arn:aws:secretsmanager:..." }
}
```

`secret_refs` holds **ARNs of secrets, never secret values** — the module's variable
validation enforces the ARN shape and rejects credential-sounding keys under
`resources`. ECS reads those ARNs at task start; nothing in this repository, this
bucket, or a CI log ever needs the value itself.

## Versioning

The bucket is versioned, so every publish keeps its predecessor. `version_id` and
`checksum_sha256` are outputs: a release can pin the exact manifest version it was
approved against, and a later foundation change cannot retroactively alter it.

## Adding a resource

Add the key to the publishing root's `local.manifest_resources`, and — if a release
cannot proceed without it — add the same key to the consuming release root's
`local.required_resource_keys`. The release root fails its plan when a required key is
missing, which is how a half-applied foundation is caught before it reaches ECS.
