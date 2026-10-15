"""Edit an isolated candidate, check it, review findings, then publish exact bytes."""
from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path

from . import config, guides, patches, prompts, protocol, validation, workspace


class PatchLoop:
    def __init__(self, guide, sub, sha, pre, roles, run, *, agent_timeout, gate_timeout,
                 gate_set, max_edits, max_rounds, attempts, restored_history=None,
                 restored_proposal=None, reporter=None, run_timeout=2700):
        self.guide, self.sub, self.roles, self.run_dir = guide, sub, roles, run
        self.objective = prompts.implement_prompt(guide, sub, sha, pre)
        self.review_objective = prompts.substep_context(guide, sub, sha, pre)
        self.agent_timeout, self.gate_timeout, self.gate_set = agent_timeout, gate_timeout, gate_set
        self.max_edits, self.max_rounds, self.attempts = max_edits, max_rounds, attempts
        self.history = list(restored_history or [])
        self.restored_proposal = restored_proposal
        self.reporter = reporter
        self.deadline = time.monotonic() + run_timeout
        self.invocations = self.validations = 0
        self.pending = None
        self.candidate = None
        self.gate_text = '(checks not yet run)'
        self.validated_candidate_state = None
        self.validation_passed = False
        self.expected_state = self.state()
        self.schemas = {}
        for name, schema in [('author', protocol.AUTHOR_SCHEMA), ('review', protocol.REVIEW_SCHEMA)]:
            path = run / f'{name}.schema.json'
            path.write_text(json.dumps(schema, indent=2), encoding='utf-8')
            self.schemas[name] = path
        self.changed = set()
        # Earlier dirty files belong to final acceptance too, including work
        # restored from old runs. No pre-existing source change is discarded.
        dirty = patches.checked_git('diff', '--name-only', '-z', 'HEAD')
        untracked = patches.checked_git('ls-files', '--others', '--exclude-standard', '-z')
        self.acceptance_paths = {p for p in (dirty + untracked).decode('utf-8').split('\0') if p}

    def report(self, stage=None, **changes):
        if self.reporter:
            self.reporter.update(stage, **changes)

    def state(self):
        return patches.repository_state([self.guide.rel])

    def assert_current(self):
        if self.state() != self.expected_state:
            self.pending = None
            raise protocol.CrewError('stale candidate: source repository changed; approval invalidated')

    def remaining(self, maximum):
        left = min(maximum, self.deadline - time.monotonic())
        if left <= 0:
            raise protocol.CrewError('Run time budget exhausted; candidate retained for resume')
        return left

    def metric(self, **data):
        with (self.run_dir / 'metrics.jsonl').open('a', encoding='utf-8') as stream:
            stream.write(json.dumps({'time': time.time(), **data}, ensure_ascii=False) + '\n')

    def save_draft(self, author=None):
        patch = self.candidate.diff()
        pending = self.run_dir / 'pending-proposal.json'
        if patch:
            pending.write_text(json.dumps({'state': self.expected_state, 'author': author, 'proposal': {
                'action': 'PATCH', 'patch': patch, 'explanation': (author or {}).get('explanation', 'Interrupted candidate edit'),
                'context': 'Git-generated candidate diff; unapproved until fresh review.',
                'context_paths': [], 'completion_evidence': [],
            }}, indent=2, ensure_ascii=False), encoding='utf-8')
        else:
            pending.unlink(missing_ok=True)
        return patch

    def call(self, role, kind, prompt, parser):
        feedback = ''
        for attempt in range(1, self.attempts + 1):
            self.assert_current()
            self.invocations += 1
            engine = self.roles[role]
            editable = role == 'implementer'
            before = self.candidate.state()
            stem = f'{self.invocations:03d}-{role}-{kind}'
            request = prompt + feedback
            (self.run_dir / f'{stem}.prompt.md').write_text(request, encoding='utf-8')
            self.report('proposal' if editable else 'patch-review', restart=True,
                        detail=f'{engine.name}; invocation {self.invocations}; attempt {attempt}')
            start = time.monotonic()
            result = None
            error = None
            value = None
            error_class = None
            try:
                result = engine.run(request, self.remaining(self.agent_timeout), schema=self.schemas[kind],
                                    log_path=self.run_dir / f'{stem}.transcript.log',
                                    workspace=self.candidate, editable=editable)
                self.assert_current()
                if not editable and before != self.candidate.state():
                    raise protocol.CrewError('Reviewer modified the candidate; approval invalidated')
                if editable:
                    self.save_draft()
                final_path = self.run_dir / f'{stem}.final.json'
                if not final_path.exists():
                    final_path.write_text(result.output, encoding='utf-8')
                if not result.ok:
                    # Authentication, quotas, tool startup and invocation timeouts
                    # do not improve when we regenerate the whole implementation.
                    raise protocol.CrewError(f'{role} invocation stopped: {result.note or "unknown transport failure"}; candidate retained')
                value = parser(result.output)
                (self.run_dir / f'{stem}.validated.json').write_text(json.dumps(value, indent=2), encoding='utf-8')
                return value
            except protocol.ProtocolError as exc:
                error = str(exc)
                error_class = type(exc).__name__
                feedback = '\nYour files are retained. Correct only the response object: ' + error
            except BaseException as exc:
                error = str(exc)
                error_class = type(exc).__name__
                if editable:
                    # Retain partial work on timeouts/interrupts without granting approval.
                    self.save_draft()
                raise
            finally:
                self.metric(stage=kind, role=role, attempt=attempt, engine=engine.name,
                            seconds=time.monotonic() - start, process_seconds=result.seconds if result else None,
                            prompt_chars=len(request), metadata=result.metadata if result else {},
                            error=error, error_class=error_class,
                            decision=(value or {}).get('decision', (value or {}).get('action')), applied=False)
        raise protocol.CrewError(f'{role} response failed after {self.attempts} attempts: {feedback}')

    def validate(self, names, *, final=False):
        self.validation_passed = False
        self.assert_current()
        before = self.candidate.state()
        self.validations += 1
        paths = (self.acceptance_paths | self.changed | set(names)) if final else set(names)
        commands = validation.select_gates(self.gate_set, paths, final=final, root=self.candidate.root)
        statuses = {name: 'pending' for name, _ in commands}
        self.report('validation', restart=True, gates=statuses, detail='Final acceptance checks' if final else 'Checks for this change')
        def on_gate(name, status):
            statuses[name] = status
            self.report(gates=dict(statuses), detail=f'{name}: {status}')
        gates = validation.run_gates(self.gate_set, self.gate_timeout, self.run_dir, self.validations,
                                     cwd=self.candidate.root, commands=commands, deadline=self.deadline, on_gate=on_gate)
        self.assert_current()
        if before != self.candidate.state():
            raise protocol.CrewError('Validation changed candidate source; refusing to reuse its results')
        self.validated_candidate_state = before
        self.gate_text = validation.condense(gates) or '(validation explicitly disabled)'
        self.gate_text = self.gate_text.replace(str(self.run_dir) + '/', '@checks/')
        (self.run_dir / f'{self.validations:02d}-gates.summary.md').write_text(self.gate_text, encoding='utf-8')
        for gate in gates:
            self.metric(stage='gate', name=gate.name, seconds=gate.seconds, ok=gate.ok,
                        final=final, environment_problem=gate.environment_problem)
        if any(gate.environment_problem for gate in gates):
            raise protocol.CrewError('Validation environment unavailable; fix the dependency before resuming. Candidate retained.')
        self.validation_passed = all(g.ok for g in gates) and len(gates) == len(commands)
        return self.validation_passed

    def apply_reviewed(self, approval, patch, *, proposal_explanation=''):
        if self.pending is not approval or approval.patch != patch:
            raise protocol.CrewError('write refused: missing approval or patch differs from reviewed bytes')
        if not self.validation_passed:
            raise protocol.CrewError('write refused: candidate has no passing validation')
        self.assert_current()
        if (approval.state != self.expected_state or self.candidate.diff() != patch
                or self.validated_candidate_state != self.candidate.state()):
            raise protocol.CrewError('write refused: stale candidate approval')
        _, flags = patches.inspect_patch(patch, self.guide.rel)
        self.assert_current()
        self.pending = None
        self.report('apply', detail='Publishing the exact validated and reviewed diff')
        patches.checked_git('apply', *flags, '--whitespace=nowarn', input_text=patch)
        self.expected_state = self.state()
        self.history.append({'patch': patch, 'decision': approval.decision, 'explanation': approval.explanation,
                             'proposal_explanation': proposal_explanation, 'base_state': approval.state,
                             'resulting_state': self.expected_state})
        (self.run_dir / f'{len(self.history):03d}-applied.patch').write_text(patch, encoding='utf-8')
        (self.run_dir / 'applied-history.json').write_text(json.dumps(self.history, indent=2), encoding='utf-8')
        (self.run_dir / 'pending-proposal.json').unlink(missing_ok=True)
        self.candidate.checkpoint()
        self.metric(stage='publish', applied=True, patch_chars=len(patch))
        self.report(applied_edits=len(self.history), detail='Reviewed change published')

    def run(self, *, skip_implement=False):
        for engine in self.roles.values():
            engine.preflight()
        self.candidate = workspace.Candidate(self.run_dir, self.guide.rel)
        self.assert_current()
        self.candidate.preflight()
        self.metric(stage='preflight', ok=True, transport='scoped-file-tools', shell_enabled=False)
        feedback = ''
        if self.restored_proposal:
            patch = self.restored_proposal['patch']
            try:
                patches.patch_paths(patch, self.guide.rel)
                self.candidate.git('apply', '--recount', '--whitespace=nowarn', input_text=patch)
            except protocol.CrewError as exc:
                raise protocol.CrewError('Saved draft no longer applies; use --fresh-proposal to start from current files') from exc
            self.save_draft()
            feedback = 'An unapproved draft was restored into the candidate. Inspect and finish it; fresh checks and review are required.'
        if skip_implement:
            self.candidate.diff()
            self.validate(self.acceptance_paths, final=True)
            feedback += '\nCurrent checks:\n' + self.gate_text
        failures = review_failures = 0
        last_findings = None
        for number in range(1, self.max_edits + 1):
            self.remaining(self.agent_timeout)
            self.report('proposal', edit_number=number, edit_summary='Implement the next coherent candidate change')
            followup = prompts.followup_prompt(feedback, self.history)
            retains_context = bool(getattr(self.roles['implementer'], 'session_id', None))
            prompt = followup if number > 1 and retains_context else self.objective + '\n' + followup
            author = self.call('implementer', 'author', prompt, protocol.parse_author)
            patch = self.save_draft(author)
            names = patches.patch_paths(patch, self.guide.rel) if patch else []
            final = author['action'] == 'COMPLETE'
            if final:
                current = guides.parse_guide(self.candidate.root / self.guide.rel, self.guide.phase, self.guide.step)
                if self.sub.num not in current.recorded:
                    feedback = f'Add the concise Implementation record for {self.sub.id} before declaring COMPLETE.'
                    continue
            elif not patch:
                raise protocol.CrewError('Author made no change and did not complete the substep')
            self.report(files=names, edit_summary=author['explanation'])
            if not self.validate(names, final=final):
                failures += 1
                if failures >= self.max_rounds:
                    return 1
                feedback = 'Repair these actual check failures in the retained candidate:\n' + self.gate_text
                continue
            failures = 0
            review = self.call('reviewer', 'review', prompts.patch_review_prompt(
                self.review_objective, patch, author, self.gate_text, names, final=final), protocol.parse_review)
            self.report(review_summary=review['decision'] + ': ' + review['explanation'])
            if review['decision'] == 'CHANGES_REQUIRED':
                review_failures += 1
                findings = json.dumps(review['findings'], sort_keys=True)
                if review_failures >= self.attempts or findings == last_findings:
                    raise protocol.CrewError('Review findings remain unresolved; candidate retained without publication')
                last_findings = findings
                feedback = 'Fix these concrete findings in the retained candidate:\n' + findings
                continue
            review_failures, last_findings = 0, None
            self.remaining(self.agent_timeout)
            self.assert_current()
            if self.validated_candidate_state != self.candidate.state():
                raise protocol.CrewError('Candidate changed after validation; approval invalidated')
            self.changed.update(names)
            if patch:
                approval = patches.ReviewedPatch(patch, self.expected_state, 'APPROVED', review['explanation'])
                self.pending = approval
                self.apply_reviewed(approval, patch, proposal_explanation=author['explanation'])
            if final:
                self.assert_current()
                (self.run_dir / 'completion.json').write_text(json.dumps({
                    **author, 'review': review, 'validation': self.gate_text, 'state': self.expected_state,
                }, indent=2), encoding='utf-8')
                self.report('ready', status='ready', detail='Final criteria, review and checks satisfied',
                            edit_number=len(self.history), applied_edits=len(self.history))
                return 0
            feedback = 'The last change was checked, approved and published. Continue remaining requirements.\n' + self.gate_text
        raise protocol.CrewError('Edit budget exhausted; candidate retained for resume')
