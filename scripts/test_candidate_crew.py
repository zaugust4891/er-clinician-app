"""Offline regression tests for candidate isolation, review and publication."""
from __future__ import annotations
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

if __package__:
    from .substep_crew_lib import checkpoints, cli, config, engines, file_tools, guides, orchestration, patches, processes, prompts, protocol, validation, workspace
else:
    from substep_crew_lib import checkpoints, cli, config, engines, file_tools, guides, orchestration, patches, processes, prompts, protocol, validation, workspace


def author(action='COMPLETE'):
    return {'action': action, 'explanation': 'Implement value handling', 'completion_evidence':
            [{'criterion': 'Value handling', 'evidence': 'Current a.py handles the value'}] if action == 'COMPLETE' else []}


def review(decision='APPROVED'):
    return {'decision': decision, 'explanation': 'Checked value behavior', 'findings':
            [{'path': 'a.py', 'problem': 'Use value 2'}] if decision != 'APPROVED' else []}


class FakeEngine(engines.Engine):
    name = 'fake'
    def __init__(self, outputs):
        self.outputs = list(outputs)
        self.calls = []
    def run(self, prompt, timeout, schema=None, *, log_path=None, workspace=None, editable=False):
        self.calls.append((prompt, editable, timeout))
        if not self.outputs:
            raise AssertionError('Unexpected model call')
        value = self.outputs.pop(0)
        if callable(value):
            value = value(workspace)
        if isinstance(value, BaseException):
            raise value
        if isinstance(value, processes.Result):
            return value
        return processes.Result(True, value if isinstance(value, str) else json.dumps(value), .01)


class CandidateTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / 'guide.md').write_text('## S1 — One\nImplement value handling.\n## Implementation record — S1\nBehavior and evidence.\n')
        (self.root / 'a.py').write_text('value = 0\n')
        (self.root / '.gitignore').write_text('.agents/\ntarget/\n.env\nignored.txt\n')
        self.git('init', '-q')
        self.git('add', '.')
        self.git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                 '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'phase 1 step 1 s0')
        for key, value in [('REPO', self.root), ('RUNS', self.root / '.agents/runs')]:
            patcher = mock.patch.object(config, key, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.output = contextlib.redirect_stdout(io.StringIO())
        self.output.__enter__()
        self.addCleanup(self.output.__exit__, None, None, None)
        self.run_dir = config.RUNS / 'test-run'
        self.run_dir.mkdir(parents=True)
        self.guide = guides.parse_guide(self.root / 'guide.md', 1, 1)
        self.gate_candidates = []

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, capture_output=True, check=True).stdout

    def edit(self, value, response=None):
        def work(candidate):
            file_tools.FileTools(candidate.root, editable=True, journal=candidate.journal).call(
                'edit_file', {'path': 'a.py', 'old': (candidate.root / 'a.py').read_text(), 'new': f'value = {value}\n'})
            return response if response is not None else author()
        return work

    def turn_limited(self, value):
        def work(candidate):
            self.author_engine.session_id = 'retained-session'
            self.author_engine.session_root = str(candidate.root)
            self.edit(value)(candidate)
            return processes.Result(False, '', .01, 'Reached maximum number of turns',
                                    metadata={'subtype': 'error_max_turns', 'session_id': 'retained-session'})
        return work

    def loop(self, authors, reviews, *, gates=(), **kwargs):
        self.author_engine, self.reviewer_engine = FakeEngine(authors), FakeEngine(reviews)
        outcomes = iter(gates)
        def run_gates(names, timeout, run_dir, round_num, **options):
            self.gate_candidates.append((options['cwd'] / 'a.py').read_text())
            ok = next(outcomes, True)
            return [validation.Gate(name, ok, 'ok' if ok else 'error: incorrect behavior', .01)
                    for name, _ in options['commands']]
        patcher = mock.patch.object(validation, 'run_gates', side_effect=run_gates)
        patcher.start()
        self.addCleanup(patcher.stop)
        params = dict(agent_timeout=10, gate_timeout=10, gate_set='auto', max_edits=4, max_rounds=3, attempts=3)
        params.update(kwargs)
        return orchestration.PatchLoop(self.guide, self.guide.get('S1'), self.git('rev-parse', 'HEAD').decode().strip(), '',
                                      {'implementer': self.author_engine, 'reviewer': self.reviewer_engine}, self.run_dir, **params)

    def test_one_edit_needs_only_author_and_review_and_preserves_dirty_files(self):
        (self.root / 'a.py').write_text('value = 7\n')
        (self.root / 'user.txt').write_text('untracked user work\n')
        (self.root / '.env').write_text('DO_NOT_COPY=secret\n')
        initial_index = self.git('ls-files', '--stage')
        def inspect(candidate):
            self.assertEqual((self.root / 'a.py').read_text(), 'value = 7\n')
            self.assertEqual((candidate.root / 'a.py').read_text(), 'value = 2\n')
            self.assertEqual((candidate.root / 'user.txt').read_text(), 'untracked user work\n')
            self.assertFalse((candidate.root / '.env').exists())
            self.assertEqual(self.gate_candidates, ['value = 2\n'])
            return review()
        loop = self.loop([self.edit(2)], [inspect])
        self.assertEqual(loop.run(), 0)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')
        self.assertEqual((self.root / 'user.txt').read_text(), 'untracked user work\n')
        self.assertEqual(self.git('ls-files', '--stage'), initial_index)
        self.assertEqual(len(self.author_engine.calls) + len(self.reviewer_engine.calls), 2)
        self.assertFalse((self.run_dir / 'pending-proposal.json').exists())
        metrics = [json.loads(line) for line in (self.run_dir / 'metrics.jsonl').read_text().splitlines()]
        self.assertEqual(sum(item.get('applied', False) for item in metrics), 1)
        self.assertTrue((self.run_dir / 'completion.json').exists())

    def test_new_deleted_and_untracked_modified_files_have_exact_git_diffs(self):
        (self.root / 'user.txt').write_text('user\n')
        def change(candidate):
            tools = file_tools.FileTools(candidate.root, editable=True)
            tools.call('delete_file', {'path': 'a.py'})
            tools.call('edit_file', {'path': 'user.txt', 'old': 'user\n', 'new': 'user plus integration\n'})
            tools.call('write_file', {'path': 'new.txt', 'content': 'new content\n'})
            return author()
        loop = self.loop([change], [review()])
        with mock.patch.object(validation, 'run_gates', return_value=[validation.Gate('diff-check', True, '', .01)]), mock.patch.object(
            validation, 'select_gates', return_value=[('diff-check', ['git', 'diff', '--check'])]
        ):
            self.assertEqual(loop.run(), 0)
        self.assertFalse((self.root / 'a.py').exists())
        self.assertEqual((self.root / 'user.txt').read_text(), 'user plus integration\n')
        self.assertEqual((self.root / 'new.txt').read_text(), 'new content\n')

    def test_review_findings_return_to_author_without_publishing_bad_candidate(self):
        def fix(candidate):
            self.assertEqual((candidate.root / 'a.py').read_text(), 'value = 1\n')
            self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
            return self.edit(2)(candidate)
        loop = self.loop([self.edit(1), fix], [review('CHANGES_REQUIRED'), review()])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(loop.history), 1)
        self.assertIn('Use value 2', self.author_engine.calls[1][0])
        self.assertIn('Use value 2', self.reviewer_engine.calls[1][0])
        self.assertEqual(self.gate_candidates, ['value = 1\n', 'value = 2\n'])

    def test_three_rejections_with_new_findings_can_be_fixed_before_publication(self):
        findings = [dict(review('CHANGES_REQUIRED'), findings=[{'path': 'a.py', 'problem': f'Use value {n}'}])
                    for n in (2, 3, 4)]
        def approve(candidate):
            self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
            self.assertEqual((candidate.root / 'a.py').read_text(), 'value = 4\n')
            return review()
        loop = self.loop([self.edit(n) for n in (1, 2, 3, 4)], [*findings, approve])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(loop.history), 1)
        self.assertEqual(self.gate_candidates, [f'value = {n}\n' for n in (1, 2, 3, 4)])
        for n in (2, 3, 4):
            self.assertIn(f'Use value {n}', self.author_engine.calls[n - 1][0])
        self.assertIsNone(checkpoints.recover_review(self.run_dir))

    def test_identical_findings_do_not_stop_a_changed_candidate(self):
        loop = self.loop([self.edit(n) for n in (1, 3, 2)],
                         [review('CHANGES_REQUIRED'), review('CHANGES_REQUIRED'), review()], attempts=1)
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.reviewer_engine.calls), 3)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')

    def test_unchanged_rejected_candidate_stops_before_repeating_gates_and_review(self):
        loop = self.loop([self.edit(1), author()], [review('CHANGES_REQUIRED')])
        with self.assertRaisesRegex(protocol.CrewError, 'previously rejected candidate'):
            loop.run()
        self.assertEqual(self.gate_candidates, ['value = 1\n'])
        self.assertEqual(len(self.reviewer_engine.calls), 1)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
        self.assertEqual(checkpoints.recover_review(self.run_dir), review('CHANGES_REQUIRED'))

    def test_review_repairs_still_obey_edit_and_run_budgets(self):
        for budget in ('edits', 'time'):
            with self.subTest(budget=budget):
                self.run_dir = config.RUNS / budget
                self.run_dir.mkdir()
                def reject(candidate):
                    if budget == 'time':
                        loop.deadline = 0
                    return review('CHANGES_REQUIRED')
                loop = self.loop([self.edit(1)], [reject], max_edits=1 if budget == 'edits' else 4)
                with self.assertRaisesRegex(protocol.CrewError, 'Edit budget|time budget'):
                    loop.run()
                self.assertEqual(len(self.author_engine.calls), 1)
                self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
                self.assertEqual(checkpoints.recover_review(self.run_dir), review('CHANGES_REQUIRED'))

    def test_gate_failures_are_fixed_before_review(self):
        loop = self.loop([self.edit(1), self.edit(2)], [review()], gates=[False, True])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.reviewer_engine.calls), 1)
        self.assertIn('incorrect behavior', self.author_engine.calls[1][0])

    def test_exhausted_validation_leaves_draft_without_source_writes(self):
        loop = self.loop([self.edit(1)], [], gates=[False], max_rounds=1)
        self.assertEqual(loop.run(), 1)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
        self.assertTrue((self.run_dir / 'pending-proposal.json').exists())

    def test_environment_failure_stops_without_asking_for_code_changes(self):
        loop = self.loop([self.edit(1)], [])
        with mock.patch.object(validation, 'run_gates', return_value=[validation.Gate('test', False, '', .01, environment_problem=True)]):
            with self.assertRaisesRegex(protocol.CrewError, 'environment unavailable'):
                loop.run()
        self.assertEqual(len(self.author_engine.calls), 1)

    def test_source_change_during_review_invalidates_publication(self):
        def mutate(candidate):
            (self.root / 'a.py').write_text('concurrent user edit\n')
            return review()
        loop = self.loop([self.edit(2)], [mutate])
        with self.assertRaisesRegex(protocol.CrewError, 'stale candidate'):
            loop.run()
        self.assertEqual((self.root / 'a.py').read_text(), 'concurrent user edit\n')
        self.assertFalse((self.run_dir / 'completion.json').exists())

    def test_reviewer_cannot_mutate_candidate_and_still_approve(self):
        def mutate(candidate):
            (candidate.root / 'a.py').write_text('unexpected\n')
            return review()
        loop = self.loop([self.edit(2)], [mutate])
        with self.assertRaisesRegex(protocol.CrewError, 'Reviewer modified'):
            loop.run()
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_validation_cannot_mutate_candidate(self):
        loop = self.loop([self.edit(2)], [])
        def mutate(*args, **kwargs):
            (kwargs['cwd'] / 'a.py').write_text('unexpected\n')
            return []
        with mock.patch.object(validation, 'run_gates', side_effect=mutate):
            with self.assertRaisesRegex(protocol.CrewError, 'Validation changed'):
                loop.run()

    def test_transport_failure_stops_once_and_retains_partial_edits(self):
        response = processes.Result(False, '', .1, 'usage limit reached')
        loop = self.loop([self.edit(2, response)], [])
        with self.assertRaisesRegex(protocol.CrewError, 'usage limit'):
            loop.run()
        self.assertEqual(len(self.author_engine.calls), 1)
        draft = checkpoints.recover_proposal(self.run_dir, 'guide.md')
        self.assertIsNotNone(draft)
        self.assertIn('+value = 2', draft[0]['patch'])

    def test_turn_limit_continues_same_candidate_then_requires_checks_and_review(self):
        def finish(candidate):
            self.assertEqual((candidate.root / 'a.py').read_text(), 'value = 2\n')
            self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
            self.assertEqual(self.author_engine.session_id, 'retained-session')
            return author()
        loop = self.loop([self.turn_limited(2), finish], [review()])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.author_engine.calls), 2)
        continuation = self.author_engine.calls[1][0]
        self.assertIn('same session and candidate files are retained', continuation)
        self.assertNotIn('Original objective and completion criteria', continuation)
        self.assertEqual(self.gate_candidates, ['value = 2\n'])
        self.assertEqual(len(self.reviewer_engine.calls), 1)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')
        metrics = [json.loads(line) for line in (self.run_dir / 'metrics.jsonl').read_text().splitlines()]
        self.assertTrue(any(row.get('error_class') == 'TurnLimitContinuation' for row in metrics))

    def test_turn_limit_continuations_stop_at_the_existing_attempt_budget(self):
        loop = self.loop([self.turn_limited(n) for n in (1, 2, 3)], [])
        with self.assertRaisesRegex(protocol.CrewError, 'maximum number of turns'):
            loop.run()
        self.assertEqual(len(self.author_engine.calls), 3)
        self.assertEqual(self.reviewer_engine.calls, [])
        self.assertEqual(self.gate_candidates, [])
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
        draft, _ = checkpoints.recover_proposal(self.run_dir, 'guide.md')
        self.assertIn('+value = 3', draft['patch'])

    def test_turn_limit_without_progress_or_matching_session_does_not_continue(self):
        for mode in ('no-progress', 'no-session', 'wrong-session', 'wrong-root'):
            with self.subTest(mode=mode):
                self.run_dir = config.RUNS / mode
                self.run_dir.mkdir()
                def limited(candidate):
                    self.author_engine.session_id = 'retained-session'
                    self.author_engine.session_root = str(candidate.root)
                    if mode != 'no-progress':
                        self.edit(2)(candidate)
                    if mode == 'no-session':
                        self.author_engine.session_id = None
                    if mode == 'wrong-root':
                        self.author_engine.session_root = '/another/candidate'
                    return processes.Result(False, '', .01, 'error_max_turns', metadata={
                        'subtype': 'error_max_turns',
                        'session_id': 'different-session' if mode == 'wrong-session' else 'retained-session',
                    })
                loop = self.loop([limited], [])
                with self.assertRaisesRegex(protocol.CrewError, 'error_max_turns'):
                    loop.run()
                self.assertEqual(len(self.author_engine.calls), 1)
                self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_source_changes_and_deadline_expiry_cannot_trigger_turn_limit_continuation(self):
        for mode in ('stale', 'deadline'):
            with self.subTest(mode=mode):
                self.run_dir = config.RUNS / mode
                self.run_dir.mkdir()
                def limited(candidate):
                    result = self.turn_limited(2)(candidate)
                    if mode == 'stale':
                        (self.root / 'external.txt').write_text('user changed source\n')
                    else:
                        loop.deadline = 0
                    return result
                loop = self.loop([limited], [])
                with self.assertRaisesRegex(protocol.CrewError, 'stale candidate|time budget'):
                    loop.run()
                self.assertEqual(len(self.author_engine.calls), 1)
                self.assertEqual(self.reviewer_engine.calls, [])
                self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_reviewer_turn_limits_do_not_create_author_continuations(self):
        failure = processes.Result(False, '', .01, 'error_max_turns',
                                   metadata={'subtype': 'error_max_turns', 'session_id': 'review-session'})
        loop = self.loop([self.edit(2)], [failure])
        with self.assertRaisesRegex(protocol.CrewError, 'reviewer invocation stopped'):
            loop.run()
        self.assertEqual(len(self.author_engine.calls), 1)
        self.assertEqual(len(self.reviewer_engine.calls), 1)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_malformed_review_retries_same_candidate_without_author_call(self):
        loop = self.loop([self.edit(2)], ['not json', review()])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.author_engine.calls), 1)
        self.assertIn('Correct only the response object', self.reviewer_engine.calls[1][0])

    def test_resume_recovers_unapproved_draft_but_rechecks_and_reviews(self):
        first = self.loop([self.edit(2)], [], gates=[False], max_rounds=1)
        self.assertEqual(first.run(), 1)
        draft, _ = checkpoints.recover_proposal(self.run_dir, 'guide.md')
        self.run_dir = config.RUNS / 'resume'
        self.run_dir.mkdir()
        def inspect(candidate):
            self.assertEqual((candidate.root / 'a.py').read_text(), 'value = 2\n')
            return author()
        resumed = self.loop([inspect], [review()], restored_proposal=draft)
        self.assertEqual(resumed.run(), 0)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')

    def test_completion_without_changes_still_requires_gates_and_review(self):
        loop = self.loop([author()], [review()])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(loop.history), 0)
        self.assertEqual(len(self.gate_candidates), 1)
        self.assertEqual(len(self.reviewer_engine.calls), 1)

    def test_missing_record_cannot_complete(self):
        (self.root / 'guide.md').write_text('## S1 — One\nImplement value handling.\n')
        loop = self.loop([self.edit(2)], [], max_edits=1)
        with self.assertRaisesRegex(protocol.CrewError, 'Edit budget'):
            loop.run()
        self.assertFalse((self.run_dir / 'completion.json').exists())

    def test_write_boundary_rejects_missing_changed_and_reused_receipts(self):
        loop = self.loop([self.edit(2)], [review()])
        self.assertEqual(loop.run(), 0)
        patch = loop.history[0]['patch']
        receipt = patches.ReviewedPatch(patch, loop.expected_state, 'APPROVED', 'old')
        with self.assertRaisesRegex(protocol.CrewError, 'missing approval'):
            loop.apply_reviewed(receipt, patch)
        loop.pending = receipt
        with self.assertRaisesRegex(protocol.CrewError, 'differs'):
            loop.apply_reviewed(receipt, patch + '\n')
        with self.assertRaisesRegex(protocol.CrewError, 'stale candidate approval'):
            loop.apply_reviewed(receipt, patch)

    def test_write_boundary_requires_passing_validation_even_with_a_receipt(self):
        loop = self.loop([self.edit(2)], [], gates=[False], max_rounds=1)
        self.assertEqual(loop.run(), 1)
        patch = loop.candidate.diff()
        receipt = patches.ReviewedPatch(patch, loop.expected_state, 'APPROVED', 'forged')
        loop.pending = receipt
        with self.assertRaisesRegex(protocol.CrewError, 'no passing validation'):
            loop.apply_reviewed(receipt, patch)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_ignored_and_binary_candidate_edits_never_reach_publication(self):
        for name, content in [('ignored.txt', b'ignored\n'), ('binary.dat', b'\x00binary')]:
            with self.subTest(name=name):
                self.run_dir = config.RUNS / name
                self.run_dir.mkdir()
                def change(candidate):
                    (candidate.root / name).write_bytes(content)
                    return author()
                loop = self.loop([change], [])
                with self.assertRaises(protocol.CrewError):
                    loop.run()
                self.assertFalse((self.root / name).exists())

    def test_existing_deletion_stays_deleted_and_git_environment_cannot_redirect_snapshot(self):
        (self.root / 'a.py').unlink()
        index_before = (self.root / '.git/index').read_bytes()
        with mock.patch.dict(os.environ, {'GIT_DIR': str(self.root / '.git'), 'GIT_WORK_TREE': str(self.root)}):
            candidate = workspace.Candidate(self.run_dir, 'guide.md')
            self.assertFalse((candidate.root / 'a.py').exists())
            self.assertEqual(candidate.diff(), '')
        self.assertEqual((self.root / '.git/index').read_bytes(), index_before)

    def test_resume_target_and_active_run_checks_are_preserved(self):
        (self.run_dir / 'run.json').write_text(json.dumps({'phase': 1, 'step': 1, 'substep': 'S1'}))
        with self.assertRaisesRegex(protocol.CrewError, 'does not match'):
            checkpoints.load(str(self.run_dir), 1, 1, 'S2')
        (self.run_dir / 'progress.json').write_text(json.dumps({'status': 'running', 'pid': os.getpid()}))
        with self.assertRaisesRegex(protocol.CrewError, 'still active'):
            checkpoints.load(str(self.run_dir), 1, 1, 'S1')

    def test_deadline_stops_before_model_calls(self):
        loop = self.loop([], [], run_timeout=-1)
        with self.assertRaisesRegex(protocol.CrewError, 'time budget'):
            loop.run()
        self.assertEqual(self.author_engine.calls, [])

    def test_candidate_transport_preflight_is_real_and_bounded(self):
        candidate = workspace.Candidate(self.run_dir, 'guide.md')
        candidate.preflight()
        self.assertIn('Implement value', file_tools.FileTools(candidate.root).call('read_file', {'path': 'guide.md'})['text'])

    def test_cli_dry_run_creates_no_workspace_or_model_call(self):
        with mock.patch.object(guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())), mock.patch.object(
            sys, 'argv', ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1', '--dry-run']
        ), mock.patch.object(engines.ClaudeEngine, 'run', side_effect=AssertionError('model invoked')):
            self.assertEqual(cli.main(), 0)
        self.assertFalse((self.run_dir / 'candidate').exists())

    def test_cli_author_turn_limit_override_is_visible_without_invoking_a_model(self):
        engine = engines.ClaudeEngine()
        roles = {'implementer': lambda: engine, 'reviewer': lambda: FakeEngine([])}
        with mock.patch.object(guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())), mock.patch.object(
            engines, 'PROFILES', {'default': roles}
        ), mock.patch.object(sys, 'argv', ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1',
                                          '--author-max-turns', '90', '--dry-run']), mock.patch.object(
            engine, 'run', side_effect=AssertionError('model invoked')
        ):
            self.assertEqual(cli.main(), 0)
        self.assertEqual(engine.max_turns, 90)
        self.assertFalse((self.run_dir / 'candidate').exists())

    def test_cli_rejects_nonpositive_author_turn_limits(self):
        with mock.patch.object(sys, 'argv', ['substep_crew.py', '--author-max-turns', '0']), contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                cli.main()
        self.assertEqual(error.exception.code, 2)

    def test_resume_latest_uses_unfinished_run_even_when_guide_has_record(self):
        (self.run_dir / 'run.json').write_text(json.dumps({'phase': 1, 'step': 1, 'substep': 'S1'}))
        with mock.patch.object(guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())), mock.patch.object(
            sys, 'argv', ['substep_crew.py', '--resume', '--dry-run']
        ), mock.patch.object(guides, 'resolve_substep', wraps=guides.resolve_substep) as resolve:
            self.assertEqual(cli.main(), 0)
        self.assertEqual(resolve.call_args.args[1], 'S1')

    def test_cli_end_to_end_uses_new_protocol_and_records_provenance(self):
        self.loop([], [])  # mock deterministic gates
        roles = {'implementer': lambda: FakeEngine([self.edit(2)]), 'reviewer': lambda: FakeEngine([review()])}
        with mock.patch.object(engines, 'PROFILES', {'default': roles}), mock.patch.object(
            guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())
        ), mock.patch.object(sys, 'argv', ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1']):
            self.assertEqual(cli.main(), 0)
        completed = list(config.RUNS.glob('*/completion.json'))
        self.assertEqual(len(completed), 1)
        run = completed[0].parent
        self.assertEqual(json.loads((run / 'run.json').read_text())['workflow'], 'candidate-v1')
        self.assertTrue((run / 'events.jsonl').exists())

    def test_cli_resume_checks_restored_candidate_once_before_publication(self):
        first = self.loop([self.edit(2)], [], gates=[False], max_rounds=1)
        self.assertEqual(first.run(), 1)
        (self.run_dir / 'run.json').write_text(json.dumps({'phase': 1, 'step': 1, 'substep': 'S1'}))
        self.gate_candidates.clear()
        def inspect(candidate):
            self.assertEqual((candidate.root / 'a.py').read_text(), 'value = 2\n')
            self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
            return author()
        roles = {'implementer': lambda: FakeEngine([inspect]), 'reviewer': lambda: FakeEngine([review()])}
        with mock.patch.object(engines, 'PROFILES', {'default': roles}), mock.patch.object(
            guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())
        ), mock.patch.object(sys, 'argv', ['substep_crew.py', '--resume', str(self.run_dir)]):
            self.assertEqual(cli.main(), 0)
        self.assertEqual(self.gate_candidates, ['value = 2\n'])
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')

    def test_cli_resume_restores_legacy_review_findings_before_author(self):
        first = self.loop([self.edit(1)], [review('CHANGES_REQUIRED')], max_edits=1)
        with self.assertRaisesRegex(protocol.CrewError, 'Edit budget'):
            first.run()
        # Simulate a failed run from before review feedback was saved in drafts.
        pending = self.run_dir / 'pending-proposal.json'
        draft = json.loads(pending.read_text())
        del draft['review']
        pending.write_text(json.dumps(draft))
        (self.run_dir / 'run.json').write_text(json.dumps({'phase': 1, 'step': 1, 'substep': 'S1'}))
        self.gate_candidates.clear()
        author_engine = FakeEngine([self.edit(2)])
        reviewer_engine = FakeEngine([review()])
        roles = {'implementer': lambda: author_engine, 'reviewer': lambda: reviewer_engine}
        with mock.patch.object(engines, 'PROFILES', {'default': roles}), mock.patch.object(
            guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())
        ), mock.patch.object(sys, 'argv', ['substep_crew.py', '--resume', str(self.run_dir)]):
            self.assertEqual(cli.main(), 0)
        self.assertIn('Use value 2', author_engine.calls[0][0])
        self.assertIn('Use value 2', reviewer_engine.calls[0][0])
        self.assertEqual(self.gate_candidates, ['value = 2\n'])
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')

    def test_review_feedback_survives_interrupted_resume_before_another_review(self):
        first = self.loop([self.edit(1)], [review('CHANGES_REQUIRED')], max_edits=1)
        with self.assertRaisesRegex(protocol.CrewError, 'Edit budget'):
            first.run()
        draft, _ = checkpoints.recover_proposal(self.run_dir, 'guide.md')
        feedback = checkpoints.recover_review(self.run_dir)
        self.run_dir = config.RUNS / 'resume-interrupted'
        self.run_dir.mkdir()
        failure = processes.Result(False, '', .01, 'provider unavailable')
        resumed = self.loop([self.edit(3, failure)], [], restored_proposal=draft, restored_review=feedback)
        with self.assertRaisesRegex(protocol.CrewError, 'provider unavailable'):
            resumed.run()
        self.assertEqual(checkpoints.recover_review(self.run_dir), feedback)
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')
        retained, _ = checkpoints.recover_proposal(self.run_dir, 'guide.md')
        self.assertIn('+value = 3', retained['patch'])

    def test_review_recovery_never_reuses_approvals_or_obsolete_findings(self):
        old = self.run_dir / '001-reviewer-review.validated.json'
        old.write_text(json.dumps(review('CHANGES_REQUIRED')))
        os.utime(old, ns=(1_000_000_000, 1_000_000_000))
        latest = self.run_dir / '002-reviewer-review.validated.json'
        for content in (json.dumps(review()), '{invalid json'):
            latest.write_text(content)
            self.assertIsNone(checkpoints.recover_review(self.run_dir))
        latest.unlink()
        (self.run_dir / 'applied-history.json').write_text('[]')
        self.assertIsNone(checkpoints.recover_review(self.run_dir))


class FileToolTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name) / 'candidate'
        self.root.mkdir()
        self.tools = file_tools.FileTools(self.root, editable=True)

    def test_paths_and_symlinks_cannot_escape_or_modify_control_files(self):
        outside = self.root.parent / 'outside'
        outside.write_text('keep')
        (self.root / 'link').symlink_to(outside)
        (self.root / 'dirlink').symlink_to(self.root.parent, target_is_directory=True)
        for name in ('../outside', str(outside), 'link', 'dirlink/outside', '.git/config', '.agents/state', '.env', 'x/../../outside'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.tools.call('write_file', {'path': name, 'content': 'bad'})
        self.assertEqual(outside.read_text(), 'keep')

    def test_read_only_server_exposes_no_mutators_and_checks_calls(self):
        server = file_tools.FileTools(self.root)
        self.assertEqual({t['name'] for t in server.definitions()}, {'read_file', 'list_files', 'search'})
        with self.assertRaises(ValueError):
            server.call('write_file', {'path': 'a.py', 'content': 'bad'})

    def test_ambiguous_edits_fail_and_successful_edits_preserve_crlf(self):
        path = self.root / 'a.py'
        path.write_bytes(b'a\r\na\r\n')
        with self.assertRaisesRegex(ValueError, 'exactly once'):
            self.tools.call('edit_file', {'path': 'a.py', 'old': 'a', 'new': 'b'})
        self.tools.call('edit_file', {'path': 'a.py', 'old': 'a\r\na', 'new': 'b\r\na'})
        self.assertEqual(path.read_bytes(), b'b\r\na\r\n')

    def test_stdio_mcp_round_trip_and_tool_errors(self):
        requests = [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {}},
            {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'},
            {'jsonrpc': '2.0', 'id': 3, 'method': 'tools/call', 'params': {'name': 'read_file', 'arguments': {'path': '../outside'}}},
        ]
        output = io.StringIO()
        file_tools.serve(self.tools, io.StringIO('\n'.join(json.dumps(r) for r in requests)), output)
        replies = [json.loads(x) for x in output.getvalue().splitlines()]
        self.assertEqual(len(replies), 3)
        self.assertTrue(replies[-1]['result']['isError'])

    def test_read_pagination_and_search_are_bounded(self):
        (self.root / 'large.txt').write_text('needle here\n' * 500)
        response = self.tools.call('read_file', {'path': 'large.txt', 'start_line': 10, 'line_count': 20})
        self.assertEqual(response['next_line'], 30)
        self.assertTrue(self.tools.call('search', {'query': 'needle'})['truncated'])
        with self.assertRaises(ValueError):
            self.tools.call('read_file', {'path': 'large.txt', 'line_count': 500})

    def test_gate_artifacts_are_readable_but_cannot_escape_or_be_written(self):
        name = '01-gate-01-test.transcript.log'
        (self.root.parent / name).write_text('error: actual diagnostic\n')
        server = file_tools.FileTools(self.root, editable=True, logs=self.root.parent)
        value = server.call('read_file', {'path': '@checks/' + name})
        self.assertIn('actual diagnostic', value['text'])
        for path in ('@checks/../outside', '@checks/run.json'):
            with self.assertRaises(ValueError):
                server.call('read_file', {'path': path})
        with self.assertRaises(ValueError):
            server.call('write_file', {'path': '@checks/' + name, 'content': 'pass'})

    def test_large_gate_logs_are_read_incrementally_with_bounded_lines(self):
        name = '01-gate-01-test.transcript.log'
        path = self.root.parent / name
        path.write_text('x' * 2_000_000 + '\nerror: the actual failure\n')
        server = file_tools.FileTools(self.root, logs=self.root.parent)
        first = server.call('read_file', {'path': '@checks/' + name, 'line_count': 1})
        self.assertLess(len(first['text']), file_tools.MAX_REPLY)
        self.assertEqual(first['next_line'], 2)
        second = server.call('read_file', {'path': '@checks/' + name, 'start_line': 2})
        self.assertIn('actual failure', second['text'])
        self.assertIsNone(second['next_line'])


class PolicyTests(unittest.TestCase):
    def test_final_gates_include_terraform_even_when_rust_is_requested(self):
        commands = validation.select_gates('rust', ['infra/modules/identity/main.tf'], final=True, root=Path('.'))
        names = {name for name, _ in commands}
        self.assertTrue({'fmt', 'test', 'tf-data-identity'} <= names)

    def test_docs_only_intermediate_checks_avoid_compilation(self):
        commands = validation.select_gates('rust', ['docs/readme.md'], final=False, root=Path('.'))
        self.assertEqual([name for name, _ in commands], ['diff-check'])
        self.assertEqual(validation.select_gates('none', ['Cargo.toml'], final=True, root=Path('.')), [])

    def test_intermediate_rust_is_scoped_but_final_is_workspace(self):
        paths = ['crates/er-server/src/config.rs']
        intermediate = dict(validation.select_gates('auto', paths, final=False, root=config.REPO))
        final = dict(validation.select_gates('auto', paths, final=True, root=config.REPO))
        self.assertIn('er-server', intermediate['test'])
        self.assertNotIn('--workspace', intermediate['test'])
        self.assertIn('--workspace', final['test'])

    def test_strict_protocol_rejects_false_approvals_and_incomplete_evidence(self):
        for value in [dict(review(), findings=[{'path': 'a.py', 'problem': 'bug'}]),
                      dict(review('CHANGES_REQUIRED'), findings=[])]:
            with self.assertRaises(protocol.ProtocolError):
                protocol.parse_review(json.dumps(value))
        with self.assertRaises(protocol.ProtocolError):
            protocol.parse_author(json.dumps(dict(author(), completion_evidence=[])))
        with self.assertRaises(protocol.ProtocolError):
            protocol.parse_review('{"decision":"APPROVED","decision":"APPROVED"}')

    def test_review_prompt_does_not_inline_whole_changed_files(self):
        prompt = prompts.patch_review_prompt('objective', 'x' * 60000, author(), 'passed', ['Cargo.lock'])
        self.assertLess(len(prompt), 26000)
        self.assertIn('@patch', prompt)


class AdapterTests(unittest.TestCase):
    def test_claude_config_honors_override_and_personal_directory(self):
        with tempfile.TemporaryDirectory() as td, mock.patch.object(Path, 'home', return_value=Path(td)), mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(config.default_claude_config_dir(), Path(td) / '.claude')
            (Path(td) / '.claude-personal').mkdir()
            self.assertEqual(config.default_claude_config_dir(), Path(td) / '.claude-personal')
            os.environ['CLAUDE_CONFIG_DIR'] = str(Path(td) / 'override')
            self.assertEqual(config.default_claude_config_dir(), Path(td) / 'override')

    def test_claude_uses_scoped_mcp_and_resumes_only_author(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            schema = root / 'schema.json'
            schema.write_text(json.dumps(protocol.AUTHOR_SCHEMA))
            candidate = mock.Mock(root=root)
            candidate.server_config.return_value = {'command': 'python3', 'args': ['trusted-file-tools.py']}
            calls = []
            def transport(argv, timeout, label, **kwargs):
                calls.append(argv)
                limited = len(calls) == 1
                kwargs['stdout_path'].write_text(json.dumps({
                    'type': 'result', 'subtype': 'error_max_turns' if limited else 'success',
                    'is_error': limited, 'session_id': 'abc-session',
                    'errors': ['Reached maximum number of turns (60)'] if limited else [],
                    'structured_output': author(), 'duration_ms': 10, 'num_turns': 2,
                    'usage': {'output_tokens': 12},
                }))
                return processes.Result(True, 'excerpt', .01)
            with mock.patch.object(engines.shutil, 'which', return_value='/bin/claude'), mock.patch.object(
                processes, 'run_process', side_effect=transport
            ):
                engine = engines.ClaudeEngine()
                results = []
                for i in range(2):
                    result = engine.run('implement', 10, schema, log_path=root / f'{i}.transcript.log', workspace=candidate, editable=True)
                    results.append(result)
                engine.run('review', 10, schema, log_path=root / 'r.transcript.log', workspace=candidate)
            self.assertEqual(calls[0][calls[0].index('--tools') + 1], '')
            self.assertIn('--restricted', calls[0])
            self.assertNotIn('--safe-mode', calls[0])
            self.assertIn('mcp__candidate__*', calls[0])
            self.assertIn('--max-turns', calls[0])
            self.assertEqual(calls[0][calls[0].index('--max-turns') + 1], '60')
            self.assertIn('--session-id', calls[0])
            self.assertIn('--resume', calls[1])
            self.assertNotIn('--resume', calls[2])
            self.assertEqual(result.metadata['usage']['output_tokens'], 12)
            self.assertFalse(results[0].ok)
            self.assertEqual(results[0].metadata['subtype'], 'error_max_turns')
            self.assertIn('Reached maximum number of turns (60)', results[0].note)
            self.assertTrue(result.ok)

    def test_codex_uses_required_file_server_and_disables_shell_and_integrations(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            candidate = mock.Mock(root=root)
            candidate.server_config.return_value = {'command': '/usr/bin/python3', 'args': ['-B', '/trusted/files.py', '--root', str(root)]}
            captured = []
            def transport(argv, timeout, label, **kwargs):
                captured.extend(argv)
                Path(argv[argv.index('-o') + 1]).write_text(json.dumps(review()))
                kwargs['log_path'].write_text(json.dumps({'type': 'turn.completed', 'usage': {'input_tokens': 50}}) + '\n')
                return processes.Result(True, 'excerpt', .01)
            with mock.patch.object(engines.shutil, 'which', return_value='/bin/codex'), mock.patch.object(processes, 'run_process', side_effect=transport):
                result = engines.CodexEngine().run('review', 10, log_path=root / 'review.transcript.log', workspace=candidate)
            self.assertTrue(result.ok)
            self.assertEqual(captured[captured.index('-s') + 1], 'read-only')
            for flag in ['features.shell_tool=false', 'features.multi_agent=false', 'features.apps=false',
                         'mcp_servers.candidate.required=true', '--ignore-user-config']:
                self.assertIn(flag, captured)
            candidate.server_config.assert_called_once_with(editable=False)
            self.assertEqual(result.metadata['usage']['input_tokens'], 50)

    def test_fatal_claude_envelope_keeps_provider_diagnostic(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            schema = root / 'schema.json'
            schema.write_text(json.dumps(protocol.AUTHOR_SCHEMA))
            def transport(*args, **kwargs):
                kwargs['stdout_path'].write_text(json.dumps({'type': 'result', 'is_error': True,
                    'subtype': 'error_max_turns', 'result': 'Turn budget exceeded'}))
                return processes.Result(True, '', .01)
            with mock.patch.object(engines.shutil, 'which', return_value='/bin/claude'), mock.patch.object(processes, 'run_process', side_effect=transport):
                result = engines.ClaudeEngine().run('implement', 10, schema, log_path=root / 'call.transcript.log')
            self.assertFalse(result.ok)
            self.assertIn('Turn budget exceeded', result.note)

    def test_profiles_have_no_condenser_and_security_is_explicit(self):
        for profile in engines.PROFILES.values():
            self.assertEqual(set(profile), {'implementer', 'reviewer'})
        self.assertEqual(engines.PROFILES['default']['implementer']().effort, 'medium')
        self.assertEqual(engines.PROFILES['security']['implementer']().effort, 'high')
        self.assertEqual(engines.PROFILES['security']['reviewer']().effort, 'xhigh')

    def test_preflight_rejects_missing_cli_and_writable_codex_sandbox(self):
        with mock.patch.object(engines.shutil, 'which', return_value=None):
            with self.assertRaises(protocol.CrewError):
                engines.ClaudeEngine().preflight()
        with mock.patch.object(engines.shutil, 'which', return_value='/bin/codex'):
            with self.assertRaises(protocol.CrewError):
                engines.CodexEngine(sandbox='workspace-write').preflight()


if __name__ == '__main__':
    unittest.main(verbosity=2)
