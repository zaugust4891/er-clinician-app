"""The propose/review/apply/validate state machine and its guarded source-write boundary."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from . import guides, patches, prompts, protocol, validation


class PatchLoop:
    """CLI action dispatcher; apply_reviewed is the ONLY repository writer.

    Every invocation is read-only. Only a current, in-memory approval followed
    by the author's exact APPLY action can reach git apply. Responses/artifacts
    never serve as executable commands or reusable approval tickets.
    """

    def __init__(self, guide: guides.Guide, sub: guides.Substep, sha: str, pre: str,
                 roles: dict, run: Path, *, agent_timeout: int, gate_timeout: int,
                 gate_set: str, max_edits: int, max_rounds: int, attempts: int):
        self.guide, self.sub, self.roles, self.run_dir = guide, sub, roles, run
        self.objective = prompts.implement_prompt(guide, sub, sha, pre)
        self.review_objective = prompts.substep_context(guide, sub, sha, pre)
        self.agent_timeout, self.gate_timeout, self.gate_set = agent_timeout, gate_timeout, gate_set
        self.max_edits, self.max_rounds, self.attempts = max_edits, max_rounds, attempts
        self.context_paths = {guide.rel}
        self.history: list[dict] = []
        self.pending: patches.ReviewedPatch | None = None
        self.last_gates: list[validation.Gate] | None = None
        self.gate_text = '(validation not yet run)'
        self.failed_rounds = 0
        self.invocations = 0
        self.validations = 0
        self.needs_validation = False
        self.validated_state: str | None = None
        self.schemas = {}
        for name, schema in (
            ('proposal', protocol.PROPOSAL_SCHEMA), ('patch-review', protocol.PATCH_REVIEW_SCHEMA),
            ('application', protocol.APPLICATION_SCHEMA), ('context', protocol.CONTEXT_SCHEMA),
        ):
            destination = run / f'{name}.schema.json'
            destination.write_text(json.dumps(schema, indent=2), encoding='utf-8')
            self.schemas[name] = destination
        self.expected_state = self.state()

    def state(self) -> str:
        return patches.repository_state(self.context_paths)

    def assert_current(self, state: str) -> None:
        if self.state() != state:
            self.pending = None
            raise protocol.CrewError('stale patch/context: repository changed during an operation; approval invalidated, no further patch applied')

    def invoke(self, role: str, kind: str, prompt: str, state: str, parser):
        """Run one read-only model call, check its base state, and save evidence."""
        self.assert_current(state)
        self.invocations += 1
        stem = f'{self.invocations:03d}-{role}-{kind}'
        (self.run_dir / f'{stem}.prompt.md').write_text(prompt, encoding='utf-8')
        try:
            result = self.roles[role].run(
                prompt, self.agent_timeout, schema=self.schemas[kind],
                log_path=self.run_dir / f'{stem}.transcript.log',
            )
        except (OSError, subprocess.SubprocessError, TimeoutError) as exc:
            self.assert_current(state)
            raise protocol.ProtocolError(f'{role} invocation failed: {exc}') from exc
        self.assert_current(state)
        final_path = self.run_dir / f'{stem}.final.json'
        if not final_path.exists():
            final_path.write_text(result.output, encoding='utf-8')
        if not result.ok:
            raise protocol.ProtocolError(f'{role} invocation failed: {result.note or "unknown error"}')
        value = parser(result.output)
        (self.run_dir / f'{stem}.validated.json').write_text(
            json.dumps(value, indent=2, ensure_ascii=False), encoding='utf-8',
        )
        return value

    def call(self, role: str, kind: str, prompt: str, state: str, parser):
        """Retry protocol failures within a fixed limit; stale state stops immediately."""
        error = ''
        for attempt in range(1, self.attempts + 1):
            try:
                return self.invoke(role, kind, prompt + error, state, parser)
            except protocol.ProtocolError as exc:
                error = f'\nPrevious response failed: {exc}. Return a complete valid {kind} object.'
                print(f'  {role}/{kind} attempt {attempt}/{self.attempts}: {exc}')
        raise protocol.CrewError(f'{role}/{kind} failed after {self.attempts} attempts:{error}')

    def add_context_paths(self, names: list[str], state: str) -> str:
        self.assert_current(state)
        for name in names:
            patches.safe_path(name, self.guide.rel)
        self.assert_current(state)
        self.context_paths.update(names)
        self.expected_state = self.state()
        return self.expected_state

    def existing_code(self, names: list[str]) -> dict:
        existing = {}
        for name in sorted(set(names) | self.context_paths):
            path = patches.safe_path(name, self.guide.rel)
            if not path.exists():
                existing[name] = None
                continue
            if path.stat().st_size > 1_000_000:
                raise protocol.CrewError(f'context file exceeds 1 MiB: {name}; split the proposal')
            existing[name] = path.read_text(encoding='utf-8')
        return existing

    def review_and_apply(self, proposal: dict, state: str) -> None:
        """Resolve review/context/reproposal exchanges before dispatching an exact APPLY."""
        if self.needs_validation:
            raise protocol.CrewError('next edit refused: validate the previously applied patch first')
        error = ''
        for attempt in range(1, self.attempts + 1):
            self.assert_current(state)
            self.pending = None
            try:
                names = patches.patch_paths(proposal['patch'], self.guide.rel)
                prompt = prompts.patch_review_prompt(
                    self.review_objective, proposal, self.existing_code(names), self.history, self.gate_text,
                ) + error
                review = self.invoke(
                    'reviewer', 'patch-review', prompt, state,
                    lambda raw: protocol.parse_patch_review(raw, proposal['patch']),
                )
                if review['decision'] == 'NEEDS_CONTEXT':
                    requests = json.dumps(review['context_requests'], ensure_ascii=False)
                    def parse_context(raw):
                        value = protocol.parse_response(raw, protocol.CONTEXT_SCHEMA)
                        if not value['context'].strip():
                            raise protocol.ProtocolError('requested context must be non-empty')
                        return value
                    context = self.call(
                        'implementer', 'context',
                        self.objective + '\nCodex needs essential context BEFORE implementation. '
                        'Read the current repository and answer these requests with inspected evidence. '
                        'Return context and context_paths only.\n' + requests
                        + '\nCurrent proposal:\n' + json.dumps(proposal, ensure_ascii=False),
                        state, parse_context,
                    )
                    state = self.add_context_paths(context['context_paths'], state)
                    proposal = {**proposal, 'context': proposal['context'] + '\n' + context['context']}
                    error = ''
                    continue
                # Check the ENTIRE returned patch before issuing an application
                # request, including any files newly introduced by a revision.
                patches.patch_paths(review['patch'], self.guide.rel)
                approved = patches.ReviewedPatch(review['patch'], state, review['decision'], review['explanation'])
                self.pending = approved
                def parse_application(raw):
                    value = protocol.parse_response(raw, protocol.APPLICATION_SCHEMA)
                    if not value['patch'].strip():
                        raise protocol.ProtocolError('application requires a complete patch')
                    if value['action'] == 'APPLY' and value['patch'] != approved.patch:
                        raise protocol.ProtocolError('modified patch must use REPROPOSE and be reviewed again')
                    if value['action'] == 'REPROPOSE' and not value['explanation'].strip():
                        raise protocol.ProtocolError('REPROPOSE requires an explanation')
                    return value
                application = self.call(
                    'implementer', 'application', self.objective
                    + '\nCodex returned this reviewed patch. Request APPLY with the EXACT patch '
                    'string to apply it. If you need ANY modification, request REPROPOSE with a '
                    'complete replacement diff and explanation; it will undergo a fresh review.\n'
                    + json.dumps(review, ensure_ascii=False), state, parse_application,
                )
                if application['action'] == 'REPROPOSE':
                    self.pending = None
                    proposal = {**proposal, 'patch': application['patch'],
                                'explanation': application['explanation']}
                    error = ''
                    continue
                self.apply_reviewed(approved, application['patch'])
                return
            except protocol.ProtocolError as exc:
                self.pending = None
                error = f'\nPrevious review failed: {exc}. Return a complete valid decision.'
                print(f'  patch review attempt {attempt}/{self.attempts}: {exc}')
        self.pending = None
        raise protocol.CrewError(f'patch review exhausted {self.attempts} attempts/context exchanges; no patch applied.{error}')

    def apply_reviewed(self, approval: patches.ReviewedPatch, patch: str) -> None:
        """Consume the current approval and apply its exact bytes once."""
        if self.pending is not approval or patch != approval.patch:
            raise protocol.CrewError('write refused: missing approval or patch differs from reviewed bytes')
        if self.needs_validation:
            raise protocol.CrewError('write refused: validate the previously applied patch first')
        self.assert_current(approval.state)
        patches.patch_paths(patch, self.guide.rel)  # check again at the actual write boundary
        self.assert_current(approval.state)
        self.pending = None  # single use; failed application is never auto-retried
        patches.checked_git('apply', '--whitespace=nowarn', input_text=patch)
        self.expected_state = self.state()
        self.needs_validation = True
        applied = {'patch': patch, 'decision': approval.decision,
                   'explanation': approval.explanation, 'base_state': approval.state,
                   'resulting_state': self.expected_state}
        self.history.append(applied)
        (self.run_dir / f'{len(self.history):03d}-applied.patch').write_bytes(patch.encode('utf-8'))
        (self.run_dir / 'applied-history.json').write_text(
            json.dumps(self.history, indent=2, ensure_ascii=False), encoding='utf-8',
        )

    def validate(self) -> bool:
        """Run fixed gates on the current tree and bind their results to that state."""
        before = self.expected_state
        self.assert_current(before)
        self.validations += 1
        print(f'\n=== validation {self.validations}')
        self.last_gates = validation.run_gates(
            self.gate_set, self.gate_timeout, run_dir=self.run_dir, round_num=self.validations,
        )
        self.assert_current(before)  # fixed validation commands must not edit source
        self.gate_text = validation.condense(
            self.last_gates, self.roles['condenser'],
            helper_log_path=self.run_dir / f'{self.validations:02d}-gate-condense.transcript.log',
        ) or '(no gates configured; deterministic validation intentionally skipped)'
        self.assert_current(before)
        (self.run_dir / f'{self.validations:02d}-gates.summary.md').write_text(self.gate_text, encoding='utf-8')
        passed = all(gate.ok for gate in self.last_gates)
        self.failed_rounds = 0 if passed else self.failed_rounds + 1
        self.validated_state = before
        self.needs_validation = False
        return passed

    def run(self, *, skip_implement: bool = False) -> int:
        """Repeat coherent edits until completion evidence, guide record, and gates agree."""
        if skip_implement and not self.validate() and self.failed_rounds >= self.max_rounds:
            return 1
        # The extra proposal gives Claude a chance to declare completion after
        # the last permitted edit; no final edit can escape validation.
        for _ in range(self.max_edits + 1):
            state = self.expected_state
            def parse_current_proposal(raw):
                value = protocol.parse_proposal(raw)
                if value['action'] == 'PATCH':
                    patches.patch_paths(value['patch'], self.guide.rel)
                return value
            proposal = self.call(
                'implementer', 'proposal', self.objective
                + '\nEarlier applied changes and current validation:\n'
                + json.dumps({'earlier_applied_changes': self.history,
                              'last_validation': self.gate_text,
                              'current_state': state,
                              'working_tree_diff': patches.checked_git(
                                  'diff', '--no-ext-diff', '--no-textconv', 'HEAD',
                              ).decode('utf-8')}, ensure_ascii=False),
                state, parse_current_proposal,
            )
            state = self.add_context_paths(proposal['context_paths'], state)
            if proposal['action'] == 'COMPLETE':
                current_guide = guides.parse_guide(self.guide.path, self.guide.phase, self.guide.step)
                if self.sub.num not in current_guide.recorded:
                    raise protocol.CrewError(f'completion refused: missing Implementation record for {self.sub.id}')
                if self.validated_state != state:
                    if not self.validate() and self.failed_rounds >= self.max_rounds:
                        return 1
                    continue  # require fresh completion evidence after validation
                if not all(gate.ok for gate in self.last_gates):
                    if self.failed_rounds >= self.max_rounds:
                        return 1
                    continue
                self.assert_current(state)
                (self.run_dir / 'completion.json').write_text(
                    json.dumps(proposal, indent=2, ensure_ascii=False), encoding='utf-8',
                )
                print(f'\n=== {self.sub.id} READY after {len(self.history)} reviewed edits. Nothing was committed.')
                return 0
            if len(self.history) >= self.max_edits:
                raise protocol.CrewError(f'edit limit ({self.max_edits}) reached before substep completion')
            self.review_and_apply(proposal, state)
            if not self.validate() and self.failed_rounds >= self.max_rounds:
                print(f'\n=== {self.sub.id} NOT READY: {self.max_rounds} consecutive failed validations')
                return 1
        raise protocol.CrewError('proposal limit reached before substep completion')
