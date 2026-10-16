"""Complete one substep with isolated editing, validation, and independent review.

Claude edits a private snapshot through scoped file tools. Git generates the diff;
Codex returns approval or findings. Python publishes only validated, reviewed bytes.
Both models have shell execution disabled. Source changes remain unstaged.
Opt-in --aws-profile/--aws-account-id exposes cloud tools; each cloud write needs
your approval of a prepared request via --resume RUN_DIR --approve-cloud ID.

Use --profile security for deeper reasoning, --gates auto for path-based checks,
--resume to recover an unapproved draft, and --status/--watch to monitor a run.
--gates none explicitly disables deterministic validation. Do not run another
source writer concurrently: stale source state invalidates publication.
Exit codes: 0 complete, 1 validation failed, 2 error, 3 blocked/external action.

Examples:
    python3 scripts/substep_crew.py --dry-run
    python3 scripts/substep_crew.py --phase 4 --step 8 --substep S6 --profile security
    python3 scripts/substep_crew.py --resume
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

from . import checkpoints, cloud, config, engines, guides, orchestration, processes, progress, prompts, protocol, validation


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
    p.add_argument('--profile', choices=sorted(engines.PROFILES), default='default',
                   help='default: medium author / high reviewer; security: high author / xhigh reviewer')
    p.add_argument('--gates', choices=sorted(validation.GATE_SETS), default='auto', help="path-based checks plus final acceptance; 'none' explicitly disables gates")
    p.add_argument('--review-policy', choices=('gates-first', 'always'), default=None,
                   help='deprecated compatibility option; every patch is always reviewed before application')
    p.add_argument('--max-rounds', type=int, default=3, help='maximum consecutive failed validations; final failure receives no edit')
    p.add_argument('--max-edits', type=int, default=12, help='maximum candidate author turns including fixes')
    p.add_argument('--review-attempts', type=int, default=3, help='response/turn-limit attempts per invocation; review fixes use --max-edits and --run-timeout')
    p.add_argument('--agent-timeout', type=int, default=900, help='seconds per model invocation')
    p.add_argument('--gate-timeout', type=int, default=1800, help='seconds per gate process group')
    p.add_argument('--run-timeout', type=int, default=2700, help='total seconds for this run; partial candidates are retained')
    p.add_argument('--author-effort', choices=('low', 'medium', 'high', 'xhigh', 'max'), help='override Claude author effort')
    p.add_argument('--author-max-turns', type=int, help='Claude tool-turn limit per invocation (default: 60); progress-making limits have bounded continuations')
    p.add_argument('--skip-implement', action='store_true', help='validate the existing tree first, then complete/fix through reviewed proposals')
    p.add_argument('--resume', nargs='?', const='latest', metavar='RUN_DIR',
                   help='restore applied history and an unapproved draft, validate current files, then review/repair the draft')
    p.add_argument('--fresh-proposal', action='store_true', help='with --resume, generate a new proposal instead of recovering an unapplied draft')
    p.add_argument('--aws-profile', help='opt in to live staging AWS tools using this local named profile')
    p.add_argument('--aws-region', help='AWS region for live tools (default: us-east-2)')
    p.add_argument('--aws-account-id', help='required expected 12-digit staging account for live tools')
    p.add_argument('--approve-cloud', metavar='REQUEST_ID', help='with --resume, approve one exact prepared cloud write; never blanket permission')
    monitor = p.add_mutually_exclusive_group()
    monitor.add_argument('--status', nargs='?', const='latest', metavar='RUN_DIR',
                         help='show a saved progress report without running agents')
    monitor.add_argument('--watch', nargs='?', const='latest', metavar='RUN_DIR',
                         help='watch saved progress until the run stops; Ctrl-C exits the monitor only')
    p.add_argument('--raw-output', action='store_true', help='also print raw model and gate output; full transcripts are always saved')
    p.add_argument('--dry-run', action='store_true', help='print prompts; create no artifacts and run no models or gates')
    a = p.parse_args()
    if a.fresh_proposal and a.resume is None:
        p.error('--fresh-proposal requires --resume')
    if a.approve_cloud and (a.resume is None or a.fresh_proposal):
        p.error('--approve-cloud requires --resume and cannot use --fresh-proposal')
    if a.status is not None or a.watch is not None:
        if a.resume is not None or a.dry_run:
            p.error('--status/--watch cannot be combined with --resume or --dry-run')
        try:
            return progress.monitor(a.watch if a.watch is not None else a.status, watch=a.watch is not None)
        except (protocol.CrewError, OSError, ValueError, KeyError) as exc:
            print(f'!! Cannot read progress: {exc}')
            return 2
    for name in ('max_rounds', 'max_edits', 'review_attempts', 'agent_timeout', 'gate_timeout', 'run_timeout'):
        if getattr(a, name) < 1:
            p.error(f"--{name.replace('_', '-')} must be at least 1")
    if a.author_max_turns is not None and a.author_max_turns < 1:
        p.error('--author-max-turns must be at least 1')

    if a.resume is not None:
        try:
            if a.resume == 'latest':
                candidates = sorted(config.RUNS.glob('*'), reverse=True)
                for candidate in candidates:
                    if not candidate.is_dir() or candidate.is_symlink() or (candidate / 'completion.json').exists():
                        continue
                    try:
                        identity = checkpoints.target(candidate)
                    except (protocol.CrewError, ValueError, KeyError, OSError):
                        continue
                    requested = (a.phase, a.step, a.substep.upper() if a.substep else None)
                    if all(want is None or want == actual for want, actual in zip(requested, identity)):
                        a.resume = str(candidate)
                        break
                else:
                    raise protocol.CrewError('No unfinished run matches the requested target.')
            prior_phase, prior_step, prior_substep = checkpoints.target(progress.resolve_run(a.resume))
        except (protocol.CrewError, OSError, ValueError, KeyError) as exc:
            print(f'!! Cannot resume: {exc}')
            return 2
        a.phase = a.phase if a.phase is not None else prior_phase
        a.step = a.step if a.step is not None else prior_step
        a.substep = a.substep if a.substep is not None else prior_substep

    history = guides.git('log', '--pretty=%s', '-200').splitlines()
    phase, step = (a.phase, a.step) if a.phase and a.step else guides.detect_phase_step(history)
    found = guides.find_guide(phase, step, include_body=True)
    assert isinstance(found, tuple)
    guide_path, guide_body = found
    guide = guides.parse_guide(guide_path, phase, step, guide_body)
    print(f'\n=== phase {phase} step {step} — {guide.rel}')
    sub = guides.resolve_substep(guide, a.substep, history)
    print(f'  target        : {sub.id} — {sub.title}  ({guide.rel}:{sub.line})')
    restored_history = []
    restored_proposal = None
    restored_review = None
    proposal_source = None
    resume_source = None
    if a.resume is not None:
        try:
            resume_source, restored_history = checkpoints.load(a.resume, phase, step, sub.id)
        except (protocol.CrewError, OSError, ValueError, KeyError) as exc:
            print(f'!! Cannot resume: {exc}')
            return 2
        print(f'  resume source : {resume_source.name}')
        print(f'  restored edits: {len(restored_history)}; fresh candidate checks and review required before publication')
        if not a.fresh_proposal:
            recovered = checkpoints.recover_proposal(resume_source, guide.rel)
            restored_review = checkpoints.recover_review(resume_source)
            if recovered is not None:
                restored_proposal, proposal_source = recovered
                print(f'  draft recovery: {proposal_source.name}; unapproved, requires current checks and fresh review')
            if restored_review:
                print(f'  review recovery: {len(restored_review["findings"])} outstanding finding(s) to address first')
    sha = guides.git('rev-parse', 'HEAD')
    cloud_options = None
    cloud_approval = None
    try:
        if a.approve_cloud:
            if not re.fullmatch(r'[a-f0-9]{64}', a.approve_cloud):
                p.error('--approve-cloud requires the exact 64-character request ID')
            path = resume_source / 'cloud' / f'request-{a.approve_cloud}.json'
            if path.is_symlink() or path.parent.is_symlink():
                raise protocol.CrewError('Cloud approval request cannot be a symlink')
            request = protocol.load_json(path.read_text())
            if request.get('id') != a.approve_cloud:
                raise protocol.CrewError('Cloud approval ID does not match the saved request')
            cloud_options = request['aws']
            cloud_approval = request, path
        if a.aws_profile or a.aws_account_id or a.aws_region:
            cloud_options = cloud.settings(a.aws_profile, a.aws_region or 'us-east-2', a.aws_account_id)
        if cloud_options:
            cloud_options = cloud.settings(**cloud_options)
    except (protocol.CrewError, OSError, ValueError, KeyError, TypeError) as exc:
        print(f'!! Cannot configure cloud access: {exc}')
        return 2
    pre = '\n'.join(f'  {line}' for line in guides.git('status', '--porcelain').splitlines())
    roles = {key: factory() for key, factory in engines.PROFILES[a.profile].items()}
    if a.author_effort:
        author = roles['implementer']
        if not isinstance(author, engines.ClaudeEngine):
            p.error('--author-effort requires a Claude implementer')
        author.effort = a.author_effort
        author.name = f'claude:{author.model} (effort={author.effort})'
    if a.author_max_turns is not None:
        author = roles['implementer']
        if not isinstance(author, engines.ClaudeEngine):
            p.error('--author-max-turns requires a Claude implementer')
        author.max_turns = a.author_max_turns
    print(f'  baseline      : {sha[:12]}')
    print(f'  profile       : {a.profile}')
    print('  review policy : candidate checks + independent review before publication')
    print('  file access   : scoped candidate tools; shell disabled for both models')
    if cloud_options:
        print(f'  live AWS      : {cloud_options["profile"]} / {cloud_options["account"]} / {cloud_options["region"]}')
        print('  cloud writes  : independent review + your approval of each prepared operation')
    if a.review_policy:
        print('  --review-policy is deprecated; it cannot bypass patch review')
    for role, engine in roles.items():
        print(f'    {role:<12}: {engine.name}')
    if isinstance(roles['implementer'], engines.ClaudeEngine):
        print(f'  author turns  : {roles["implementer"].max_turns} per invocation; up to {a.review_attempts} response/continuation attempts')
    gate_count = len(validation.GATE_SETS[a.gates])
    print(f'  gates         : {a.gates}' + (' — selected by affected paths' if a.gates == 'auto' else
                                           ' — intentionally none' if a.gates == 'none' else f' ({gate_count} required at completion)'))

    if a.dry_run:
        objective = prompts.implement_prompt(guide, sub, sha, pre)
        if cloud_options:
            objective += prompts.cloud_instructions(cloud_options)
        print('\n--- proposal prompt ---\n' + objective)
        if restored_proposal or restored_review:
            print(prompts.followup_prompt('Inspect the restored draft and resolve outstanding findings first.',
                                          restored_history, review=restored_review))
        print('\n--- review prompt ---\n' + prompts.patch_review_prompt(
            prompts.substep_context(guide, sub, sha, pre), '(Git-generated candidate diff)',
            {'action': 'COMPLETE', 'explanation': '(brief summary)', 'completion_evidence': []},
            '(actual checks)', ['(changed paths)'], final=True,
        ))
        return 0

    run = _unique_run_dir(step, sub.id)
    (run / 'run.json').write_text(json.dumps({
        'phase': phase, 'step': step, 'substep': sub.id, 'guide': guide.rel,
        'baseline': sha, 'profile': a.profile, 'gates': a.gates, 'workflow': 'candidate-v1',
        'run_timeout': a.run_timeout, 'engines': {role: engine.name for role, engine in roles.items()},
        'author_max_turns': getattr(roles['implementer'], 'max_turns', None),
        'cloud': cloud_options,
        'approved_cloud_request': a.approve_cloud,
        'resumed_from': str(resume_source) if resume_source else None,
        'restored_edits': len(restored_history),
        'recovered_proposal': str(proposal_source) if proposal_source else None,
    }, indent=2), encoding='utf-8')
    if resume_source is not None:
        (run / 'applied-history.json').write_text(json.dumps(restored_history, indent=2, ensure_ascii=False), encoding='utf-8')
    (run / 'baseline.diff').write_text(guides.git('diff', '--no-ext-diff', '--no-textconv', 'HEAD'), encoding='utf-8')
    print(f'  artifacts     : {run.relative_to(config.REPO)}\n')
    print(f'  monitor       : python3 scripts/substep_crew.py --watch {run.relative_to(config.REPO)}')
    reporter = progress.Reporter(run, phase, step, sub.id, sub.title, applied_edits=len(restored_history))
    try:
        with reporter, processes.console_output(a.raw_output):
            loop = orchestration.PatchLoop(
                guide, sub, sha, pre, roles, run, agent_timeout=a.agent_timeout,
                gate_timeout=a.gate_timeout, gate_set=a.gates, max_edits=a.max_edits,
                max_rounds=a.max_rounds, attempts=a.review_attempts,
                restored_history=restored_history if resume_source is not None else None, reporter=reporter,
                restored_proposal=restored_proposal, restored_review=restored_review, run_timeout=a.run_timeout,
            )
            if cloud_options:
                loop.cloud = cloud.CloudAccess(run, cloud_options, deadline=time.time() + a.run_timeout)
                loop.cloud_approval = cloud_approval
                if resume_source and not a.fresh_proposal and not cloud_approval:
                    pending = resume_source / 'cloud/pending.json'
                    if pending.is_file() and not pending.is_symlink():
                        loop.cloud.queue(protocol.load_json(pending.read_text()))
            code = loop.run(skip_implement=a.skip_implement)
            if code == 1:
                reporter.update(status='failed', error='Required validation failed; see the gate summaries.')
                print('Required validation failed. See the complete gate transcripts.')
        print(f'    artifacts: {run.relative_to(config.REPO)}')
        return code
    except (protocol.CrewError, OSError, UnicodeError, subprocess.SubprocessError) as exc:
        (run / 'error.txt').write_text(str(exc), encoding='utf-8')
        print(f'\n!! {exc}\n    artifacts: {run.relative_to(config.REPO)}')
        return 2
