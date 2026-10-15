**Substep crew latency review — October 15, 2026**

The largest opportunity is to reduce model work and failed edit cycles. The
current workflow makes both models manufacture complete diff strings, adds an
author confirmation after review, and reconstructs the author's context for each
invocation. These choices explain much more of the observed delay than Python
execution overhead. Keep independent review, exact-byte application, preservation
of user changes, and meaningful validation while simplifying the exchanges.

This review covers the current working-tree versions of
`scripts/substep_crew_lib/`, the launcher, tests, benchmark, and saved runs. Several
modules already contain uncommitted improvements. Historical measurements below
are evidence of particular failure modes, not benchmarks of all current fixes
combined. Runtime code was not changed by this review.

**Measured evidence**

The four saved October S6 runs provide six Claude responses containing a PATCH.
Their durations were 12.65, 21.36, 38.80, 16.39, 15.29, and 26.84 minutes: median
18.87 minutes. PATCH here means a structured draft, not necessarily an applicable
or approved change. There is no completed October S6 run in this sample.

| Observation | Measurement | Evidence |
| --- | --- | --- |
| Long proposal at `xhigh` | 38m 48s; 66 reported turns; 255,841 thinking tokens | [S6 proposal](../.agents/runs/20261014T232917.621068Z-step8-S6/001-implementer-proposal.final.json) |
| Failed proposal cycle at `high` with compact prompts | Three proposals consumed 58m 31s and produced no new applied edit; their prompts were about 19,000 characters each | [Run progress](../.agents/runs/20261015T043343.932415Z-step8-S6/progress.log), [failure](../.agents/runs/20261015T043343.932415Z-step8-S6/error.txt) |
| Earlier author confirmations | 3m 36s, 9m 40s, and 7m 12s; 20m 28s total | [First](../.agents/runs/20261014T214441.421662Z-step8-S6/003-implementer-application.final.json), [second](../.agents/runs/20261014T214441.421662Z-step8-S6/006-implementer-application.final.json), [third](../.agents/runs/20261014T232917.621068Z-step8-S6/003-implementer-application.final.json) |
| Latest recovery run | About 25m 33s; no additional applied edit; a context detour followed by two inapplicable reviewer revisions | [Run progress](../.agents/runs/20261015T054546.394585Z-step8-S6/progress.log), [failure](../.agents/runs/20261015T054546.394585Z-step8-S6/error.txt) |
| Latest recovery review packet | 227,087 prompt characters; existing-code field about 177,335 serialized characters; Cargo.lock alone 106,769 content characters | [Review prompt](../.agents/runs/20261015T054546.394585Z-step8-S6/001-reviewer-patch-review.prompt.md) |
| Full Rust gates in the three resumed runs | Approximately 239s, 26s, 28s, and 29s across their four validation rounds | Gate summaries in the three resumed run directories |
| Current repository fingerprint | Median 30.87ms, maximum 61.07ms, ten local calls including the selected guide | Local read-only timing of `patches.repository_state()` |

Across the six PATCH envelopes, the provider reports 763,577 thinking tokens out
of 912,208 output tokens, or 83.7%. Their `duration_api_ms` totals are 99.6% of
their `duration_ms` totals. These are invocation-wide provider counters, not
final-patch token counts or a decomposition of model compute, queueing, and
network time. They strongly favor reducing model work over optimizing Python,
but do not establish a particular speedup from changing effort.

One successful single-edit fixture, including the final COMPLETE call, performs
27 repository fingerprints and finished in 0.222s with models and gates mocked
in a tiny temporary repository. That fixture measures orchestration overhead;
it is not an end-to-end throughput benchmark.

**Changes in priority order**

1. **Resolve the reviewer's broken repository access before further tuning.**
   The [latest context request](../.agents/runs/20261015T054546.394585Z-step8-S6/001-reviewer-patch-review.final.json)
   says reads failed with `bwrap: loopback: Failed RTM_NEWADDR: Operation not
   permitted`. Earlier reviews also explicitly report sandbox startup failures.
   The reviewer is producing security-sensitive revisions without being able to
   inspect or check them locally. Verify read access once at startup and either
   repair the supported sandbox environment or explicitly select a packet-based
   review mode. Repeated model calls cannot fix an unavailable execution
   environment. Preserve sandbox restrictions when fixing this.

   The current packet omitted `privileges.rs` because it exceeded the 16,000-byte
   dependency threshold. The reviewer then asked Claude for the entire file,
   consuming another 102.6s. Have Python satisfy structured file/range requests
   through the existing path checks; use the author only for questions about
   implementation intent. If packet-based review is selected, do not give the
   reviewer file references it cannot resolve. Relevant code:
   [existing_code and review_and_apply](../scripts/substep_crew_lib/orchestration.py).

2. **Remove the routine author application confirmation.**
   `review_and_apply()` already constructs a state-bound `ReviewedPatch` and
   `apply_reviewed()` verifies its identity, bytes, applicability, and base state.
   A fresh Claude call to echo its approval ID adds a model decision where Python
   can dispatch the authorized action. Start by applying unchanged APPROVED
   patches directly. For REVISED patches, either explicitly retain author
   assessment of semantic changes or adopt the findings-based review below;
   do not quietly assume that removing the second judgment preserves behavior.

   Current confirmation already disables read tools, uses medium effort, and
   permits an empty patch. The 20m 28s historical total predates these changes,
   so it is not a prediction of current savings. The remaining mandatory model
   call is still removable. With no retries and one final completion call, the
   current path needs `3N + 1` model invocations for N edits. Direct application
   reduces the all-approved path to `2N + 1`. That is a call-count reduction,
   not a claimed one-third wall-clock improvement. Relevant code:
   [review_and_apply](../scripts/substep_crew_lib/orchestration.py),
   [application_prompt](../scripts/substep_crew_lib/prompts.py), and
   [APPLICATION_SCHEMA](../scripts/substep_crew_lib/protocol.py).

3. **Stop making models serialize complete unified diffs.**
   A proposal is already a full implementation, including tests and integration,
   expressed as a JSON string. Claude cannot use editing or patch-check tools
   while constructing it. Python discovers bad context only after that expensive
   response finishes. Codex's REVISED response must repeat every retained file
   and hunk. The latest run spent roughly 23 minutes on two such revisions that
   still failed applicability.

   The preferred target is an isolated candidate workspace seeded from the
   user's current working tree, including relevant uncommitted and untracked
   files. Give Claude scoped editing tools there. Let Git generate the candidate
   diff and let the harness run checks. Have Codex return APPROVED or concise
   findings tied to files and requirements; Claude fixes findings in the same
   candidate workspace. Publish only the exact reviewed diff after verifying
   that the user's baseline is unchanged. Snapshot isolation must be enforced
   by tool permissions or a sandbox; changing the working directory alone is
   insufficient. A bare worktree at HEAD would omit the substantial current
   uncommitted changes and is not an adequate baseline here.

   This permits deleting most model-authored patch repair, full replacement
   revision, and application-confirmation machinery. It is a focused workflow
   redesign, not the first quick fix. A smaller intermediate option is scoped
   search/replace edits compiled into a diff by Python, but ambiguous matches
   still require strict rejection and feedback.

4. **Use less reasoning for routine drafting and preserve useful context.**
   The current `default` and `routine` profiles both use Claude at `high`;
   `routine` only lowers Codex's reviewer effort. It therefore does not directly
   address slow Claude proposals. Trial medium effort for bounded routine edits
   and reserve higher effort for difficult architecture/security changes or
   demonstrated failures. Measure acceptance and completed-substep rates as
   well as proposal latency. Anthropic documents effort as a soft control over
   thinking, text, and tool use, with a quality tradeoff; it is not a strict
   token budget. [Effort documentation](https://platform.claude.com/docs/en/build-with-claude/effort).

   Every `ClaudeEngine.run()` launches a new `claude -p` with no session resume.
   Retaining the engine object does not retain conversation context. Trial one
   explicitly identified author session per substep, sending the accepted diff,
   current-state information, and gate results as follow-ups. Keep reviewers
   independent, and reset/compact author context when it becomes stale or large.
   Existing cache-read counters show caching already occurs; session reuse is
   intended to reduce repeated discovery, not to guarantee a cache benefit.
   The installed CLI is 2.1.300 and its help supports resume and effort controls.
   [CLI reference](https://code.claude.com/docs/en/cli-reference).

5. **Trim context and documentation demands.**
   `existing_code()` bounds dependency contents but includes changed files in
   full up to 1 MiB each, with no aggregate changed-file budget. Supply the diff,
   nearby functions, and paths first; allow targeted reads or Python-served
   ranges. A small Cargo.lock edit should not force the entire lockfile into
   review. Apply this only alongside working read access or a functional range
   request protocol.

   `implement_prompt()` requires a large implementation record with architecture,
   failure behavior, walkthrough, diagram, verification, and deviations for each
   substep. Keep a short record of changed behavior, evidence, and limitations;
   request architectural exposition only when the actual task needs it. Attach
   observed gate results programmatically instead of asking a model to retell
   them. The current guide is 133,466 bytes and CLAUDE.md is 37,777 bytes. The
   selected section is already in the prompt, so avoid rediscovering historical
   material on every invocation. Safe mode disables automatic customizations,
   but the prompt still explicitly asks Claude to read CLAUDE.md.

   `compact_history()` is an existing improvement. It still includes every
   edit's author and reviewer explanation, patch hash, and two state hashes.
   A bounded list of completed requirements, affected paths, and unresolved
   findings is enough for most subsequent model turns; retain exact provenance
   in artifacts for the harness.

6. **Match validation to changed behavior and run full acceptance once.**
   `run()` validates after every applied edit, and `run_gates()` executes every
   selected command sequentially. Use focused checks after intermediate edits
   and all required acceptance gates on the final candidate. Classify an
   unavailable test dependency as an environment problem instead of reflexively
   requesting code changes. The earliest S6 gate failures included database
   connection refusal.

   Gate relevance also affects correctness: the resumed runs selected `rust`,
   even when one applied edit changed eight Terraform files. Introduce explicit
   path-to-gate mapping and a required final gate set; do not substitute `none`
   for appropriate validation. Documentation-only changes should avoid repeated
   full compilation, while Cargo/workspace/config changes need broader coverage.
   Run genuinely independent Rust and Terraform work concurrently only after
   checking resource conflicts. Parallel Cargo commands sharing one target
   directory and tests sharing a database can serialize or interfere.

   Remove Ollama as a synchronous log condenser. It is used only when deterministic
   extraction finds nothing, but can add up to 300 seconds per unfamiliar
   failure. Bounded diagnostic excerpts plus the complete-log path are sufficient
   input to the author. This also removes the condenser role and its schema and
   validation dependency on model adapters. No condenser delay was established
   in the measured October sample. Relevant code:
   [validation.py](../scripts/substep_crew_lib/validation.py).

7. **Bound retries and record actual stage metrics.**
   The default agent timeout is 3,600 seconds per invocation, with three attempts
   and separate context/review loops. These are large failure ceilings rather
   than useful throughput controls. Add an elapsed budget per edit/substep,
   phase-specific call limits, and explicit treatment of authentication,
   sandbox, transport, and malformed-output failures. Preserve a draft on budget
   exhaustion; restarting its full generation defeats the budget. CLI print
   mode also supports `--max-turns`, which exits with an error at the limit and
   therefore needs deliberate recovery handling.
   [CLI reference](https://code.claude.com/docs/en/cli-reference).

   `Result.seconds` exists, but `invoke()` does not persist it as normalized
   per-stage metadata. Save duration, model/effort, input size, provider usage,
   attempt, decision, failure class, and whether an edit was actually applied.
   Save stage transitions rather than relying only on the latest heartbeat
   snapshot. Optional streaming can expose tool activity; it primarily improves
   observability and perceived responsiveness, not the work required to produce
   an accepted patch. [Latency guidance](https://platform.claude.com/docs/en/test-and-evaluate/strengthen-guardrails/reduce-latency).

**Implementation sequence and boundaries**

First establish working reviewer access and timing records, remove confirmations
for unchanged approvals, and replace model-mediated file copying and condensation
with Python. Then trial medium author effort, smaller review packets, short
documentation records, and relevant intermediate gates. These changes can be
evaluated individually before adopting an isolated editing workspace and a
findings-based reviewer. Avoid building a new general agent framework to achieve
this simplification.

The desired main path is: scoped author edit in a candidate workspace, relevant
deterministic checks, independent review of that exact candidate, then guarded
application. Review findings return to the same author context. Final completion
must still cover the original requirements and required final gates. A passing
compiler or a requested COMPLETE flag alone does not establish completion.

Keep single-use review receipts, stale-state detection, path restrictions,
timeouts with descendant cleanup, resumable drafts, and original acceptance
criteria. Repeated fingerprint and patch checks can eventually be consolidated
between unchanged synchronous operations, while preserving checks around model
calls and the write boundary. Their measured cost makes that a low priority.
Likewise, most of `benchmark_substep_crew.py` targets guide scans and a retired
blocker-matching algorithm; it does not measure the actual latency problem.

Independent substeps can eventually run in isolated workspaces with an explicit
dependency order and integration review. Parallel writers in the present shared
tree would invalidate each other's repository fingerprints. First improve the
number of accepted edits per model hour before adding concurrency.

**Validation and uncertainty**

The existing mocked suite passed: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest
scripts.test_substep_crew` — 90 tests in 17.270 seconds. The suite checks protocol
and application behavior; it does not measure live model quality or verify the
reviewer's real sandbox. The local fingerprint experiment used ten read-only
calls; timings vary by machine and filesystem cache.

Historical Claude timing and token statistics were read from
`duration_ms`, `duration_api_ms`, `num_turns`, and `usage` in the four October
run directories. Review durations without provider metadata were taken from
saved progress transitions and are approximate. The sample contains repeated
attempts at one complex security/infrastructure substep, mixes earlier code
versions and efforts, and is too small for reliable tail-latency estimates.

Measure the proposed changes on matched starting snapshots with representative
routine and security-sensitive tasks. Track time to the first applicable patch,
time to an accepted edit, time to a completed substep, malformed-patch rate,
review findings, validation failures, and model usage. Count failed and timed-out
runs rather than excluding them. Re-run only checks affected by implementation
changes, including preservation of existing dirty files and rejection of stale
review receipts. There is no evidence yet for a specific end-to-end speedup.
