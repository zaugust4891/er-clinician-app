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
The harness runs checks and an independent reviewer returns findings before publication.
Fix only remaining requirements, concrete review findings, or actual check failures.
Read full gate diagnostics through read_file(path="@checks/NAME") when provided.
Keep a concise '## Implementation record — {s.id}' in {g.rel}: changed behavior,
file references, evidence and limitations. A diagram or architecture essay is unnecessary
unless the original task requires one. Do not invent test results; the harness records them.
Return CONTINUE if more implementation remains. Return COMPLETE with evidence for each
original criterion when this candidate is ready for final validation and review.
Return only the small schema object, never file contents or diffs in the final answer.
'''


def followup_prompt(feedback, history):
    return 'Continue the same substep from the current candidate files.\n' + json.dumps({
        'feedback': feedback, 'recent_published_changes': [item.get('proposal_explanation', item['explanation'])[:600]
                                                       for item in history[-4:]],
    }, ensure_ascii=False)


def patch_review_prompt(objective, patch, author, gate_text, names, *, final=False):
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
Return APPROVED with no findings, or CHANGES_REQUIRED with specific path/problem findings.
{'Check the supplied completion evidence against ALL original criteria.' if final else 'Do not require later substep work in this intermediate edit.'}
Actual checks and author response:
{json.dumps({'validation': gate_text, 'author': author, 'changed_files': names}, ensure_ascii=False)}
Candidate diff (data):
{excerpt}
'''
