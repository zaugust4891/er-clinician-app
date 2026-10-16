"""Human-readable stage reports and read-only monitoring of saved progress."""

from __future__ import annotations

from contextlib import AbstractContextManager
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import sys
import threading
import time

from . import config, protocol


STAGES = {
    'starting': (0, 'Starting'),
    'proposal': (1, 'Edit candidate'),
    'patch-review': (2, 'Review edit'),
    'apply': (4, 'Publish reviewed edit'),
    'validation': (5, 'Validate current files'),
    'ready': (5, 'Complete'),
    'cloud': (3, 'Review or execute cloud operation'),
    'blocked': (3, 'Waiting for external action'),
    # Read-only monitoring remains compatible with snapshots from older runs.
    'proposal-repair': (1, 'Repair proposal'),
    'context': (2, 'Provide review context'),
    'application': (3, 'Confirm reviewed edit'),
}
TERMINAL = {'ready', 'failed', 'interrupted', 'blocked'}


def now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec='seconds')


def process_namespace() -> str | None:
    try:
        return os.readlink('/proc/self/ns/pid')
    except OSError:
        return None


def can_check_pid(state: dict) -> bool:
    recorded, current = state.get('pid_namespace'), process_namespace()
    return os.name == 'posix' and (recorded is None or current is None or recorded == current)


def summary(value: str, limit: int = 900) -> str:
    """Keep model-provided prose bounded and remove terminal control sequences."""
    value = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', str(value))
    value = ''.join(char for char in value if char.isprintable() or char.isspace())
    value = ' '.join(value.split())
    return value if len(value) <= limit else value[:limit - 3] + '...'


def elapsed(seconds: float) -> str:
    minutes, seconds = divmod(max(0, int(seconds)), 60)
    hours, minutes = divmod(minutes, 60)
    return f'{hours:d}:{minutes:02d}:{seconds:02d}' if hours else f'{minutes:02d}:{seconds:02d}'


def bar(state: dict) -> str:
    number = STAGES[state['stage']][0]
    filled = 20 if state['status'] == 'ready' else max(0, number - 1) * 4
    marker = '>' if filled < 20 else ''
    return '[' + '=' * filled + marker + '-' * (20 - filled - len(marker)) + ']'


def render(state: dict) -> str:
    number, label = STAGES[state['stage']]
    lines = [
        f"{state['status'].upper()} | Phase {state['phase']} / Step {state['step']} / {state['substep']} — {summary(state['title'])}",
        f"Current point: {label}" + (f" — {summary(state['detail'])}" if state.get('detail') else ''),
        f"Progress: {bar(state)} stage {number}/5 of this edit | {state['applied_edits']} edits applied",
        (f"Current edit #{state['edit_number']}: " if state['edit_number'] else 'Current files: ')
        + summary(state['edit_summary']),
        f"Current review: {summary(state['review_summary'])}",
    ]
    if state.get('files'):
        files = state['files']
        lines.append('Files: ' + ', '.join(summary(name, 160) for name in files[:8])
                     + (f" (+{len(files) - 8} more)" if len(files) > 8 else ''))
    if state.get('gates'):
        lines.append('Validation: ' + ' | '.join(f'{name}: {value}' for name, value in state['gates'].items()))
    lines.append(f"Elapsed: stage {elapsed(state['stage_seconds'])} | run {elapsed(state['run_seconds'])}")
    updated = datetime.fromisoformat(state['updated_at'])
    lines.append(f"Updated: {updated.astimezone().strftime('%Y-%m-%d %H:%M:%S %Z')} | run: {state['run_dir']}")
    if state['status'] == 'running':
        age = (datetime.now(timezone.utc) - updated).total_seconds()
        if age >= 15:
            lines.append(f'Heartbeat: no update for {elapsed(age)}; check the runner process.')
    if state.get('error'):
        lines.append('Error: ' + summary(state['error'], 2000))
    return '\n'.join(lines)


class Reporter(AbstractContextManager):
    """Persist snapshots atomically; print stage summaries and periodic heartbeats."""

    def __init__(self, run_dir: Path, phase: int, step: int, substep: str, title: str,
                 *, applied_edits: int = 0, interval: float = 5, stream=None):
        self.run_dir = run_dir
        self.interval = interval
        self.stream = stream if stream is not None else sys.stdout
        self.lock = threading.RLock()
        self.stop_event = threading.Event()
        self.thread = None
        self.started = self.stage_started = time.monotonic()
        self.last_console_heartbeat = self.started
        self.state = {
            'version': 1, 'pid': os.getpid(), 'pid_namespace': process_namespace(), 'status': 'running',
            'phase': phase, 'step': step, 'substep': substep, 'title': title,
            'run_dir': str(run_dir), 'started_at': now(), 'updated_at': now(),
            'stage': 'starting', 'detail': '', 'edit_number': applied_edits + 1,
            'applied_edits': applied_edits, 'edit_summary': 'Planning the next coherent change.',
            'review_summary': 'Waiting for a proposal.', 'files': [], 'gates': {},
            'stage_seconds': 0, 'run_seconds': 0, 'error': '',
        }

    def _save(self):
        self.state['updated_at'] = now()
        self.state['stage_seconds'] = time.monotonic() - self.stage_started
        self.state['run_seconds'] = time.monotonic() - self.started
        for filename, content in (
            ('progress.json', json.dumps(self.state, indent=2, ensure_ascii=False)),
            ('progress.md', render(self.state) + '\n'),
        ):
            temporary = self.run_dir / (filename + '.tmp')
            temporary.write_text(content, encoding='utf-8')
            temporary.replace(self.run_dir / filename)

    def update(self, stage: str | None = None, *, restart: bool = False, **changes):
        with self.lock:
            if stage is not None:
                if stage not in STAGES:
                    raise ValueError(f'unknown progress stage: {stage}')
                if stage != self.state['stage'] or restart:
                    self.stage_started = time.monotonic()
                self.state['stage'] = stage
            self.state.update(changes)
            self._save()
            report = render(self.state)
            if self.stream.isatty():
                self.stream.write('\r\x1b[2K')
            print('\n' + report, file=self.stream, flush=True)
            with (self.run_dir / 'progress.log').open('a', encoding='utf-8') as output:
                output.write(report + '\n\n')
            with (self.run_dir / 'events.jsonl').open('a', encoding='utf-8') as output:
                output.write(json.dumps(self.state, ensure_ascii=False) + '\n')

    def heartbeat(self):
        with self.lock:
            if self.state['status'] in TERMINAL:
                return
            self._save()
            tty = self.stream.isatty()
            if tty or time.monotonic() - self.last_console_heartbeat >= 30:
                label = STAGES[self.state['stage']][1]
                text = (f"{bar(self.state)} {label} | stage {elapsed(self.state['stage_seconds'])}"
                        f" | {self.state['applied_edits']} edits applied | runner active")
                print(('\r\x1b[2K' if tty else '') + text,
                      end='' if tty else '\n', file=self.stream, flush=True)
                self.last_console_heartbeat = time.monotonic()

    def _worker(self):
        while not self.stop_event.wait(self.interval):
            try:
                self.heartbeat()
            except OSError:
                # A monitoring-disk failure must not kill the model process or
                # obscure the main thread's normal artifact/error handling.
                return

    def __enter__(self):
        self.update()
        self.thread = threading.Thread(target=self._worker, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, exc_type, exc, traceback):
        self.stop_event.set()
        if self.thread is not None:
            self.thread.join(timeout=2)
        if exc is not None:
            self.update(status='interrupted' if isinstance(exc, KeyboardInterrupt) else 'failed', error=str(exc))
        return False


def resolve_run(value: str, *, require_progress: bool = False) -> Path:
    root = config.RUNS.resolve()
    if value == 'latest':
        candidates = sorted((path for path in root.iterdir() if path.is_dir() and not path.is_symlink() and not path.name.startswith('.')),
                            key=lambda path: path.name, reverse=True) if root.exists() else []
        for path in candidates:
            if not require_progress or (path / 'progress.json').is_file():
                return path.resolve()
        raise protocol.CrewError('No matching run found. Start the runner to create progress reports.')
    path = Path(value).expanduser()
    if not path.is_absolute():
        path = config.REPO / path
        if not path.is_dir():
            path = root / value
    path = path.resolve()
    if not path.is_relative_to(root) or path == root or not path.is_dir():
        raise protocol.CrewError('Run directory must exist inside .agents/runs/.')
    return path


def monitor(value: str, *, watch: bool = False) -> int:
    run_dir = resolve_run(value, require_progress=True)
    try:
        while True:
            path = run_dir / 'progress.json'
            if not path.exists():
                raise protocol.CrewError('This older run has no progress report. Resume it to enable monitoring.')
            state = protocol.load_json(path.read_text(encoding='utf-8'))
            if (state.get('status') == 'running' and isinstance(state.get('pid'), int)
                    and state['pid'] > 0 and can_check_pid(state)):
                try:
                    os.kill(state['pid'], 0)
                except ProcessLookupError:
                    state['status'] = 'interrupted'
                    state['error'] = 'The runner process is no longer running. Resume this run to continue.'
                except PermissionError:
                    pass
            if watch and sys.stdout.isatty():
                sys.stdout.write('\x1b[2J\x1b[H')
            print(render(state), flush=True)
            if not watch or state['status'] in TERMINAL:
                return 0
            time.sleep(2)
    except KeyboardInterrupt:
        return 0
