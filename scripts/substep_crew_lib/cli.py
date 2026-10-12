"""substep_crew.py — propose/review/apply/validate loop for one phase substep.

    Claude proposes a coherent diff -> Codex returns a complete reviewed diff
              ^                              |
              |                    Claude requests exact application
              +--------- required gates <----+

Claude's JSON APPLY action is executed by one guarded git-apply boundary. Both
models have read-only repository access; shell/edit tools, MCPs and custom hooks
are disabled for Claude. Codex uses a read-only sandbox with approvals disabled
and integrations disabled. A modified reviewed diff must be proposed again.
No model-selected shell command is ever executed by this harness.

Reviews use APPROVED (identical patch), REVISED (complete replacement), or
NEEDS_CONTEXT (no implementation). Failures and context exchanges are bounded
by --review-attempts; stale repository snapshots fail immediately. Each applied
edit runs the selected gates before another proposal. --max-rounds bounds
consecutive failed validations; --max-edits bounds the total applied edits.
Completion requires Claude's acceptance-criterion evidence, a guide record,
and passing configured gates. --gates none is an explicit validation opt-out.

Only UTF-8 text diffs for regular files are supported. Renames can be expressed
as deletion/addition. Ignored paths other than the selected guide are refused.
Do not run another writer concurrently: snapshots detect changes between model
calls but cannot lock out arbitrary external processes during git apply.

Roles and who runs them:

    implementer  claude -p            plans, proposes, requests application
    reviewer     codex exec           read-only sandbox, returns reviewed code
    condenser    ollama (local HTTP)  squeezes gate logs, no tools

The cross-model split is deliberate: the reviewer is a different model family
from the author, and the gates (fmt/clippy/test) are run by THIS script rather
than by an agent claiming it ran them.

Nothing here commits, pushes, or stages. The final staging decision stays yours.

Each noisy invocation stores a complete ``*.transcript.log`` and retains only
a bounded in-memory excerpt. Model calls also save a complete ``*.final.*``
response. Excerpt and summary limits are Unicode character counts, not tokens.

Claude invocations honor ``CLAUDE_CONFIG_DIR``. When it is unset, the harness
uses ``~/.claude-personal`` if that directory exists, otherwise ``~/.claude``;
``PAI_DIR`` is kept aligned. Safe mode preserves authentication while disabling
customizations; repository instructions must be read explicitly.

On POSIX, child processes run in a separate session. Timeout or interruption
terminates the whole process group, escalating to SIGKILL after a short grace.
Process-group cleanup is POSIX-only; non-POSIX child signaling is best-effort,
and selector support for subprocess pipes is platform-dependent.

Usage:
    scripts/substep_crew.py --dry-run                 # resolve substep, print prompts
    scripts/substep_crew.py                           # next substep, auto-detected
    scripts/substep_crew.py --substep S5 --step 8
    scripts/substep_crew.py --profile local           # free harness test on Ollama
    scripts/substep_crew.py --gates terraform --max-rounds 4"""

from __future__ import annotations

import argparse
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from . import config, engines, guides, orchestration, prompts, protocol, validation


def _unique_run_dir(step: int, substep: str) -> Path:
    base = config.RUNS / f"{datetime.now(timezone.utc):%Y%m%dT%H%M%S.%fZ}-step{step}-{substep}"
    candidate = base
    serial = 1
    while candidate.exists():
        candidate = base.with_name(f"{base.name}-{serial}")
        serial += 1
    candidate.mkdir(parents=True)
    return candidate


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--phase', type=int, help='default: detected from the 40 newest commit subjects')
    p.add_argument('--step', type=int, help='default: detected from the 40 newest commit subjects')
    p.add_argument('--substep', help='e.g. S5; default: first not recorded and not committed')
    p.add_argument('--profile', choices=sorted(engines.PROFILES), default='default')
    p.add_argument('--gates', choices=sorted(validation.GATE_SETS), default='rust', help="'none' explicitly disables deterministic gates")
    p.add_argument('--review-policy', choices=('gates-first', 'always'), default=None,
                   help='deprecated compatibility option; every patch is always reviewed before application')
    p.add_argument('--max-rounds', type=int, default=3, help='maximum consecutive failed validations; final failure receives no edit')
    p.add_argument('--max-edits', type=int, default=24, help='maximum reviewed/applied coherent edits')
    p.add_argument('--review-attempts', type=int, default=3, help='bounded response retries and review/context/reproposal exchanges')
    p.add_argument('--agent-timeout', type=int, default=3600, help='seconds per model invocation')
    p.add_argument('--gate-timeout', type=int, default=1800, help='seconds per gate process group')
    p.add_argument('--local-model', help='override the Ollama model for condensing gate logs')
    p.add_argument('--skip-implement', action='store_true', help='validate the existing tree first, then complete/fix through reviewed proposals')
    p.add_argument('--dry-run', action='store_true', help='print prompts; create no artifacts and run no models or gates')
    a = p.parse_args()
    for name in ('max_rounds', 'max_edits', 'review_attempts', 'agent_timeout', 'gate_timeout'):
        if getattr(a, name) < 1:
            p.error(f"--{name.replace('_', '-')} must be at least 1")

    history = guides.git('log', '--pretty=%s', '-200').splitlines()
    phase, step = (a.phase, a.step) if a.phase and a.step else guides.detect_phase_step(history)
    found = guides.find_guide(phase, step, include_body=True)
    assert isinstance(found, tuple)
    guide_path, guide_body = found
    guide = guides.parse_guide(guide_path, phase, step, guide_body)
    print(f'\n=== phase {phase} step {step} — {guide.rel}')
    sub = guides.resolve_substep(guide, a.substep, history)
    print(f'  target        : {sub.id} — {sub.title}  ({guide.rel}:{sub.line})')
    sha = guides.git('rev-parse', 'HEAD')
    pre = '\n'.join(f'  {line}' for line in guides.git('status', '--porcelain').splitlines())
    roles = {key: factory() for key, factory in engines.PROFILES[a.profile].items()}
    if a.local_model:
        roles = {
            key: (engines.OllamaEngine(a.local_model) if isinstance(engine, engines.OllamaEngine) else engine)
            for key, engine in roles.items()
        }
    print(f'  baseline      : {sha[:12]}')
    print(f'  profile       : {a.profile}')
    print('  review policy : every patch, before application')
    if a.review_policy:
        print('  --review-policy is deprecated; it cannot bypass patch review')
    for role, engine in roles.items():
        print(f'    {role:<12}: {engine.name}')
    gate_count = len(validation.GATE_SETS[a.gates])
    print(f'  gates         : {a.gates} ({gate_count})' + (' — intentionally none' if not gate_count else ''))

    if a.dry_run:
        objective = prompts.implement_prompt(guide, sub, sha, pre)
        print('\n--- proposal prompt ---\n' + objective)
        print('\n--- patch review prompt ---\n' + prompts.patch_review_prompt(
            prompts.substep_context(guide, sub, sha, pre), {'patch': '(proposed coherent diff)', 'context': '(inspected context)'},
            {'path': '(current file contents)'}, [], '(gates not yet run)',
        ))
        return 0

    run = _unique_run_dir(step, sub.id)
    (run / 'baseline.diff').write_text(guides.git('diff', '--no-ext-diff', '--no-textconv', 'HEAD'), encoding='utf-8')
    print(f'  artifacts     : {run.relative_to(config.REPO)}\n')
    try:
        loop = orchestration.PatchLoop(
            guide, sub, sha, pre, roles, run, agent_timeout=a.agent_timeout,
            gate_timeout=a.gate_timeout, gate_set=a.gates, max_edits=a.max_edits,
            max_rounds=a.max_rounds, attempts=a.review_attempts,
        )
        code = loop.run(skip_implement=a.skip_implement)
        if code:
            print('Required validation failed. See the complete gate transcripts.')
        print(f'    artifacts: {run.relative_to(config.REPO)}')
        return code
    except (protocol.CrewError, OSError, UnicodeError, subprocess.SubprocessError) as exc:
        (run / 'error.txt').write_text(str(exc), encoding='utf-8')
        print(f'\n!! {exc}\n    artifacts: {run.relative_to(config.REPO)}')
        return 2
