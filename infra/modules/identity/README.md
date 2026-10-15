# S6 identity boundary

Called by both foundation roots; production uses the root's existing enablement
gate. Creates separate API, worker, migration and onboarding task roles, five
execution roles (including privileged DB bootstrap), and five value-free encrypted
secret containers. Task roles never read application secrets. ECS execution roles
inject each process's own credentials. There are no secret-version resources.

The module exports role/secret ARNs, ECS JSON-field injection references, dedicated
log-group names for S9, and five service-specific KMS delegations for the data key.
The foundation composes bucket/queue ARNs from their names to avoid cycles, and the
data module's separate key-policy resource merges these delegations with account
IAM administration. Permissions and encryption contexts are tested from the exact
JSON policy documents.

Optional `ci` inputs create S11's staging role templates with exact GitHub protected
environment trust. They default to null. The infrastructure template has state
access only until S11 attaches a reviewed provisioning policy; release and smoke
have separate application-deployment and synthetic-secret permissions. No CI role
can pass the bootstrap/onboarding roles or chain into a production identity.

Run `bash scripts/tf-data-check.sh` from the repository root for mocked module and
foundation checks. The full bootstrap, maintenance rotation, naming contract, IAM
tradeoffs and remaining live probes are in
[identity-verification.md](../../../docs/ops/identity-verification.md).
