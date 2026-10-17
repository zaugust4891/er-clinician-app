# Shared ECS task JSON

Pure RegisterTaskDefinition data for the five process binaries. No registration,
execution, secret reads or log-group creation. Services consume it through `compute`;
foundation exposes private bootstrap/onboarding operator templates from it.

All images are digest-pinned; container restrictions and process entrypoints are
shared. Bootstrap has no task role. Before registering it, `db-secrets.py render`
replaces staged secret references with the exact rotation's version IDs. See
[verification](../../../docs/ops/compute-edge-verification.md).
