# Substep crew

The runner completes one planning-guide substep through an isolated candidate:
Claude edits files, Python runs relevant checks, Codex approves or returns findings,
and Python publishes the exact reviewed diff. Models no longer generate patches,
rewrite each other's patches, or confirm application. Git constructs the diff.
The runner requires Python 3.11 or later and installed Claude/Codex CLIs.

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
Incomplete or malformed responses never grant publication. COMPLETE also requires
the guide's implementation record, evidence for original criteria, final checks,
and independent final acceptance review. Passing intermediate checks alone cannot
mark a substep complete. The final author response can include its last edit, so
a successful one-edit substep requires only two model calls.

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
three consecutive failed validations, and three response/review attempts. Claude
also has a 30-turn limit per invocation. Authentication, quota, invocation timeout,
and tool-startup failures stop instead of regenerating the implementation.
Malformed response objects have bounded retries against the retained files.

Every author turn, including an interrupted call, saves a Git-generated unapproved
draft when possible. `--resume` imports an applicable draft into a fresh snapshot,
then requires current checks and fresh review. Old proposal artifacts remain
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

Run directories contain `candidate/`, `candidate.patch`, `pending-proposal.json`,
model prompts/transcripts/results, applied history, gate summaries, `events.jsonl`
and `metrics.jsonl`. Metrics retain duration, role, effort, prompt size, provider
usage when available, errors, decisions and actual publications. The benchmark
script summarizes these records and reads Claude timings from older runs. Failed
attempts are included; no live models are called by the benchmark.

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.test_substep_crew scripts.test_candidate_crew
```

The tests exercise actual temporary repositories and the stdio file server with
scripted model responses. They verify isolation, preservation of user changes,
validation/review ordering, stale-state rejection, recovery, budgets, CLI wiring,
and process cleanup. They do not establish live model quality or a specific
end-to-end speedup. Compare completed-substep latency and rejection rates on
matched starting snapshots before drawing those conclusions.

The CLI controls are documented in the [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
and [Claude CLI reference](https://code.claude.com/docs/en/cli-reference).
