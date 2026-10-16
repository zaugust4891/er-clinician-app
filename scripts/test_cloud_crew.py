"""Offline cloud transport, exact approvals, and blocked/resume regressions."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock

if __package__:
    from . import test_candidate_crew as fixtures
    from .substep_crew_lib import cli, cloud, config, engines, guides, processes, protocol
else:
    import test_candidate_crew as fixtures
    from substep_crew_lib import cli, cloud, config, engines, guides, processes, protocol


OPTIONS = {'profile': 'staging-test', 'region': 'us-east-2', 'account': '123456789012'}


def operation(kind='aws_write', parameters=None):
    return {'kind': kind, 'parameters': parameters if parameters is not None else {
        'service': 'cognito-idp', 'operation': 'admin-create-user',
        'input': {'UserPoolId': 'us-east-2_example', 'Username': 'synthetic-clinician-a', 'MessageAction': 'SUPPRESS'},
    }, 'purpose': 'Provision the synthetic staging user'}


class CloudTests(unittest.TestCase):
    git = fixtures.CandidateTests.git
    edit = fixtures.CandidateTests.edit
    loop = fixtures.CandidateTests.loop

    def setUp(self):
        fixtures.CandidateTests.setUp(self)
        self.access = cloud.CloudAccess(self.run_dir, OPTIONS)

    def prepare(self, value=None):
        self.access.queue(value or operation())
        return self.access.prepare()

    def test_cloud_tools_are_opt_in_and_reviewer_has_only_artifact_reads(self):
        reader = cloud.CloudTools(self.access, editable=False)
        self.assertEqual([s['name'] for s in reader.definitions()], ['cloud_read_artifact'])
        for name in ('aws_read', 'cloud_request'):
            with self.assertRaises(ValueError):
                reader.call(name, {})

    def test_actual_stdio_server_exposes_only_reads_to_reviewer(self):
        server = self.access.server_config(editable=False)
        settings_before = (self.access.directory / 'settings.json').read_bytes()
        requests = [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {}},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'},
            {'jsonrpc': '2.0', 'id': 3, 'method': 'tools/call', 'params': {'name': 'cloud_request', 'arguments': {}}},
        ]
        result = subprocess.run([server['command'], *server['args']], capture_output=True, text=True,
                                input=''.join(json.dumps(r) + '\n' for r in requests), timeout=10, check=True)
        responses = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual([t['name'] for t in responses[1]['result']['tools']], ['cloud_read_artifact'])
        self.assertTrue(responses[2]['result']['isError'])
        self.assertEqual((self.access.directory / 'settings.json').read_bytes(), settings_before)

    def test_aws_reads_reject_writes_and_secret_retrieval_and_redact_results(self):
        tools = cloud.CloudTools(self.access, editable=True)
        for service, name in [('cognito-idp', 'admin-create-user'), ('secretsmanager', 'get-secret-value'), ('sts', 'assume-role')]:
            with self.assertRaisesRegex(ValueError, 'not allowlisted'):
                tools.call('aws_read', {'service': service, 'operation': name, 'parameters_json': '{}'})
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'aws', return_value={
            'UserPoolClient': {'ClientId': 'client-id', 'ClientSecret': 'must-not-leak'},
            'environment': [{'name': 'DB_PASSWORD', 'value': 'must-not-leak'}],
        }):
            result = tools.call('aws_read', {'service': 'cognito-idp', 'operation': 'describe-user-pool-client', 'parameters_json': '{}'})
        self.assertNotIn('must-not-leak', json.dumps(result))
        self.assertNotIn('must-not-leak', self.access.public_path(result['artifact']).read_text())

    def test_account_mismatch_stops_before_resource_query(self):
        with mock.patch.object(self.access, 'aws', return_value={'Account': '000000000000'}) as aws:
            with self.assertRaisesRegex(ValueError, 'account mismatch'):
                cloud.CloudTools(self.access, editable=True).call('aws_read', {
                    'service': 'cognito-idp', 'operation': 'list-user-pools', 'parameters_json': '{}'})
        self.assertEqual(aws.call_count, 1)
        self.assertEqual(aws.call_args.args[:2], ('sts', 'get-caller-identity'))

    def test_named_profile_environment_removes_ambient_credentials_and_overrides(self):
        with mock.patch.dict(os.environ, {'AWS_ACCESS_KEY_ID': 'wrong', 'AWS_SECRET_ACCESS_KEY': 'wrong',
                                         'AWS_PROFILE': 'production', 'AWS_ENDPOINT_URL': 'https://wrong',
                                         'TF_CLI_ARGS': '-destroy', 'TF_VAR_password': 'secret'}):
            env = self.access.environment()
        self.assertEqual(env['AWS_PROFILE'], OPTIONS['profile'])
        self.assertEqual(env['AWS_REGION'], OPTIONS['region'])
        for key in ('AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_ENDPOINT_URL', 'TF_CLI_ARGS', 'TF_VAR_password'):
            self.assertNotIn(key, env)

    def test_transport_is_bounded_no_shell_and_private_output_not_console(self):
        def transport(argv, timeout, label, **kwargs):
            self.assertFalse(kwargs['inherit_env'])
            self.assertLessEqual(timeout, 60)
            self.assertEqual(argv[:3], ['aws', 'sts', 'get-caller-identity'])
            self.assertEqual(argv[argv.index('--profile') + 1], OPTIONS['profile'])
            self.assertEqual(kwargs['stdout_path'].stat().st_mode & 0o777, 0o600)
            kwargs['stdout_path'].write_text(json.dumps({'Account': OPTIONS['account']}))
            return processes.Result(True, 'not exposed', .01)
        with mock.patch.object(processes, 'run_process', side_effect=transport):
            self.assertEqual(self.access.identity()['Account'], OPTIONS['account'])

    def test_write_needs_exact_approval_and_cannot_replay(self):
        request, path = self.prepare()
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'aws', return_value={'ok': True}) as aws:
            for approval in (None, 'another-id'):
                with self.assertRaisesRegex(protocol.CrewError, 'explicit operator approval'):
                    self.access.execute(request, approval=approval)
            aws.assert_not_called()
            artifact = self.access.execute(request, approval=request['id'])
            self.assertTrue(self.access.public_path(artifact).exists())
            with self.assertRaisesRegex(protocol.CrewError, 'already started'):
                self.access.execute(request, approval=request['id'])
            self.assertEqual(aws.call_count, 1)

    def test_failed_write_is_not_replayed_after_uncertain_outcome(self):
        request, _ = self.prepare()
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'aws', side_effect=protocol.CrewError('timeout')) as aws:
            with self.assertRaisesRegex(protocol.CrewError, 'timeout'):
                self.access.execute(request, approval=request['id'])
            with self.assertRaisesRegex(protocol.CrewError, 'already started'):
                self.access.execute(request, approval=request['id'])
            self.assertEqual(aws.call_count, 1)

    def test_changed_request_source_or_account_invalidates_approval(self):
        request, _ = self.prepare()
        changed = json.loads(json.dumps(request))
        changed['operation']['parameters']['input']['Username'] = 'another-user'
        with self.assertRaisesRegex(protocol.CrewError, 'altered'):
            self.access.check_request(changed)
        with mock.patch.dict(self.access.options, {'account': '000000000000'}):
            with self.assertRaisesRegex(protocol.CrewError, 'another account'):
                self.access.check_request(request)
        (self.root / 'a.py').write_text('external change\n')
        with self.assertRaisesRegex(protocol.CrewError, 'stale'):
            self.access.check_request(request)

    def test_operation_paths_and_inline_credentials_are_rejected(self):
        values = [operation('terraform_plan', {'root': 'infra/envs/prod/foundation', 'variables': {}}),
                  operation('terraform_init', {'root': 'infra/envs/staging/foundation', 'backend': {'profile': 'production'}}),
                  operation('python_script', {'path': '../outside.py', 'args': []}),
                  operation('python_script', {'path': 'scripts/db-secrets.py', 'args': []}),
                  operation('aws_write', {'service': 'cognito-idp', 'operation': 'admin-set-user-password',
                                           'input': {'Password': 'must-not-leak'}})]
        for value in values:
            with self.subTest(value=value), self.assertRaises((ValueError, protocol.CrewError)):
                self.access.queue(value)
        self.assertFalse(self.access.pending.exists())

    def test_terraform_root_cannot_be_redirected_through_a_symlink(self):
        directory = self.root / 'infra/envs/staging'
        directory.mkdir(parents=True)
        (directory / 'foundation').symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(protocol.CrewError, 'symlinks'):
            self.prepare(operation('terraform_plan', {'root': 'infra/envs/staging/foundation', 'variables': {}}))

    def test_approved_python_script_runs_locally_with_profile_and_returns_redacted_json(self):
        path = self.root / 'scripts/cloud/check.py'
        path.parent.mkdir(parents=True)
        path.write_text('import json, os, sys\nprint(json.dumps({"profile": os.environ["AWS_PROFILE"], '
                        '"has_static_key": bool(os.environ.get("AWS_ACCESS_KEY_ID")), '
                        '"argument": sys.argv[1], "AccessToken": "private-token"}))\n')
        argument = 'literal $(not a shell command)'
        request, _ = self.prepare(operation('python_script', {'path': 'scripts/cloud/check.py', 'args': [argument]}))
        with mock.patch.object(self.access, 'identity'), mock.patch.dict(os.environ, {'AWS_ACCESS_KEY_ID': 'wrong-key'}):
            artifact = self.access.execute(request, approval=request['id'])
        result = json.loads(self.access.public_path(artifact).read_text())['result']
        self.assertEqual(result['profile'], OPTIONS['profile'])
        self.assertFalse(result['has_static_key'])
        self.assertEqual(result['argument'], argument)
        self.assertEqual(result['AccessToken'], '[REDACTED]')

    def test_plan_is_redacted_and_apply_uses_only_the_exact_saved_binary(self):
        request, _ = self.prepare(operation('terraform_plan', {'root': 'infra/envs/staging/foundation', 'variables': {'auth': {'enabled': True}}}))
        calls = []
        def command(argv, **kwargs):
            calls.append(argv)
            if 'plan' in argv:
                binary = Path(next(a.removeprefix('-out=') for a in argv if a.startswith('-out=')))
                binary.write_bytes(b'exact reviewed plan')
                return {'ok': True}
            if 'show' in argv:
                return {'resource_changes': [{'address': 'aws_secret.example', 'type': 'aws_secret', 'change': {
                    'actions': ['create'], 'after': {'value': 'hidden'}, 'after_sensitive': {'value': True},
                }}], 'output_changes': {'token': {'actions': ['create'], 'after': 'hidden-output', 'after_sensitive': True}}}
            return {'ok': True}
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'command', side_effect=command):
            artifact = self.access.execute(request)
            result = json.loads(self.access.public_path(artifact).read_text())['result']
            public = self.access.public_path(result['plan'])
            self.assertNotIn('hidden', public.read_text())
            apply, _ = self.prepare(operation('terraform_apply', {'plan': result['plan']}))
            with self.assertRaisesRegex(protocol.CrewError, 'explicit operator approval'):
                self.access.execute(apply)
            self.access.execute(apply, approval=apply['id'])
        self.assertEqual(sum('plan' in c for c in calls), 1)
        self.assertEqual(sum('apply' in c for c in calls), 1)
        self.assertTrue(calls[-1][-1].endswith('.tfplan'))
        original = public.read_text()
        public.write_text(original + '\n')
        with self.assertRaisesRegex(protocol.CrewError, 'differs'):
            self.access.check_request(apply)
        public.write_text(original)
        binary = Path(self.access.plan(result['plan'])['binary'])
        binary.write_bytes(b'tampered plan')
        with self.assertRaisesRegex(protocol.CrewError, 'altered'):
            self.access.check_request(apply)

    def test_public_artifact_reader_cannot_read_raw_plans_logs_or_symlinks(self):
        for name in ('.aws/credentials', '../secret', '.agents/runs/test-run/cloud/private/plan.tfplan',
                     '.agents/runs/test-run/cloud/settings.json'):
            with self.assertRaises(protocol.CrewError):
                self.access.public_path(name)
        link = self.access.directory / ('result-' + 'a' * 32 + '.json')
        link.symlink_to(self.root / 'a.py')
        with self.assertRaises(protocol.CrewError):
            self.access.public_path(link.relative_to(self.root).as_posix())

    def test_blocked_author_preserves_draft_without_review_or_false_failure(self):
        response = dict(fixtures.author('BLOCKED'), explanation='Need an authenticated staging session')
        loop = self.loop([self.edit(2, response)], [])
        self.assertEqual(loop.run(), 3)
        self.assertEqual(self.gate_candidates, [])
        self.assertTrue((self.run_dir / 'blocked.json').exists())
        self.assertTrue((self.run_dir / 'pending-proposal.json').exists())
        self.assertFalse((self.run_dir / 'completion.json').exists())
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_cloud_request_without_file_edit_reviews_then_waits_for_human(self):
        def queue(candidate):
            self.access.queue(operation())
            return fixtures.author('CONTINUE')
        loop = self.loop([queue], [fixtures.review()])
        loop.cloud = self.access
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'execute') as execute:
            self.assertEqual(loop.run(), 3)
        execute.assert_not_called()
        self.assertEqual(self.gate_candidates, [])
        self.assertIn('--approve-cloud', (self.run_dir / 'blocked.json').read_text())
        self.assertTrue(list(self.access.directory.glob('request-*.json')))

    def test_code_is_checked_and_published_before_cloud_operation_review(self):
        def edit_and_queue(candidate):
            self.access.queue(operation())
            return self.edit(2, fixtures.author('CONTINUE'))(candidate)
        def inspect(candidate):
            self.assertEqual((self.root / 'a.py').read_text(), 'value = 2\n')
            return fixtures.review()
        loop = self.loop([edit_and_queue], [fixtures.review(), inspect])
        loop.cloud = self.access
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'execute') as execute:
            self.assertEqual(loop.run(), 3)
        execute.assert_not_called()
        self.assertEqual(self.gate_candidates, ['value = 2\n'])
        self.assertEqual(len(loop.history), 1)

    def test_cli_approval_resumes_exact_request_and_returns_execution_evidence(self):
        request, _ = self.prepare()
        (self.run_dir / 'run.json').write_text(json.dumps({'phase': 1, 'step': 1, 'substep': 'S1'}))
        self.loop([], [])  # mock deterministic gates
        author = fixtures.FakeEngine([fixtures.author()])
        reviewer = fixtures.FakeEngine([fixtures.review(), fixtures.review()])
        roles = {'implementer': lambda: author, 'reviewer': lambda: reviewer}
        with mock.patch.object(engines, 'PROFILES', {'default': roles}), mock.patch.object(
            guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())
        ), mock.patch.object(sys, 'argv', ['substep_crew.py', '--resume', str(self.run_dir), '--approve-cloud', request['id']]), mock.patch.object(
            cloud.CloudAccess, 'identity'
        ), mock.patch.object(cloud.CloudAccess, 'aws', return_value={'created': True}) as aws:
            self.assertEqual(cli.main(), 0)
        self.assertEqual(aws.call_count, 1)
        self.assertIn('Cloud operation executed', author.calls[0][0])
        receipt = json.loads((self.access.directory / f'execution-{request["id"]}.json').read_text())
        self.assertEqual(receipt['status'], 'completed')

    def test_cli_live_dry_run_makes_no_aws_calls(self):
        with mock.patch.object(guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())), mock.patch.object(
            sys, 'argv', ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1', '--dry-run',
                          '--aws-profile', OPTIONS['profile'], '--aws-account-id', OPTIONS['account']]
        ), mock.patch.object(cloud.CloudAccess, 'identity', side_effect=AssertionError('AWS called')):
            self.assertEqual(cli.main(), 0)

    def test_cli_missing_authentication_is_blocked_not_failed(self):
        author = fixtures.FakeEngine([])
        roles = {'implementer': lambda: author, 'reviewer': lambda: fixtures.FakeEngine([])}
        with mock.patch.object(guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())), mock.patch.object(
            engines, 'PROFILES', {'default': roles}
        ), mock.patch.object(sys, 'argv', ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1',
                                          '--aws-profile', OPTIONS['profile'], '--aws-account-id', OPTIONS['account']]), mock.patch.object(
            cloud.CloudAccess, 'identity', side_effect=protocol.CrewError('SSO session expired')
        ):
            self.assertEqual(cli.main(), 3)
        blocked = list(config.RUNS.glob('*/blocked.json'))
        self.assertEqual(len(blocked), 1)
        status = json.loads((blocked[0].parent / 'progress.json').read_text())
        self.assertEqual(status['status'], 'blocked')
        self.assertIsNone(status['error'])
        self.assertEqual(author.calls, [])

    def test_cli_cloud_mode_requires_explicit_expected_account(self):
        with mock.patch.object(guides, 'find_guide', return_value=(self.root / 'guide.md', (self.root / 'guide.md').read_text())), mock.patch.object(
            sys, 'argv', ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1', '--aws-profile', OPTIONS['profile']]
        ), mock.patch.object(cloud.CloudAccess, 'identity', side_effect=AssertionError('AWS called')):
            self.assertEqual(cli.main(), 2)

    def test_explicit_approval_still_requires_independent_review(self):
        request, path = self.prepare()
        loop = self.loop([dict(fixtures.author('BLOCKED'), explanation='Repair cloud operation first')],
                         [fixtures.review('CHANGES_REQUIRED')])
        loop.cloud = self.access
        loop.cloud_approval = request, path
        with mock.patch.object(self.access, 'identity'), mock.patch.object(self.access, 'execute') as execute:
            self.assertEqual(loop.run(), 3)
        execute.assert_not_called()
        self.assertIn('Cloud operation rejected', self.author_engine.calls[0][0])

    def test_both_adapters_pass_the_correct_cloud_server_role(self):
        candidate = mock.Mock(root=self.root)
        candidate.server_config.return_value = {'command': 'python3', 'args': ['file-tools.py']}
        candidate.cloud_servers = {role: {'cloud': self.access.server_config(editable=role == 'author')}
                                   for role in ('author', 'reviewer')}
        schema = self.run_dir / 'schema.json'
        schema.write_text(json.dumps(protocol.AUTHOR_SCHEMA))
        calls = []
        def transport(argv, timeout, label, **kwargs):
            calls.append(argv)
            if argv[0] == 'claude':
                kwargs['stdout_path'].write_text(json.dumps({'type': 'result', 'subtype': 'success', 'is_error': False,
                                                            'structured_output': fixtures.author()}))
            else:
                Path(argv[argv.index('-o') + 1]).write_text(json.dumps(fixtures.author()))
            return processes.Result(True, '', .01)
        with mock.patch.object(engines.shutil, 'which', return_value='/bin/model'), mock.patch.object(
            processes, 'run_process', side_effect=transport
        ):
            for engine in (engines.ClaudeEngine(), engines.CodexEngine()):
                for editable in (True, False):
                    engine.run('request', 10, schema, log_path=self.run_dir / f'{len(calls)}.log', workspace=candidate, editable=editable)
        for index, argv in enumerate(calls):
            if argv[0] == 'claude':
                servers = json.loads(argv[argv.index('--mcp-config') + 1])['mcpServers']
                args = servers['cloud']['args']
                self.assertEqual(argv[argv.index('--tools') + 1], '')
                self.assertIn('mcp__cloud__*', argv)
            else:
                args = json.loads(next(item.split('=', 1)[1] for item in argv if item.startswith('mcp_servers.cloud.args=')))
                self.assertIn('features.shell_tool=false', argv)
            self.assertEqual('--read-only' in args, index % 2 == 1)


if __name__ == '__main__':
    unittest.main()
