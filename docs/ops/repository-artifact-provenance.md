# Repository artifact provenance and review boundary

Reviewed **2026-10-09** against application HEAD `6845878`. This records filenames,
source references and tracking status only. Artifact contents and historical contents
were not classified; no synthetic-only or credential-safety conclusion is established.

## Generated agent runs

There were **65 tracked files** under `.agents/runs/`, including prompts, transcripts,
verdicts, diffs and the shared generated gate schema. `scripts/substep_crew.py` writes
run output under that directory and generates its `.schemas` files itself. These files
are local working evidence, not application source. The cleanup removes them from the
Git index while retaining the local files; `/.agents/runs/` prevents new accidental adds.
This does not remove anything from previously published history.

## Existing tracked development artifacts

| Paths | Source evidence | Remaining decision |
| --- | --- | --- |
| `er_notes.db`, `er_notes.db-wal`, `er_notes.db-shm` | `crates/er-cli/src/main.rs` uses `./er_notes.db` for local persistence | Owner must classify current and historical records and relevant caches privately; filenames do not establish synthetic data |
| `shift.json` | The local CLI loads and saves this file; remote mode stores shift state on the server | Owner must classify current/history contents and decide retention and repository remediation |
| `dev/keys/dev_private.pem`, `dev/keys/dev_public.pem`, `dev/keys/jwks.json` | Auth unit/integration tests read the dev private key/JWKS; `mint_dev_token` uses the key pair | These are active test dependencies. Owner must establish dev-only provenance and absence of live reuse before changing tracking or rotating/replacing them |

Existing ignore rules already cover these paths, but tracked files remain tracked.
They are retained during this cleanup. The Docker source allowlist excludes local
databases, shift files, development keys and agent runs; the public RDS trust bundle
is a separate allowed asset.

## Pending S0 provenance work

- Privately classify current/history artifacts and relevant build or backup caches;
  record owner, date and evidence references in the deployment contract.
- If live secrets or patient records are identified, agree the appropriate access,
  rotation and remediation actions before changing shared history.
- Decide whether to keep explicit synthetic test fixtures or generate replacements
  before untracking the development keys that tests currently require.

No database, shift file or development key was deleted or untracked by this cleanup.
