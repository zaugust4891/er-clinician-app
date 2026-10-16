# Substep crew

The runner completes one planning-guide substep through an isolated candidate:
Claude edits files, Python runs relevant checks, Codex approves or returns findings,
and Python publishes the exact reviewed diff. Models no longer generate patches,
rewrite each other's patches, or confirm application. Git constructs the diff.
The runner requires Python 3.11 or later and installed Claude/Codex CLIs.
Live AWS access is optional; ordinary runs retain their file-only tools.

```bash
python3 scripts/substep_crew.py --dry-run
python3 scripts/substep_crew.py --phase 4 --step 8 --substep S6 --profile security
python3 scripts/substep_crew.py --resume
python3 scripts/substep_crew.py --status
python3 scripts/substep_crew.py --watch
python3 scripts/benchmark_substep_crew.py
```

## Working files and publication

`workspace.Candidate` copies tracked and nonignored files from the current working
tree, plus the selected guide. It preserves dirty and untracked source content and
existing deletions. It excludes credentials, runner configuration/artifacts,
symlinks, and build products. A new repository inside the run directory supplies
private baseline snapshots; its commits never touch the user's index or history.

The author edits that candidate with `read_file`, `list_files`, `search`,
`edit_file`, `write_file`, and `delete_file`. `file_tools.py` is a small stdio MCP
server launched from the trusted source checkout. It enforces path containment,
rejects symlinks and protected paths, bounds content and reads, and provides no
shell execution. Reviewers get only its three read tools. `@patch` exposes the
exact Git-generated review diff in numbered ranges; `@checks/NAME` exposes saved
gate diagnostics through the same read-only path. This avoids copying whole
changed files, particularly lockfiles, into every review prompt.

Claude runs in restricted mode with built-in tools removed, hooks disabled, and
only the explicitly configured candidate MCP server allowed. The author resumes
its own session within a run; reviewers start independent sessions. Codex uses
its read-only sandbox, disables command execution, other integrations and agent
spawning, and requires the candidate MCP server to initialize. Repository reads
therefore do not depend on the nested shell/bubblewrap path that failed in prior
runs. No sandbox bypass is enabled. Startup exercises the actual local file-server
transport before any model invocation. CLI authentication/provider availability
is checked by the CLI when invoked; it is not proven by this local preflight.

Only `PatchLoop.apply_reviewed()` writes the source checkout. It requires the
current in-memory review receipt, matching source state, unchanged validated
candidate, identical reviewed bytes, safe paths and an applicable patch. Approval
is consumed once. Source edits remain unstaged and uncommitted. Arbitrary external
writers are unsupported: fingerprints detect intervening changes but cannot lock
out another process between the last check and `git apply`.

## Loop and validation

The author returns a small object: CONTINUE or COMPLETE, explanation, and acceptance
evidence. The reviewer returns APPROVED or CHANGES_REQUIRED with file/problem
findings. Review findings and failed checks return to the retained candidate.
Both author and reviewer receive the outstanding findings on subsequent passes.
Review fixes use the overall edit and time budgets, rather than a separate cap of
three rejected reviews. Returning an unchanged previously rejected candidate stops
before repeating its checks and review. Reviewers are asked to report related
blocking cases together so the author can address them in one repair.
Incomplete or malformed responses never grant publication. COMPLETE also requires
the guide's implementation record, evidence for original criteria, final checks,
and independent final acceptance review. Passing intermediate checks alone cannot
mark a substep complete. The final author response can include its last edit, so
a successful one-edit substep requires only two model calls.

The author can also return BLOCKED with a concrete external dependency. This saves
the unapproved draft and `blocked.json`, sets the monitor to BLOCKED, and exits 3.
It does not publish the draft or mark the substep complete. Missing live access
therefore produces a handoff rather than the former no-change error.

## Optional live staging AWS access

Authenticate the local named profile first. Use a role whose IAM permissions cover
only the intended staging work. The expected account is an independent guard,
not a grant of permissions; a broad administrator profile remains broad.

```bash
aws sso login --profile ernote-staging-admin
aws sts get-caller-identity --profile ernote-staging-admin
python3 scripts/substep_crew.py --resume \
  --aws-profile ernote-staging-admin \
  --aws-account-id YOUR_STAGING_ACCOUNT_ID \
  --aws-region us-east-2
```

Replace the account placeholder with the 12-digit staging account recorded in the
deployment contract. Add `--dry-run` to inspect the configuration without calling
AWS or a model. Live access is not silently inherited on ordinary resumes: supply
the AWS flags again. An exact `--approve-cloud` resume reuses the request's pinned
profile, account and region.

Claude receives an additional scoped MCP server. Its `aws_read` tool allows named
inventory operations for Cognito, ECS, EC2, IAM, S3 and Secrets Manager metadata.
It cannot retrieve secret values or assume roles through that tool. Authentication
is checked with STS before access. The subprocess environment removes ambient AWS
credential/endpoint overrides and Terraform flag overrides, then selects the local
profile and region. Credentials are resolved locally, not put into model prompts.
The reviewer receives only public cloud artifact reads, with no AWS execution or
request tool. Both models still have built-in shell execution disabled.

For other operations, Claude queues a `cloud_request` and returns CONTINUE. The
harness first validates, reviews and publishes any associated file changes. A
separate Codex review then inspects the proposed operation and its code/configuration.
Terraform init and plan can run after that review. Every direct AWS write,
Terraform apply, and Python cloud script stops for **your explicit approval**.
The BLOCKED report links the exact request JSON and prints a command like:

```bash
python3 scripts/substep_crew.py --resume RUN_DIR --approve-cloud REQUEST_ID
```

Read the request's operation and parameters before running that command. For
Terraform apply, also read its referenced public plan JSON, including resource
replacements/deletions and output changes. Approval binds the request ID, source
fingerprint, AWS profile/account/region and saved plan hash. Resume performs a
fresh independent review, checks those bindings again and executes only that
operation. A subsequent write requires another approval. A changed source tree or
plan invalidates the approval; prepare a new request instead of editing its JSON.

Terraform is restricted to the staging foundation and release roots. Init accepts
only bucket/KMS backend overrides and uses the existing lockfile. Plan accepts
variable values and pins the account/region variables. Apply consumes the saved
binary plan; it never silently replans. Terraform's sensitivity markers and common
credential field names are redacted from the public JSON. Raw plans and command
logs live under the run's `cloud/private/` directory with owner-only access and
are never exposed through model tools. Keep these artifacts private: Terraform
plans can contain plaintext sensitive values even when terminal output hides them.
[Terraform plan documentation](https://developer.hashicorp.com/terraform/cli/commands/plan)

For onboarding, secret handling or live HTTP tests, Claude can author a focused
`scripts/cloud/*.py` script and request its execution with explicit arguments.
The script must output JSON evidence, handle secrets locally, avoid printing them,
and bound network/subprocess calls. Its code passes file review and its invocation
passes operation review and your approval. These scripts and Terraform execute
with host filesystem access and the selected profile's permissions; they are not
OS-sandboxed. Inspect their behavior as you would an operator script. Field-based
redaction is a backstop, not a way to make arbitrary secret-bearing prose safe.

An execution receipt is claimed before a write starts. Failed, interrupted or
timed-out operations may have partial effects and are never automatically replayed
using the same approval. Inspect the receipt, private log and actual cloud state
before preparing a retry. No automatic rollback is attempted. Results are saved as
public JSON evidence; merely preparing a request never satisfies live acceptance.

`--gates auto` selects checks from affected paths. Intermediate Rust changes use
package checks where scope is clear. Final checks include workspace Rust tests,
Terraform gates, and Python tests as relevant, including pre-existing dirty files
and previously published edits. An explicit gate set adds required final checks;
it does not suppress checks for another affected language. Documentation-only
intermediate edits use `git diff --check`. `--gates none` explicitly opts out.
Gates run serially and stop at the first failed prerequisite. Known missing
infrastructure errors stop the run rather than prompting a code rewrite.

Commands come from the harness, never model responses. They execute in the
candidate, inherit the existing environment and source `.env` values, and reuse
source `target/` for Cargo builds. As with ordinary repository tests, gates execute
project code and are not an OS sandbox for arbitrary malicious test code. Source
and candidate fingerprints are checked around validation. Deterministic bounded
log extraction supplies failures; there is no model condenser.

The default profile uses Claude medium effort and Codex high effort. `routine`
uses medium for both; `security` uses Claude high and Codex xhigh. `claude-only`
and `local` are also available, with the same file-tool and publication boundaries.
`--author-effort` overrides Claude effort. Session reuse applies to Claude authors;
the local Codex author receives the objective on every call.

## Budgets, recovery, and evidence

Defaults are 15 minutes per model invocation, 45 minutes per run, 12 author turns,
three consecutive failed validations, and three response attempts per invocation
(`--review-attempts`). Repairing review findings consumes `--max-edits` and
`--run-timeout`, independently of response retries. Claude
has a 60-tool-turn limit per invocation, configurable with `--author-max-turns`.
Startup prints the effective author limit and records it in `run.json`.

If an author reaches that limit after changing the candidate, the runner can send
a short continuation in the same retained session and workspace. This consumes
the existing response-attempt budget (at most two continuations by default) and
obeys the unchanged invocation/run deadlines. It stops if there is no file
progress, the matching author session is unavailable, or the budget is exhausted.
Turn limits do not count as successful author responses, passing checks, or
review approval. The author is prompted to return CONTINUE after one coherent
change rather than attempting the entire substep in one invocation.

Authentication, quota, invocation timeout, and tool-startup failures still stop
instead of regenerating the implementation. Malformed response objects have
bounded retries against the retained files.

Resume a stopped S7 candidate with a larger per-invocation allowance if needed:

```bash
python3 scripts/substep_crew.py --resume RUN_DIR --author-max-turns 90
```

Every author turn, including an interrupted call, saves a Git-generated unapproved
draft when possible. `--resume` imports an applicable draft and the latest outstanding
review findings into a fresh snapshot, then requires current checks and fresh review.
Findings are retained even if a resumed run stops before its next review. Old proposal artifacts remain
readable for migration. An inapplicable old draft requires `--fresh-proposal`;
no stale patch is silently rebased. Neither history nor a saved artifact grants
approval. Source changes already published remain in place after later failures.

| Module | Responsibility |
| --- | --- |
| `cli.py`, `guides.py` | Target selection, flags, resume and exit status |
| `workspace.py`, `file_tools.py` | Candidate snapshots and confined file access |
| `orchestration.py` | Author/check/review loop and guarded publication |
| `engines.py`, `processes.py` | CLI adapters, session reuse, transcripts, process cleanup |
| `prompts.py`, `protocol.py` | Short instructions and strict response validation |
| `validation.py` | Path-based commands and deterministic diagnostics |
| `patches.py` | Source state, path policy and exact patch checks |
| `checkpoints.py`, `progress.py` | Draft recovery, progress and stage transitions |
| `cloud.py` | Named AWS reads, operation requests, account checks, saved plans and single-use approvals |

Run directories contain `candidate/`, `candidate.patch`, `pending-proposal.json`,
model prompts/transcripts/results, applied history, gate summaries, `events.jsonl`
and `metrics.jsonl`. Metrics retain duration, role, effort, prompt size, provider
usage when available, errors, decisions and actual publications. The benchmark
script summarizes these records and reads Claude timings from older runs. Failed
attempts are included; no live models are called by the benchmark.

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*.py'
```

The tests exercise actual temporary repositories and the stdio file server with
scripted model responses. They verify isolation, preservation of user changes,
validation/review ordering, stale-state rejection, recovery, budgets, CLI wiring,
and process cleanup. They do not establish live model quality or a specific
end-to-end speedup. Compare completed-substep latency and rejection rates on
matched starting snapshots before drawing those conclusions.

The CLI controls are documented in the [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
and [Claude CLI reference](https://code.claude.com/docs/en/cli-reference).
