# HTTPS edge

Foundation-owned regional ALB, ACM DNS validation, Route53 alias, HTTPS readiness
IP targets and WAF. Starts closed; the foundation separately guards client SG CIDRs.
An existing public hosted zone and exact hostname are required. Listeners depend on
WAF association; the target-group output waits for its HTTPS listener association.

See [compute/edge verification](../../../docs/ops/compute-edge-verification.md) for
activation, the 8 KiB WAF inspection boundary and exact sync-body exceptions, TLS
assumptions, and live tests. `tests/edge.tftest.hcl` uses only mocked AWS providers.
