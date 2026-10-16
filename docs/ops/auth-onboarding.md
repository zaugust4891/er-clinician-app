# Cognito and synthetic tenant onboarding (Step 8 S7)

Written 2026-10-15. **Nothing here has been run against AWS yet.** No pool, user,
secret or clinician row exists from this procedure, and no live `/v1/me` or
cross-org result is recorded. Staging is synthetic-only.

The S7 code is available for local validation before AWS setup. The staging
release root now rejects missing/empty Cognito settings and pool/Region mismatch.
An auth-disabled foundation is valid for preparation, but cannot supply a
deployable release manifest. The private ECS onboarding template arrives in S8;
GitHub environment/OIDC activation arrives in S11. Follow the
[live AWS checklist](step8-live-aws-checklist.md) for those dependencies.

Rules for every step:

- Usernames, display names and org names are synthetic (`synthetic-…`,
  `Synthetic …`). No patient data and no real person goes into Cognito.
- Passwords, access tokens and refresh tokens never appear in command output,
  shell history you keep, screenshots, tickets, workflow logs or committed files.
  Commands below use `--query`/`--output` to print only what is needed.
- A Cognito login proves a subject only. The database `clinicians` row is the
  sole org authority, and only `er-onboard` writes it.

## 1. Inventory the Step 4 sandbox pool (read-only)

Do not assume the sandbox exists and do not delete it.

```sh
aws cognito-idp list-user-pools --max-results 60 \
  --query 'UserPools[].{id:Id,name:Name}' --output table
aws cognito-idp describe-user-pool --user-pool-id "$SANDBOX_POOL_ID" \
  --query 'UserPool.{mfa:MfaConfiguration,signup:AdminCreateUserConfig.AllowAdminCreateUserOnly,schema:SchemaAttributes[?starts_with(Name, `custom:`)].Name,deletion:DeletionProtection}'
aws cognito-idp list-user-pool-clients --user-pool-id "$SANDBOX_POOL_ID" \
  --query 'UserPoolClients[].ClientId' --output text
```

Record the answers in the S7 implementation record. Decision:

- **Import** only if the sandbox already matches `infra/modules/auth` (no
  self-signup, no `custom:org_id` used for authorization, secretless client) and
  its users must be preserved. Import with `terraform import` into
  `module.auth[0].aws_cognito_user_pool.this` and the client, then review a plan
  that shows **no replacement**. A replacement plan is a stop: it would change every
  `sub` and invalidate every clinician row and token.
- **Otherwise (expected)** enable a new synthetic staging pool and re-provision
  synthetic users deliberately (steps 2–4). Leave the sandbox untouched.

## 2. Create the pool and client

Set `auth = { enabled = true }` in the staging foundation tfvars (the root defaults
to the synthetic smoke exception) and apply after review. The manifest then
publishes `cognito_pool_id`, `cognito_client_id` and `cognito_region`, which become
the API's `COGNITO_POOL_ID`, `COGNITO_CLIENT_ID` and `COGNITO_REGION`. The
onboarding task role gains `cognito-idp:AdminGetUser` on this pool only.

Pool/client settings and their reasons are in
[`infra/modules/auth/README.md`](../../infra/modules/auth/README.md): no
self-signup, admin-only recovery, secretless client, SRP/refresh (plus password auth
under the exception), 60-minute access tokens, 12-hour refresh tokens with
revocation, no hosted login until exact redirects are reviewed.

### Recorded MFA exception (staging only)

> The staging pool `ernote-staging` sets MFA to OPTIONAL and enables
> `ALLOW_USER_PASSWORD_AUTH` so the two synthetic smoke users can sign in without a
> second factor. The pool may hold synthetic users only and must never be reused
> for patient data. Human AWS and GitHub administrators keep their own MFA; they are
> not users of this pool. Production does not inherit this exception: the module
> rejects it outside staging, and production needs a separately reviewed
> MFA/session policy.

## 3. Create the synthetic users and their secret

Disable shell tracing first and keep it disabled through step 5. With `set -x`
active, the shell writes each expanded command to stderr, including the generated
passwords passed to `jq --arg` and `admin-set-user-password --password` below;
disabling history does not prevent that. Do not run these steps in a CI job or
script that enables tracing.

Create users without sending any message and without printing the temporary
password:

```sh
set +x  # stays off until provisioning and smoke authentication are finished
for user in synthetic-clinician-a synthetic-clinician-b; do
  aws cognito-idp admin-create-user --user-pool-id "$POOL_ID" --username "$user" \
    --message-action SUPPRESS --query 'User.Username' --output text
done
```

Read the smoke secret's ARN (never its value) from the foundation root's `auth`
output. That output also carries `synthetic_smoke_kms_key_arn`; both feed
`ci.smoke_secret_arn`/`ci.smoke_kms_key_arn`. Both are `null` when the synthetic
smoke exception is off, and the whole output is `null` while auth is disabled:

```sh
SMOKE_SECRET_ARN="$(terraform -chdir=infra/envs/staging/foundation output -json auth \
  | jq -er '.synthetic_smoke_secret_arn')"
terraform -chdir=infra/envs/staging/foundation output -json auth \
  | jq -er '.synthetic_smoke_kms_key_arn'
```

Build the credential document in a private file, set the permanent passwords from
it, store it in the module's secret container, then destroy the file:

```sh
set +x  # repeat if this is a new shell: the passwords below must never be traced
umask 077
cred="$(mktemp)"
jq -n --arg a "$(openssl rand -base64 30)Aa1" --arg b "$(openssl rand -base64 30)Aa1" \
  '{clinician_a: {username: "synthetic-clinician-a", password: $a},
    clinician_b: {username: "synthetic-clinician-b", password: $b}}' >"$cred"
for key in clinician_a clinician_b; do
  aws cognito-idp admin-set-user-password --user-pool-id "$POOL_ID" --permanent \
    --username "$(jq -r ".$key.username" "$cred")" \
    --password "$(jq -r ".$key.password" "$cred")"
done
aws secretsmanager put-secret-value --secret-id "$SMOKE_SECRET_ARN" \
  --secret-string "file://$cred" --query 'VersionId' --output text
shred -u "$cred"
```

`admin-set-user-password` receives the password as an argument, so run this from a
single-user operator shell with history and tracing disabled for the session. The secret is
encrypted under its own key, so the smoke role, given only those two ARNs, can read
that secret and no provider, database or audio-bucket resource.

## 4. Seed two orgs and bridge rows

Fetch each actual subject (never a username or email):

```sh
aws cognito-idp admin-get-user --user-pool-id "$POOL_ID" \
  --username synthetic-clinician-a \
  --query "UserAttributes[?Name=='sub'].Value | [0]" --output text
```

Compose the request for the one-shot `er-onboard` task (`ONBOARDING_ACTION=apply`):

```json
[
  {"org_id": "<uuid A>", "org_name": "Synthetic Org A", "cognito_sub": "<sub A>", "display_name": "Synthetic Clinician A"},
  {"org_id": "<uuid B>", "org_name": "Synthetic Org B", "cognito_sub": "<sub B>", "display_name": "Synthetic Clinician B"}
]
```

Run it as the private one-shot onboarding task described in
[identity-verification.md](identity-verification.md) (onboarding login, onboarding
security group). The task prints only `created/unchanged/renamed` per entry.
Expected: `created, created` the first time and `unchanged, unchanged` on a re-run.
A subject already bound to another org, or previously revoked, fails the whole
request with `subject_bound_to_another_org` or `subject_revoked`; nothing moves.
Restoring a revoked subject is a reviewed manual change, not a re-run.

## 5. Smoke authentication and validation

The smoke client reads the secret with the smoke role, authenticates through
Cognito and calls the public API like any other client:

```sh
set +x  # no shell tracing while credentials or tokens are in variables
ACCESS_TOKEN="$(aws cognito-idp initiate-auth --client-id "$CLIENT_ID" \
  --auth-flow USER_PASSWORD_AUTH \
  --auth-parameters "USERNAME=...,PASSWORD=..." \
  --query 'AuthenticationResult.AccessToken' --output text)"
```

The command substitution captures the token directly; nothing is written to the
terminal or a workflow log. Never `echo` the variable, enable `set -x` while it is
set, or save it as an artifact; `unset ACCESS_TOKEN` when finished.

Use the **access token** only. An ID token, a dev token, or the AWS OIDC deployment
credential that lets a GitHub job assume the smoke role is not an application
access token; the API rejects all of them.

Done when, through the deployed router:

- each synthetic access token gets 200 from `/v1/me` with its own, distinct org;
- expired, wrong-pool, wrong-client, ID, dev, missing-bearer and unknown-`sub`
  tokens all get 401;
- org A's token gets the same 404 for org B's patient note/events/snapshot/export,
  jobs and audio as for ids that never existed;
- re-running step 4 reports `unchanged` and adds no rows.

`crates/er-server/tests/cognito_token_gate.rs` pins the same matrix locally with a
stand-in pool key; it is not live evidence.

## 6. Cache lifetimes and offboarding

The API validates JWTs offline. It does **not** ask Cognito per request, so
disabling a Cognito user does not end an already-issued access token.

| What | Lifetime | Effect |
|---|---|---|
| Access token | 60 min (`access_token_minutes`) | Signature-valid until expiry even after the user is disabled |
| Refresh token | 12 h, revocable | `admin-user-global-sign-out` revokes it; no new access tokens |
| Membership cache | 300 s per API task (`resolver.rs`) | A revoked clinician can keep access on a task until the entry expires or the task is replaced |
| JWKS | Refreshed only on unknown `kid`, at most once per 30 s | Unrelated to user revocation |

Staging emergency sequence for a synthetic clinician:

1. **Stop issuance:** `aws cognito-idp admin-disable-user` then
   `admin-user-global-sign-out` for the user.
2. **Remove membership:** run `er-onboard` with `ONBOARDING_ACTION=revoke` and
   `[{"org_id": "<org>", "cognito_sub": "<sub>"}]`; expect `revoked`.
3. **Clear caches:** force a new deployment of the API service so every task starts
   with an empty membership cache.
4. **Verify denial:** the clinician's still-unexpired access token gets 401 from
   `/v1/me`.

Without step 3, denial can lag by up to the membership cache lifetime; without
step 2, a disabled user's token keeps working until it expires. Step 9 must settle
the clinical revocation SLA and its implementation before any real clinician uses
the system.
