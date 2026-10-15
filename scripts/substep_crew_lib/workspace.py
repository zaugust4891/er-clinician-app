"""Private candidate snapshots seeded from the user's actual working files."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from . import config, file_tools, patches, protocol


class Candidate:
    def __init__(self, run: Path, guide_path: str):
        self.root = run / 'candidate'
        self.guide_path = guide_path
        self.journal = run / 'candidate-edits.jsonl'
        self.patch_path = run / 'candidate.patch'
        self.root.mkdir()
        entries = patches.checked_git('ls-files', '--cached', '--others', '--exclude-standard', '-z')
        names = {os.fsdecode(x) for x in entries.split(b'\0') if x} | {guide_path}
        for name in sorted(names):
            try:
                source = file_tools.path_in(config.REPO, name)
            except ValueError:
                continue  # credentials, runner state, and build output are never copied
            if not source.exists():
                continue  # preserve existing deletions
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        self.git('init', '-q')
        self.git('config', 'core.hooksPath', '/dev/null')
        self.git('config', 'core.autocrlf', 'false')
        self.checkpoint()

    def git(self, *args, input_text=None):
        return patches.checked_git(*args, repo=self.root, input_text=input_text)

    def checkpoint(self):
        # This index/commit belongs only to the disposable snapshot. Never stage
        # or commit the user's repository, and never follow its hooks/config.
        tracked = self.git('ls-files', '-z').decode('utf-8').split('\0')
        names = sorted({name for name, _ in file_tools.files(self.root)} | {name for name in tracked if name})
        if names:
            self.git('add', '-A', '-f', '--pathspec-from-file=-', '--pathspec-file-nul',
                     input_text='\0'.join(names) + '\0')
        self.git('-c', 'user.name=Candidate snapshot', '-c', 'user.email=candidate@localhost',
                 '-c', 'commit.gpgSign=false', 'commit', '-qm', 'Candidate baseline', '--allow-empty')

    def diff(self) -> str:
        # Include new files even when an edited .gitignore hides them.
        names = [name for name, _ in file_tools.files(self.root)]
        if names:
            self.git('add', '-N', '-f', '--pathspec-from-file=-', '--pathspec-file-nul',
                     input_text='\0'.join(names) + '\0')
        text = self.git('diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--binary', 'HEAD').decode('utf-8')
        self.patch_path.write_text(text, encoding='utf-8')
        return text

    def state(self) -> str:
        import hashlib
        digest = hashlib.sha256()
        for name, path in sorted(file_tools.files(self.root)):
            digest.update(name.encode() + b'\0' + str(path.stat().st_mode).encode() + b'\0' + path.read_bytes())
        return digest.hexdigest()

    def server_config(self, *, editable=False):
        args = ['-B', str(Path(file_tools.__file__).resolve()), '--root', str(self.root), '--logs', str(self.root.parent)]
        if editable:
            args += ['--editable', '--journal', str(self.journal)]
        elif self.patch_path.exists():
            args += ['--patch', str(self.patch_path)]
        return {'command': sys.executable, 'args': args}

    def preflight(self):
        """Exercise the actual read transport once, before paying for any model."""
        cfg = self.server_config()
        messages = [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2024-11-05'}},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call',
             'params': {'name': 'read_file', 'arguments': {'path': self.guide_path, 'line_count': 1}}},
        ]
        result = subprocess.run([cfg['command'], *cfg['args']], input=''.join(json.dumps(x) + '\n' for x in messages),
                                capture_output=True, text=True, timeout=10)
        try:
            responses = [json.loads(line) for line in result.stdout.splitlines()]
            if (result.returncode != 0 or len(responses) != 2
                    or responses[-1]['result'].get('isError')
                    or 'content' not in responses[-1]['result']):
                raise ValueError('File-server response did not pass preflight')
        except (ValueError, KeyError) as exc:
            raise protocol.CrewError('Candidate file access preflight failed; no model was called.') from exc
