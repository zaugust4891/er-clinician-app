# Production promotion contract — inactive

The production Terraform roots remain disabled. `promote-production.yml` accepts an
exact staging release reference and then fails deliberately, without an environment,
OIDC permission or AWS credentials. This makes accidental activation observable; it
is not an implemented production deployment pipeline.

Before replacing that stop gate, approve and record:

1. Production account, region, resource/DNS/secrets ownership, budget, recovery
   objectives, operational owner and every pre-PHI closure in the deployment contract.
2. The accepted staging record's exact digest and architecture, source/config commit,
   migration filenames/hashes, configuration diff, live evidence and current scan
   exceptions. Promote the same digest; never rebuild. Cross-account ECR transfer
   must verify manifest/layer identity and the destination digest.
3. An available protected `prod` environment with required reviewers, prevented
   self-approval where supported, protected branches, and reviewed exact OIDC subject.
   Verify rejection of staging and untrusted subjects **before** obtaining production
   credentials. No production role is authored/enabled by the S11 staging exception.
4. Separate production foundation/release/smoke authorization, account/state/manifest
   guards and approved provisioning policy. Production smoke must use its reviewed
   authentication policy; do not copy staging passwords, password-flow exception,
   synthetic organizations or cleanup jobs.
5. The migration-before-rollout, exact digest/target verification and accepted-record
   transaction, with explicit schema compatibility and a production rollback/forward
   repair rehearsal. Reuse the tested staging algorithm only after parameterizing
   and testing production boundaries; `deploy.py` is deliberately staging-only.

Production approval is a future live gate, not something this inactive workflow can
prove. No production service should be enabled merely by setting a GitHub variable.
