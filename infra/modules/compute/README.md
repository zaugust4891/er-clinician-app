# Release compute

Consumes the versioned foundation manifest and one digest. Produces API, worker and
migration RegisterTaskDefinition templates before service activation. Only
`services_enabled = true` registers the API/worker definitions and creates services.
No migration is implicitly run. The complete S7/S8/S9 manifest is mandatory.

Tasks are private Fargate/Linux/X86_64, 512 CPU units and 1024 MiB, with separate roles,
secret injection, non-root/read-only containers and no scratch mounts. API desired
count 2 rolls 100/200; worker desired count 1 rolls 0/100 without AZ rebalancing.
Prior task revisions remain active for rollback. See
[verification and activation](../../../docs/ops/compute-edge-verification.md).
