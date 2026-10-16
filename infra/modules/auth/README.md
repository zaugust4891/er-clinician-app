# auth module (Step 8 S7)

One Cognito user pool and one secretless native/CLI app client per environment.
The pool proves identity only. Org membership lives in the database
`clinicians` table and is granted only by the `er-onboard` task, so the pool has
no custom attributes (no `custom:org_id`) and a successful login grants nothing.

| Setting | Value |
|---|---|
| Self-signup | Off (`allow_admin_create_user_only`) |
| Recovery | `admin_only`: no email/SMS takeover channel |
| MFA | `ON` (TOTP), except under `synthetic_smoke_exception` |
| Client secret | None; SRP + refresh flows |
| Tokens | Access/ID 60 min, refresh 12 h by default (bounded 5–60 min and 1–24 h); refresh-token revocation on |
| Hosted login | Off unless exact `callback_urls` are given; then authorization code only, never implicit, no wildcards or plain http |
| Replacement | `deletion_protection = ACTIVE` and `prevent_destroy`: a new pool changes every `sub` and orphans clinician rows |

## Synthetic smoke exception (staging only)

`synthetic_smoke_exception = true` is accepted only with `environment = "staging"`.
It makes MFA optional and adds `ALLOW_USER_PASSWORD_AUTH` to the client so the
automated smoke users can sign in non-interactively. The pool must then contain
synthetic users only; it must never hold a real clinician or be reused for
patient data. Production must not inherit it and needs its own reviewed
MFA/session policy.

Under the exception the module also creates an empty Secrets Manager container
(`<name>/synthetic-smoke-credentials`) under a dedicated rotating KMS key. Its
ARNs feed `ci.smoke_secret_arn` and `ci.smoke_kms_key_arn` in the identity
module, so the smoke role can decrypt that secret and nothing else. Operators
write the value out of band; Terraform never holds a user or password.

Outputs: `user_pool_id`, `user_pool_arn`, `client_id`, `issuer`,
`synthetic_smoke_secret_arn`, `synthetic_smoke_kms_key_arn`.
