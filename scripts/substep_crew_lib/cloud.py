"""Opt-in AWS tools and exact-operation approvals, separate from file publication.

Only named AWS reads run inside the model's tool server. Other operations are
queued for the harness: review first, then explicit human approval for writes.
Reviewed Python scripts and Terraform run with host privileges, not a sandbox.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time
import uuid

if __package__ in (None, ''):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from substep_crew_lib import config, file_tools, patches, processes, protocol
else:
    from . import config, file_tools, patches, processes, protocol


READS = {
    'sts': {'get-caller-identity'},
    'cognito-idp': {'list-user-pools', 'describe-user-pool', 'list-user-pool-clients',
                    'describe-user-pool-client', 'admin-get-user', 'list-users'},
    'ecs': {'list-clusters', 'describe-clusters', 'list-services', 'describe-services',
            'list-tasks', 'describe-tasks', 'describe-task-definition'},
    'ec2': {'describe-vpcs', 'describe-subnets', 'describe-security-groups'},
    'secretsmanager': {'list-secrets', 'describe-secret'},
    's3api': {'list-buckets', 'get-bucket-location', 'head-bucket'},
    'iam': {'get-role', 'list-roles', 'list-role-policies', 'get-role-policy', 'list-attached-role-policies'},
}
WRITE_SERVICES = {'cognito-idp', 'secretsmanager', 'ecs'}
ROOTS = {'infra/envs/staging/foundation', 'infra/envs/staging/release'}
KINDS = {'aws_write', 'terraform_init', 'terraform_plan', 'terraform_apply', 'python_script'}
SECRET = re.compile(r'password|secretstring|secretbinary|clientsecret|accesstoken|refreshtoken|idtoken|'
                    r'secretaccesskey|accesskeyid|sessiontoken|authorization|credential', re.I)


def redact(value):
    if isinstance(value, dict):
        sensitive_value = SECRET.search(str(value.get('name', value.get('Name', ''))))
        return {k: '[REDACTED]' if SECRET.search(k.replace('_', '').replace('-', ''))
                or (k.lower() == 'value' and sensitive_value) else redact(v) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v) for v in value]
    return value


def mask(value, sensitive):
    if sensitive is True:
        return '[REDACTED]'
    if isinstance(value, dict):
        return {k: mask(v, sensitive.get(k, False) if isinstance(sensitive, dict) else False)
                for k, v in value.items()}
    if isinstance(value, list):
        return [mask(v, sensitive[i] if isinstance(sensitive, list) and i < len(sensitive) else False)
                for i, v in enumerate(value)]
    return value


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with path.open('w', encoding='utf-8') as stream:
        os.chmod(path, 0o600)
        json.dump(value, stream, indent=2, ensure_ascii=False)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def settings(profile, region, account):
    if not profile or not re.fullmatch(r'[\w.-]+', profile):
        raise protocol.CrewError('A named AWS profile is required')
    if not region or not re.fullmatch(r'[a-z]{2}(?:-[a-z]+)+-\d', region):
        raise protocol.CrewError('A valid AWS region is required')
    if not account or not re.fullmatch(r'\d{12}', account):
        raise protocol.CrewError('--aws-account-id must be the expected 12-digit staging account')
    return {'profile': profile, 'region': region, 'account': account}


class CloudAccess:
    def __init__(self, run, options, *, deadline=None, initialize=True):
        self.run = Path(run).resolve()
        self.directory = self.run / 'cloud'
        self.private = self.directory / 'private'
        self.options = settings(**options)
        self.deadline = deadline or time.time() + 2700
        self.pending = self.directory / 'pending.json'
        if initialize:
            self.private.mkdir(parents=True, exist_ok=True, mode=0o700)
            os.chmod(self.directory, 0o700)
            write_json(self.directory / 'settings.json', {'options': self.options, 'deadline': self.deadline})

    def environment(self):
        # Do not let ambient static keys, endpoint overrides, or Terraform flags
        # silently override the profile/account chosen by the operator.
        env = {k: v for k, v in os.environ.items() if not k.startswith(('AWS_', 'TF_'))}
        env.update(AWS_PROFILE=self.options['profile'], AWS_REGION=self.options['region'],
                   AWS_DEFAULT_REGION=self.options['region'], AWS_SDK_LOAD_CONFIG='1',
                   AWS_PAGER='', AWS_CLI_AUTO_PROMPT='off', AWS_MAX_ATTEMPTS='1',
                   TF_IN_AUTOMATION='1', TF_INPUT='0', PYTHONDONTWRITEBYTECODE='1')
        return env

    def command(self, argv, *, timeout=900, json_output=False):
        remaining = min(timeout, self.deadline - time.time())
        if remaining <= 0:
            raise protocol.CrewError('Cloud operation time budget exhausted')
        token = uuid.uuid4().hex
        log, output = self.private / f'{token}.log', self.private / f'{token}.stdout'
        for path in (log, output):
            path.touch(mode=0o600)
        with processes.console_output(False):
            result = processes.run_process(argv, remaining, 'cloud', env=self.environment(), inherit_env=False,
                                           cwd=config.REPO, log_path=log, stdout_path=output)
        if not result.ok:
            # Raw provider diagnostics can contain secrets; keep them out of model
            # transcripts. Operators can inspect the private log locally.
            raise protocol.CrewError(f'Cloud command failed or timed out; outcome may be partial. Private log: {log}')
        if json_output:
            if output.stat().st_size > 10_000_000:
                raise protocol.CrewError('Cloud JSON output exceeds 10 MB; narrow the request')
            return protocol.load_json(output.read_text()) if output.stat().st_size else {}
        return {'ok': True, 'private_log': str(log)}

    def aws(self, service, operation, parameters):
        # Parameter JSON cannot introduce CLI switches, shell expansion or an
        # alternate profile, endpoint, executable, or output file.
        return self.command(['aws', service, operation, '--profile', self.options['profile'],
                             '--region', self.options['region'], '--output', 'json', '--no-cli-pager',
                             '--cli-connect-timeout', '10', '--cli-read-timeout', '45',
                             '--cli-input-json', json.dumps(parameters)], timeout=60, json_output=True)

    def identity(self):
        try:
            identity = self.aws('sts', 'get-caller-identity', {})
        except (OSError, protocol.CrewError) as exc:
            raise protocol.CrewError(f'AWS authentication/preflight failed for {self.options["profile"]}. '
                                     f'Check the AWS CLI and local SSO session. {exc}') from exc
        if identity.get('Account') != self.options['account']:
            raise protocol.CrewError('AWS account mismatch; cloud access refused')
        return identity

    def artifact(self, prefix, value):
        path = self.directory / f'{prefix}-{uuid.uuid4().hex}.json'
        write_json(path, redact(value))
        return path.relative_to(config.REPO).as_posix()

    def public_path(self, name):
        if not re.fullmatch(r'\.agents/runs/[\w.-]+/cloud/(?:plan|result|request)-[a-f0-9]{32,64}\.json', name):
            raise protocol.CrewError('Only public cloud JSON artifacts may be read')
        path = config.REPO / name
        if not path.resolve().is_relative_to(config.RUNS.resolve()) or any(p.is_symlink() for p in [path, *path.parents]):
            raise protocol.CrewError('Cloud artifact path escapes the run directory')
        return path

    def validate_operation(self, operation):
        if not isinstance(operation, dict) or set(operation) != {'kind', 'parameters', 'purpose'}:
            raise protocol.CrewError('Expected kind, parameters, and purpose')
        kind, params = operation['kind'], operation['parameters']
        if kind not in KINDS or not isinstance(params, dict) or not isinstance(operation['purpose'], str) or not operation['purpose'].strip():
            raise protocol.CrewError('Invalid cloud operation')
        if len(json.dumps(operation)) > 24000:
            raise protocol.CrewError('Cloud request exceeds 24 KB')
        if redact(params) != params:
            raise protocol.CrewError('Do not put credentials, passwords or tokens in requests; use a reviewed script that handles them locally')
        allowed = {
            'aws_write': {'service', 'operation', 'input'}, 'terraform_init': {'root', 'backend'},
            'terraform_plan': {'root', 'variables'}, 'terraform_apply': {'plan'}, 'python_script': {'path', 'args'},
        }[kind]
        if set(params) != allowed:
            raise protocol.CrewError(f'{kind} requires exactly: {sorted(allowed)}')
        if kind == 'aws_write':
            if (params['service'] not in WRITE_SERVICES or not isinstance(params['operation'], str)
                    or not re.fullmatch(r'[a-z]+(?:-[a-z0-9]+)+', params['operation']) or not isinstance(params['input'], dict)):
                raise protocol.CrewError('Unsupported AWS write operation')
        elif kind in {'terraform_init', 'terraform_plan'}:
            self.terraform_root(params['root'])
            values = params['backend' if kind == 'terraform_init' else 'variables']
            if not isinstance(values, dict):
                raise protocol.CrewError('Terraform values must be an object')
            if kind == 'terraform_init' and (set(values) - {'bucket', 'kms_key_id'} or any(not isinstance(v, str) for v in values.values())):
                raise protocol.CrewError('Backend overrides may contain only bucket and kms_key_id')
        elif kind == 'terraform_apply':
            self.plan(params['plan'])
        elif kind == 'python_script':
            path = file_tools.path_in(config.REPO, params['path'])
            if not params['path'].startswith('scripts/cloud/') or path.suffix != '.py':
                raise protocol.CrewError('Cloud scripts must be Python files under scripts/cloud/')
            if not isinstance(params['args'], list) or any(not isinstance(v, str) for v in params['args']):
                raise protocol.CrewError('Script args must be strings')

    def terraform_root(self, name):
        if name not in ROOTS:
            raise protocol.CrewError('Terraform is limited to the staging foundation/release roots')
        path = config.REPO / name
        if not path.resolve().is_relative_to(config.REPO.resolve()) or any(p.is_symlink() for p in [path, *path.parents]):
            raise protocol.CrewError('Terraform roots cannot contain symlinks')

    def queue(self, operation):
        self.validate_operation(operation)
        if self.pending.exists():
            raise protocol.CrewError('One operation is already queued; return CONTINUE for the harness to handle it')
        write_json(self.pending, operation)
        return {'queued': True, 'next': 'Return CONTINUE. The harness reviews this operation; writes require operator approval.'}

    def prepare(self):
        operation = protocol.load_json(self.pending.read_text())
        self.validate_operation(operation)
        request = {'operation': operation, 'aws': dict(self.options), 'source_state': patches.repository_state(),
                   'nonce': uuid.uuid4().hex}
        if operation['kind'] == 'terraform_apply':
            plan = self.plan(operation['parameters']['plan'])
            request['plan_sha256'] = plan['sha256']
            request['plan_artifact_sha256'] = plan['artifact_sha256']
        request['id'] = digest(request)
        path = self.directory / f'request-{request["id"]}.json'
        write_json(path, request)
        self.pending.unlink()
        return request, path

    def plan(self, reference):
        path = self.public_path(reference)
        if not path.name.startswith('plan-'):
            raise protocol.CrewError('Expected a saved Terraform plan artifact')
        plan = protocol.load_json(path.read_text())
        self.terraform_root(plan['root'])
        binary = path.parent / 'private' / (path.stem + '.tfplan')
        if (binary.is_symlink() or not binary.is_file() or plan['aws'] != self.options
                or plan['root'] not in ROOTS or plan['source_state'] != patches.repository_state()
                or hashlib.sha256(binary.read_bytes()).hexdigest() != plan['sha256']):
            raise protocol.CrewError('Saved Terraform plan is stale, altered, or belongs to another account/profile')
        return {**plan, 'binary': str(binary), 'artifact_sha256': hashlib.sha256(path.read_bytes()).hexdigest()}

    def check_request(self, request):
        if request.get('id') != digest({k: v for k, v in request.items() if k != 'id'}):
            raise protocol.CrewError('Cloud approval request was altered')
        if request['aws'] != self.options or request['source_state'] != patches.repository_state():
            raise protocol.CrewError('Cloud approval is stale or belongs to another account/profile; prepare a new request')
        self.validate_operation(request['operation'])
        if request['operation']['kind'] == 'terraform_apply':
            plan = self.plan(request['operation']['parameters']['plan'])
            if request['plan_sha256'] != plan['sha256'] or request['plan_artifact_sha256'] != plan['artifact_sha256']:
                raise protocol.CrewError('Terraform plan differs from the approved plan')

    def execute(self, request, *, approval=None, receipt_path=None):
        self.check_request(request)
        kind, params = request['operation']['kind'], request['operation']['parameters']
        if kind not in {'terraform_init', 'terraform_plan'} and approval != request['id']:
            raise protocol.CrewError('Cloud write requires explicit operator approval of this request ID')
        self.identity()
        self.check_request(request)
        # Claim before starting. Failures/timeouts may already have cloud effects;
        # never automatically replay them with the same approval.
        receipt = receipt_path or self.directory / f'execution-{request["id"]}.json'
        if receipt.exists():
            raise protocol.CrewError('This cloud operation already started; inspect its execution receipt before preparing another request')
        with receipt.open('x') as stream:
            os.chmod(receipt, 0o600)
            json.dump({'status': 'started', 'id': request['id']}, stream)
        if kind == 'aws_write':
            result = self.aws(params['service'], params['operation'], params['input'])
        elif kind == 'python_script':
            result = self.command([sys.executable, '-B', str(file_tools.path_in(config.REPO, params['path'])),
                                   *params['args']], json_output=True)
        elif kind == 'terraform_apply':
            plan = self.plan(params['plan'])
            result = self.command(['terraform', f'-chdir={plan["root"]}', 'apply', '-input=false', '-no-color', plan['binary']])
        else:
            argv = ['terraform', f'-chdir={params["root"]}']
            if kind == 'terraform_init':
                result = self.command(argv + ['init', '-input=false', '-no-color', '-lockfile=readonly'] +
                                      [f'-backend-config={k}={v}' for k, v in sorted(params['backend'].items())])
            else:
                name = 'plan-' + uuid.uuid4().hex
                binary = self.private / f'{name}.tfplan'
                variables = self.private / f'{name}.tfvars.json'
                write_json(variables, {**params['variables'], 'aws_account_id': self.options['account'],
                                       'aws_region': self.options['region']})
                self.command(argv + ['plan', '-input=false', '-no-color', '-lock-timeout=30s',
                                     f'-var-file={variables}', f'-out={binary}'])
                os.chmod(binary, 0o600)
                raw = self.command(argv + ['show', '-json', str(binary)], json_output=True)
                changes = [{k: change[k] for k in ('address', 'type')} | {
                    'actions': change['change']['actions'],
                    'before': mask(change['change'].get('before'), change['change'].get('before_sensitive')),
                    'after': mask(change['change'].get('after'), change['change'].get('after_sensitive')),
                    'after_unknown': change['change'].get('after_unknown'),
                } for change in raw.get('resource_changes', [])]
                public = self.directory / f'{name}.json'
                write_json(public, {'aws': dict(self.options), 'root': params['root'], 'source_state': request['source_state'],
                                    'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                                    'changes': redact(changes), 'outputs': redact({key: {
                                        'actions': item.get('actions'),
                                        'before': mask(item.get('before'), item.get('before_sensitive')),
                                        'after': mask(item.get('after'), item.get('after_sensitive')),
                                        'after_unknown': item.get('after_unknown'),
                                    } for key, item in raw.get('output_changes', {}).items()})})
                result = {'plan': public.relative_to(config.REPO).as_posix(), 'changes': redact(changes)}
        if patches.repository_state() != request['source_state']:
            raise protocol.CrewError('Cloud command changed source files; effects may have occurred. Inspect before resuming.')
        artifact = self.artifact('result', {'request_id': request['id'], 'operation': request['operation'], 'result': result})
        write_json(receipt, {'status': 'completed', 'id': request['id'], 'artifact': artifact})
        return artifact

    def server_config(self, *, editable):
        return {'command': sys.executable, 'args': ['-B', str(Path(__file__).resolve()), '--run', str(self.run)]
                + ([] if editable else ['--read-only'])}


class CloudTools:
    def __init__(self, access, *, editable):
        self.access, self.editable = access, editable

    def definitions(self):
        specs = [
            ('cloud_read_artifact', 'Read public cloud request/result/plan JSON, never raw plans or private logs.',
             file_tools.schema({'path': file_tools.S, 'start_line': {'type': 'integer'}, 'line_count': {'type': 'integer'}}, ['path'])),
        ]
        if self.editable:
            specs += [
                ('aws_read', 'Run an allowlisted AWS read using the operator profile/account. No secrets retrieval. Parameters are AWS API JSON.',
                 file_tools.schema({'service': file_tools.S, 'operation': file_tools.S, 'parameters_json': file_tools.S},
                                   ['service', 'operation', 'parameters_json'])),
                ('cloud_request', 'Queue one operation, then return CONTINUE. Kinds: aws_write (service,operation,input); '
                 'terraform_init (root,backend); terraform_plan (root,variables); terraform_apply (plan artifact path); '
                 'python_script (path under scripts/cloud/,args). Writes await human approval. Never include secret values.',
                 file_tools.schema({'kind': file_tools.S, 'parameters_json': file_tools.S, 'purpose': file_tools.S},
                                   ['kind', 'parameters_json', 'purpose'])),
            ]
        return [{'name': name, 'description': description, 'inputSchema': spec,
                 'annotations': {'readOnlyHint': name != 'cloud_request'}} for name, description, spec in specs]

    def call(self, name, args):
        spec = next((item['inputSchema'] for item in self.definitions() if item['name'] == name), None)
        if spec is None or protocol._schema_error(args, spec):
            raise ValueError('Unavailable cloud tool or invalid arguments')
        try:
            if name == 'cloud_read_artifact':
                start, count = args.get('start_line', 1), args.get('line_count', 150)
                if start < 1 or not 1 <= count <= 300:
                    raise ValueError('Invalid line range')
                return file_tools.log_range(self.access.public_path(args['path']), start, count)
            params = protocol.load_json(args['parameters_json'])
            if not isinstance(params, dict):
                raise ValueError('Parameters must be an object')
            if name == 'cloud_request':
                return self.access.queue({'kind': args['kind'], 'parameters': params, 'purpose': args['purpose']})
            if args['operation'] not in READS.get(args['service'], set()):
                raise ValueError('AWS read not allowlisted; use a reviewed cloud script for other operations')
            self.access.identity()
            result = redact(self.access.aws(args['service'], args['operation'], params))
            artifact = self.access.artifact('result', result)
            return {'artifact': artifact, 'result': result if len(json.dumps(result)) <= 16000 else '(read artifact for full result)'}
        except protocol.CrewError as exc:
            raise ValueError(str(exc)) from exc


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('--run', type=Path, required=True)
    parser.add_argument('--read-only', action='store_true')
    args = parser.parse_args()
    saved = protocol.load_json((args.run / 'cloud/settings.json').read_text())
    access = CloudAccess(args.run, saved['options'], deadline=saved['deadline'], initialize=False)
    file_tools.serve(CloudTools(access, editable=not args.read_only))
