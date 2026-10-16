"""CLI adapters with scoped candidate file tools and no model shell execution."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import time
import uuid

from . import config, processes, protocol


class Engine:
    name = 'engine'
    tools = True

    def preflight(self):
        pass

    def run(self, prompt, timeout, schema=None, *, log_path=None, workspace=None, editable=False):
        raise NotImplementedError


def _invocation_final_path(log_path, suffix):
    if log_path is None:
        config.RUNS.mkdir(parents=True, exist_ok=True)
        return config.RUNS / f'standalone-{os.getpid()}-{time.time_ns()}.final{suffix}'
    return log_path.with_name(log_path.name.removesuffix('.transcript.log') + '.final' + suffix)


class ClaudeEngine(Engine):
    def __init__(self, model='claude-opus-5-5', *, effort='medium', config_dir=None, max_turns=60):
        self.model, self.effort, self.max_turns = model, effort, max_turns
        self.config_dir = config_dir or config.default_claude_config_dir()
        self.name = f'claude:{model} (effort={effort})'
        self.session_id = None
        self.session_root = None

    def preflight(self):
        if not shutil.which('claude'):
            raise protocol.CrewError('claude CLI not found')
        if self.max_turns < 1:
            raise protocol.CrewError('Claude max_turns must be at least 1')

    def run(self, prompt, timeout, schema=None, *, log_path=None, workspace=None, editable=False):
        self.preflight()
        if editable and workspace is None:
            raise protocol.CrewError('Editing requires an isolated candidate workspace')
        final_path = _invocation_final_path(log_path, '.json' if schema else '.md')
        final_path.parent.mkdir(parents=True, exist_ok=True)
        final_path.unlink(missing_ok=True)
        servers = {'candidate': workspace.server_config(editable=editable)} if workspace else {}
        extra = getattr(workspace, 'cloud_servers', {})
        if isinstance(extra, dict):
            servers.update(extra.get('author' if editable else 'reviewer', {}))
        # Restricted mode confines built-in file tools and ignores project/user
        # customization. Built-ins are removed entirely; only our path-checked
        # MCP tools remain. Safe mode is not used: it disables MCP as well.
        argv = ['claude', '-p', '--model', self.model, '--effort', self.effort,
                '--permission-mode', 'dontAsk', '--restricted', '--tools', '',
                '--strict-mcp-config', '--mcp-config', json.dumps({'mcpServers': servers}),
                '--disable-slash-commands', '--settings', '{"disableAllHooks":true}',
                '--output-format', 'json' if schema else 'text', '--max-turns', str(self.max_turns)]
        if workspace:
            argv += ['--allowedTools', 'mcp__candidate__*']
            if 'cloud' in servers:
                argv += ['mcp__cloud__*']
        if schema:
            argv += ['--json-schema', json.dumps(json.loads(schema.read_text()))]
        # Only the author retains context; every review starts independently.
        if editable:
            root = str(workspace.root)
            if self.session_root != root:
                self.session_id, self.session_root = None, root
            argv += ['--resume', self.session_id] if self.session_id else ['--session-id', str(uuid.uuid4())]
        result = processes.run_process(
            argv, timeout, 'claude', env={'CLAUDE_CONFIG_DIR': str(self.config_dir), 'PAI_DIR': str(self.config_dir)},
            log_path=log_path, stdout_path=final_path, input_text=prompt,
            cwd=workspace.root if workspace else None,
        )
        try:
            raw = final_path.read_text(encoding='utf-8')
            result.output = raw
            if schema:
                envelope = protocol.load_json(raw)
                if isinstance(envelope, dict) and envelope.get('type') == 'result':
                    result.metadata = {key: envelope.get(key) for key in
                                       ('duration_ms', 'duration_api_ms', 'num_turns', 'usage', 'modelUsage',
                                        'subtype', 'session_id', 'terminal_reason')}
                    if editable and envelope.get('session_id'):
                        self.session_id = envelope['session_id']
                    if envelope.get('is_error') is not False or envelope.get('subtype') != 'success':
                        result.ok = False
                        result.note = str(envelope.get('result') or envelope.get('api_error')
                                          or '; '.join(str(item) for item in (envelope.get('errors') or []))
                                          or envelope.get('subtype'))
                    elif 'structured_output' not in envelope:
                        result.ok = False
                        result.note = 'Claude returned no structured output'
                    else:
                        result.output = json.dumps(envelope['structured_output'], ensure_ascii=False)
        except (OSError, UnicodeError, ValueError) as exc:
            result.ok = False
            result.note = result.note or f'Cannot read Claude final response: {exc}'
        return result


class CodexEngine(Engine):
    def __init__(self, model='gpt-6.1-sol', *, effort='high', sandbox='read-only', oss=False, local_provider='ollama'):
        self.model, self.effort, self.sandbox = model, effort, sandbox
        self.oss, self.local_provider = oss, local_provider
        self.name = f'codex:{model} (effort={effort})'

    def preflight(self):
        if not shutil.which('codex'):
            raise protocol.CrewError('codex CLI not found')
        if self.sandbox != 'read-only':
            raise protocol.CrewError('Candidate sessions require the read-only Codex sandbox')

    def run(self, prompt, timeout, schema=None, *, log_path=None, workspace=None, editable=False):
        self.preflight()
        if editable and workspace is None:
            raise protocol.CrewError('Editing requires an isolated candidate workspace')
        final_path = _invocation_final_path(log_path, '.json' if schema else '.md')
        final_path.parent.mkdir(parents=True, exist_ok=True)
        final_path.unlink(missing_ok=True)
        argv = ['codex', 'exec', '--cd', str(workspace.root if workspace else config.REPO),
                '-s', 'read-only', '--color', 'never', '--ignore-rules', '--ignore-user-config', '--ephemeral', '--json']
        # All file reads go through our stdio MCP, avoiding the broken nested
        # bubblewrap shell path without granting broader filesystem permissions.
        overrides = ['approval_policy="never"', 'mcp_servers={}', 'plugins={}', 'hooks={}', 'notify=[]',
                     'features.apps=false', 'features.plugins=false', 'features.hooks=false',
                     'features.shell_tool=false', 'features.multi_agent=false', 'features.js_repl=false',
                     'features.memories=false', 'web_search="disabled"']
        if workspace:
            server = workspace.server_config(editable=editable)
            overrides += ['mcp_servers.candidate.command=' + json.dumps(server['command']),
                          'mcp_servers.candidate.args=' + json.dumps(server['args']),
                          'mcp_servers.candidate.required=true',
                          'mcp_servers.candidate.startup_timeout_sec=10']
            extra = getattr(workspace, 'cloud_servers', {})
            if isinstance(extra, dict):
                for name, server in extra.get('author' if editable else 'reviewer', {}).items():
                    overrides += [f'mcp_servers.{name}.command=' + json.dumps(server['command']),
                                  f'mcp_servers.{name}.args=' + json.dumps(server['args']),
                                  f'mcp_servers.{name}.required=true', f'mcp_servers.{name}.startup_timeout_sec=10']
        for setting in overrides:
            argv += ['-c', setting]
        if self.oss:
            argv += ['--oss', '--local-provider', self.local_provider]
        argv += ['-m', self.model, '-c', 'model_reasoning_effort=' + json.dumps(self.effort)]
        if schema:
            argv += ['--output-schema', str(schema)]
        argv += ['-o', str(final_path), '-']
        result = processes.run_process(argv, timeout, 'codex', log_path=log_path, input_text=prompt,
                                       cwd=workspace.root if workspace else None)
        try:
            result.output = final_path.read_text(encoding='utf-8')
        except (OSError, UnicodeError) as exc:
            result.ok, result.note = False, f'Cannot read Codex final response: {exc}'
        if log_path and log_path.exists():
            with log_path.open(encoding='utf-8', errors='replace') as stream:
                for line in stream:
                    try:
                        event = json.loads(line)
                        if event.get('type') == 'turn.completed':
                            result.metadata['usage'] = event.get('usage')
                    except (ValueError, AttributeError):
                        continue
        return result


PROFILES = {
    'default': {'implementer': lambda: ClaudeEngine(effort='medium'),
                'reviewer': lambda: CodexEngine(effort='high')},
    'routine': {'implementer': lambda: ClaudeEngine(effort='medium'),
                'reviewer': lambda: CodexEngine(effort='medium')},
    'security': {'implementer': lambda: ClaudeEngine(effort='high'),
                 'reviewer': lambda: CodexEngine(effort='xhigh')},
    'local': {'implementer': lambda: CodexEngine('qwen3.5:4b', oss=True),
              'reviewer': lambda: CodexEngine('qwen3.5:4b', oss=True)},
    'claude-only': {'implementer': lambda: ClaudeEngine(), 'reviewer': lambda: ClaudeEngine(effort='high')},
}
