# Repository artifact provenance and review boundary

Initial metadata review **2026-10-09** against application HEAD `6845878`;
owner confirmations added **2026-10-10**. The owner confirmed current and historical
database/shift data were **synthetic only** throughout the project during the checklist
walkthrough. This records data-origin confirmation for the database, its WAL/SHM
companions and the shift file; the assistant did not independently inspect their
contents or scan residual pages. The owner also confirmed the signing keys were
intended solely for development/testing and have only ever been used that way.
Repository tracking cleanup was completed and verified **2026-10-10**, as described below.

## Generated agent runs

There were **65 tracked files** under `.agents/runs/`, including prompts, transcripts,
verdicts, diffs and the shared generated gate schema. `scripts/substep_crew.py` writes
run output under that directory and generates its `.schemas` files itself. These files
are local working evidence, not application source. The cleanup removes them from the
Git index while retaining the local files; `/.agents/runs/` prevents new accidental adds.
This does not remove anything from previously published history.

## Development artifacts and retention

| Paths | Source evidence | Tracking disposition |
| --- | --- | --- |
| `er_notes.db` | `crates/er-cli/src/main.rs` uses `./er_notes.db` for local persistence; owner confirmed current and historical data are synthetic only on 2026-10-10 | Untracked and ignored; local file preserved with matching checksum |
| `er_notes.db-wal`, `er_notes.db-shm` | SQLite companion files; owner confirmed database data have always been synthetic on 2026-10-10. Residual pages were not independently scanned | Untracked and ignored alongside the database; local files preserved with matching checksums |
| `shift.json` | The local CLI loads and saves this file; owner confirmed current and historical data are synthetic only on 2026-10-10 | Untracked and ignored; local file preserved with matching checksum |
| `dev/keys/dev_private.pem`, `dev/keys/dev_public.pem`, `dev/keys/jwks.json` | Auth unit/integration tests read the dev private key/JWKS; `mint_dev_token` uses the key pair. Owner confirmed exclusively development/test intent and usage on 2026-10-10 | Retain as development/test fixtures because existing auth tests depend on them |

Existing ignore rules now keep the four runtime paths out of future accidental adds.
The three development/test signing fixtures remain tracked. The Docker source allowlist excludes local
databases, shift files, development keys and agent runs; the public RDS trust bundle
is a separate allowed asset.

## Inventory scope and completed cleanup

A filename-only workspace inventory on **2026-10-10** found only the four known
database/shift paths among `.db`, SQLite, `.bak`, `.backup` and `shift.json` candidates.
Dependency/tool/cache directories (`.git`, `target`, `node_modules`, `.agents`, `.aws`,
`.codex`, `.claude`, `.terraform`, `.venv`, `venv`) and locations outside the workspace
were excluded. This is a bounded inventory, not an inspection of all caches or backups.
The Docker source allowlist continues to exclude the runtime data and development keys.

- Owner completed untracking `er_notes.db`, `er_notes.db-wal`, `er_notes.db-shm`
  and `shift.json` on **2026-10-10**. `git ls-files` confirms none remains tracked;
  all four paths match the existing ignore rules.
- `sha256sum -c /tmp/er-note-runtime-artifacts.sha256` returned four `OK` results
  against the owner's pre-cleanup checksums. All local copies remain present.
- `git ls-files` confirms the three signing-key files remain tracked as
  development/test fixtures.

No local database, shift file or development key was deleted. Untracking the runtime
files does not remove their previously committed versions from Git history.
