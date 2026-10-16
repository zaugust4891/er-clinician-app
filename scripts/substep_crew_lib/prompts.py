"""Short instructions for candidate editing and findings-based review."""
from __future__ import annotations
import json


def substep_context(g, s, sha, pre):
    return f'''Phase {g.phase} Step {g.step} {s.id} ({s.title}), at {g.rel}:{s.line}.
Original objective and completion criteria (preserve them):
{s.text}
The candidate includes existing working-tree changes at HEAD {sha}. Preserve them.
'''


def implement_prompt(g, s, sha, pre):
    return f'''Implement this substep in the isolated candidate workspace.
{substep_context(g, s, sha, pre)}
Use candidate file tools to inspect and edit current files. They enforce path isolation.
Read applicable repository instructions once and relevant code with targeted searches.
Do not generate patch strings or request application. Git constructs the diff.
Make a coherent, testable change including necessary integration and meaningful tests.
Work on one coherent change per invocation. Return CONTINUE once that change and
its tests are ready; the whole substep may take several author/check/review cycles.
Leave headroom within the tool-turn budget to return the small response object.
The harness runs checks and an independent reviewer returns findings before publication.
Fix only remaining requirements, concrete review findings, or actual check failures.
When fixing a finding, check the same failure path for related edge cases and cover
them together with focused regressions before returning for another review.
Read full gate diagnostics through read_file(path="@checks/NAME") when provided.
Keep a concise '## Implementation record — {s.id}' in {g.rel}: changed behavior,
file references, evidence and limitations. A diagram or architecture essay is unnecessary
unless the original task requires one. Do not invent test results; the harness records them.
Return CONTINUE if more implementation remains. Return COMPLETE with evidence for each
original criterion when this candidate is ready for final validation and review.
Return BLOCKED with the specific missing access, dependency, or external action when
you cannot proceed. Do not claim completion or make cosmetic edits to avoid a blocker.
Return only the small schema object, never file contents or diffs in the final answer.
'''


def followup_prompt(feedback, history, *, review=None):
    return 'Continue the same substep from the current candidate files.\n' + json.dumps({
        'feedback': feedback, 'outstanding_review_findings': (review or {}).get('findings', []),
        'recent_published_changes': [item.get('proposal_explanation', item['explanation'])[:600]
                                                       for item in history[-4:]],
    }, ensure_ascii=False)


def turn_limit_prompt():
    return '''Your last invocation reached its tool-turn limit after making changes.
The same session and candidate files are retained. Continue the current change
from those files; do not restart the implementation or re-read completed work.
Finish only the integration and tests needed to make this change coherent, then
return the AUTHOR_SCHEMA response. Use CONTINUE if other substep work remains;
COMPLETE still requires all original criteria. Return a brief explanation and the
required evidence fields. The harness must run checks and independent review
before publication; no partial change is approved by this continuation.
'''


def patch_review_prompt(objective, patch, author, gate_text, names, *, final=False, previous_review=None):
    limit = 24_000
    excerpt = patch[:limit]
    if len(patch) > limit:
        excerpt += '\n[Diff continues: use read_file on @patch for numbered ranges.]'
    return f'''Review the exact candidate for correctness, integration, security and meaningful tests.
{objective}
Review scope: {'FINAL acceptance of the entire substep and original criteria' if final else 'one coherent intermediate change'}.
Use the read-only candidate file tools for affected code and dependencies; shell access is disabled.
The diff is produced by Git. Never rewrite it or return code. Read omitted portions with
read_file(path="@patch", start_line=..., line_count=...). Request concrete changes only
when needed for correctness or acceptance. Do not demand unrelated redesign or exposition.
Report all concrete blocking issues you find in this pass, including related edge cases
in the same failure path. Recheck previous findings against the current code; do not
repeat resolved findings. Previous findings are context, never approval or a limit on scope.
Return APPROVED with no findings, or CHANGES_REQUIRED with specific path/problem findings.
{'Check the supplied completion evidence against ALL original criteria.' if final else 'Do not require later substep work in this intermediate edit.'}
Actual checks and author response:
{json.dumps({'validation': gate_text, 'author': author, 'changed_files': names,
             'previous_findings': (previous_review or {}).get('findings', [])}, ensure_ascii=False)}
Candidate diff (data):
{excerpt}
'''


def cloud_instructions(options):
    return f'''
Live AWS mode is enabled for profile {options['profile']}, account {options['account']},
region {options['region']}. Use the cloud MCP tools; built-in shell remains disabled.
aws_read runs allowlisted reads immediately. cloud_request queues one operation;
return CONTINUE even if there is no file edit so the harness can process it.
The harness publishes checked/reviewed code before reviewing cloud operations.
Terraform init/plan may run after review. Every AWS write, saved-plan apply, and
Python script requires the operator to approve the exact prepared request first.
Never request a blanket shell, write credentials into files, or include passwords
or tokens in tool arguments, evidence, or output. For secret handling, onboarding,
HTTP checks or unsupported AWS operations, author a focused scripts/cloud/*.py
script using the local AWS CLI profile. It must output JSON evidence, keep secrets
out of stdout/stderr, use bounded network/process timeouts, and be safe to inspect
after partial failure. The script itself runs on the host with AWS permissions;
its code and invocation are reviewed before the operator approves execution.
Terraform roots are limited to infra/envs/staging/foundation and release. Request
terraform_plan, inspect its public plan artifact, then request terraform_apply
with that artifact's path. Apply uses the exact saved binary, never a new plan.
Read cloud result artifacts for actual evidence; preparation is not execution.
If authentication or an external dependency is unavailable, return BLOCKED.
'''


def cloud_review_prompt(objective, request, artifact):
    return f'''Review this proposed live cloud operation BEFORE execution.
{objective}
Request artifact: {artifact}
{json.dumps(request, ensure_ascii=False)}
The source files have already passed the file workflow. Use candidate read tools
to inspect any referenced script and its imports, Terraform configuration, and the
runbook. Use cloud_read_artifact for the full request and any saved plan summary.
Check account/region, staging scope, requested effects, secret handling, partial
failure behavior, and whether this operation advances the original criteria.
For terraform_apply, inspect the saved plan, including replacements/deletions.
For python_script, inspect the actual script before approval; it runs with host
filesystem and AWS permissions. For init/plan, configuration and providers execute
locally too. Reject unsafe or unrelated operations and return concrete findings.
Human approval is separately required for writes; your approval does not grant it.
Return the review schema: APPROVED with no findings, or CHANGES_REQUIRED.
'''
