"""Read prior run history as context; never restore executable patch approvals."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re

from . import patches, progress, protocol


def target(run_dir: Path) -> tuple[int, int, str]:
    metadata = run_dir / 'run.json'
    if metadata.is_file():
        data = protocol.load_json(metadata.read_text(encoding='utf-8'))
        return int(data['phase']), int(data['step']), data['substep']
    # Runs written before explicit resume support still have the original scope
    # in their proposal prompt. Read it as data, never as fresh instructions.
    prompt = run_dir / '001-implementer-proposal.prompt.md'
    if prompt.is_file():
        match = re.search(r'^Phase (\d+) Step (\d+) (S\d+)\b', prompt.read_text(encoding='utf-8'), re.MULTILINE)
        if match:
            return int(match[1]), int(match[2]), match[3]
    raise protocol.CrewError('Cannot identify the phase, step and substep of the previous run.')


def load(value: str, phase: int, step: int, substep: str) -> tuple[Path, list[dict]]:
    if value == 'latest':
        root = progress.resolve_run('latest').parent
        candidates = sorted((path for path in root.iterdir() if path.is_dir() and not path.is_symlink() and not path.name.startswith('.')),
                            key=lambda path: path.name, reverse=True)
        run_dir = None
        for path in candidates:
            try:
                if target(path) == (phase, step, substep):
                    run_dir = path
                    break
            except (protocol.CrewError, ValueError, KeyError, OSError):
                continue
        if run_dir is None:
            raise protocol.CrewError('No previous run matches the selected phase, step and substep.')
    else:
        run_dir = progress.resolve_run(value)
    if target(run_dir) != (phase, step, substep):
        raise protocol.CrewError('Resume target does not match the selected phase, step and substep.')
    if (run_dir / 'completion.json').exists():
        raise protocol.CrewError('That run is already complete; select the next substep.')
    status = run_dir / 'progress.json'
    if status.is_file():
        data = protocol.load_json(status.read_text(encoding='utf-8'))
        if data.get('status') == 'running' and isinstance(data.get('pid'), int):
            if data['pid'] <= 0 or not progress.can_check_pid(data):
                raise protocol.CrewError('The previous run is marked active; its process cannot be checked from this environment.')
            try:
                os.kill(data['pid'], 0)
            except ProcessLookupError:
                pass
            except PermissionError:
                raise protocol.CrewError('The previous run may still be active; stop it before resuming.') from None
            else:
                raise protocol.CrewError('The previous run is still active; stop it before resuming.')
    history_path = run_dir / 'applied-history.json'
    history = protocol.load_json(history_path.read_text(encoding='utf-8')) if history_path.exists() else []
    if not isinstance(history, list):
        raise protocol.CrewError('Previous applied history must be a JSON array.')
    for item in history:
        if not isinstance(item, dict):
            raise protocol.CrewError('Previous applied history contains an invalid edit.')
        review = protocol.parse_response(json.dumps({
            key: item.get(key) for key in ('decision', 'patch', 'explanation')
        } | {'context_requests': []}), protocol.PATCH_REVIEW_SCHEMA)
        if review['decision'] not in {'APPROVED', 'REVISED'} or not review['patch'].strip() or len(review['patch']) > 200_000:
            raise protocol.CrewError('Previous applied history contains an unapproved edit.')
    return run_dir, history


def recover_proposal(run_dir: Path, guide_path: str) -> tuple[dict, Path] | None:
    """Recover unapproved draft data after the last applied edit, never an approval.

    Prefer a draft that still applies to the CURRENT tree. Otherwise keep the
    newest safely parseable draft for targeted repair. Resume captures a fresh
    fingerprint and requires a fresh review before any application.
    """
    history = run_dir / 'applied-history.json'
    cutoff = history.stat().st_mtime_ns if history.exists() else 0
    sources = sorted(run_dir.glob('*-implementer-proposal.final.json'), reverse=True)
    pending = run_dir / 'pending-proposal.json'
    if pending.exists():
        sources.insert(0, pending)
    fallback = None
    for path in sources:
        try:
            if path.is_symlink() or not path.is_file() or path.stat().st_mtime_ns <= cutoff or path.stat().st_size > 2_000_000:
                continue
            value = protocol.load_json(path.read_text(encoding='utf-8'))
            if path == pending:
                value = value['proposal']  # state is historical data, never authorization
            elif isinstance(value, dict) and value.get('type') == 'result':
                if value.get('is_error') is not False or value.get('subtype') != 'success':
                    continue
                value = value['structured_output']
            proposal = protocol.parse_proposal(json.dumps(value, ensure_ascii=False))
            if proposal['action'] != 'PATCH':
                continue
            patches.patch_paths(proposal['patch'], guide_path, check=False)
            if fallback is None:
                fallback = proposal, path
            try:
                patches.patch_paths(proposal['patch'], guide_path)
            except protocol.ProtocolError:
                continue
            return proposal, path
        except (protocol.CrewError, OSError, ValueError, KeyError, TypeError):
            continue
    return fallback


def recover_review(run_dir: Path) -> dict | None:
    """Recover outstanding findings as repair context, never as approval.

    Read validated review artifacts from older candidate runs too. The pending
    draft carries feedback across resumes that stop before another review.
    """
    history = run_dir / 'applied-history.json'
    cutoff = history.stat().st_mtime_ns if history.exists() else 0
    sources = [*run_dir.glob('*-reviewer-review.validated.json'), run_dir / 'pending-proposal.json']
    sources = [path for path in sources if path.is_file() and not path.is_symlink()
               and path.stat().st_mtime_ns > cutoff]
    for path in sorted(sources, key=lambda item: (item.stat().st_mtime_ns, item.name), reverse=True):
        try:
            if path.stat().st_size > 2_000_000:
                return None
            value = protocol.load_json(path.read_text(encoding='utf-8'))
            if path.name == 'pending-proposal.json':
                if 'review' not in value:  # Older drafts did not store review feedback.
                    continue
                value = value['review']
            review = protocol.parse_review(json.dumps(value, ensure_ascii=False))
            return review if review['decision'] == 'CHANGES_REQUIRED' else None
        except (protocol.CrewError, OSError, ValueError, TypeError):
            return None
    return None
