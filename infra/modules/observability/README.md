# S9 observability

Foundation-owned retained/encrypted logs for all five ECS processes, restricted
CloudTrail audit storage, scoped operator read role, encrypted SNS email delivery,
native deployment alarms, two worker metric filters, and actual/forecast budgets.
No raw WAF/ALB/application-body logging and no workload activation.

Use through `envs/{staging,prod}/foundation/observability.tf`. All input values are
non-secret; emails/operator ARNs belong in ignored tfvars. Runtime notifications
remain off until exact operator principals and recipients are configured and live
subscription/delivery is verified. Production remains disabled by its root gate.

Run `bash scripts/tf-data-check.sh modules/observability` for isolated mocked tests.
The gate handles Terraform 1.10 mock teardown of protected resources only in its
temporary source copy; never remove `prevent_destroy` from the actual module.

See [telemetry verification](../../../docs/ops/telemetry-verification.md) for field
schemas, retention/access decisions, notification windows, diagnostic SQL, volume
estimates, and the remaining AWS acceptance procedure.
