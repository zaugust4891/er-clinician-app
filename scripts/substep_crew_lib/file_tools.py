"""Small stdio MCP server: bounded file tools confined to one candidate tree.

No shell tool, arbitrary command, symlink traversal, or access to runner metadata.
Run this trusted module from the source checkout, never from candidate code.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
from pathlib import Path
import re
import sys

MAX_FILE = 1_000_000
MAX_REPLY = 24_000
PRIVATE = {'.git', '.agents', '.codex', '.claude', '.aws', '__pycache__', 'target', '.terraform'}


def path_in(root: Path, name: str) -> Path:
    relative = Path(name)
    if (not name or relative.is_absolute() or str(relative) != name
            or '\\' in name or any(p in PRIVATE or p in {'.', '..'} for p in relative.parts)
            or any(p.startswith('.env') and p != '.env.example' for p in relative.parts)):
        raise ValueError(f'Unsupported candidate path: {name!r}')
    path = root / relative
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Path escapes candidate workspace')
    for parent in [path, *path.parents]:
        if parent == root:
            break
        if parent.is_symlink():
            raise ValueError('Symlinks are unsupported')
    if path.exists() and not path.is_file():
        raise ValueError('Expected a regular file')
    return path


def files(root: Path):
    for directory, dirs, names in os.walk(root, followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in PRIVATE and not (Path(directory) / d).is_symlink())
        for name in sorted(names):
            relative = (Path(directory) / name).relative_to(root).as_posix()
            try:
                yield relative, path_in(root, relative)
            except ValueError:
                continue


def read_text(path: Path) -> str:
    if path.stat().st_size > MAX_FILE:
        raise ValueError('File exceeds 1 MiB')
    text = path.read_bytes().decode('utf-8')
    if '\x00' in text:
        raise ValueError('Binary files are unsupported')
    return text


def log_range(path: Path, start: int, count: int) -> dict:
    """Read large transcripts incrementally; neither long lines nor files fill RAM."""
    selected, used, number = [], 0, 0
    with path.open('r', encoding='utf-8', errors='replace') as stream:
        while True:
            line = stream.readline(MAX_REPLY + 1)
            if not line:
                return {'text': '\n'.join(selected), 'total_lines': number, 'next_line': None}
            number += 1
            truncated = len(line) > MAX_REPLY
            tail = line
            while tail and not tail.endswith('\n'):
                tail = stream.readline(MAX_REPLY + 1)
            if number < start:
                continue
            text = f'{number}: {line.rstrip()}'
            if truncated or len(text) > MAX_REPLY:
                text = text[:MAX_REPLY - 100] + ' [...long log line truncated...]'
            if used + len(text) > MAX_REPLY:
                return {'text': '\n'.join(selected), 'total_lines': None, 'next_line': number}
            selected.append(text)
            used += len(text) + 1
            if len(selected) == count:
                return {'text': '\n'.join(selected), 'total_lines': None,
                        'next_line': number + 1 if stream.read(1) else None}


def schema(properties, required=()):
    return {'type': 'object', 'properties': properties, 'required': list(required), 'additionalProperties': False}


S = {'type': 'string'}
TOOLS = [
    ('read_file', 'Read numbered lines. @patch is the review diff; @checks/NAME reads a saved gate transcript.',
     schema({'path': S, 'start_line': {'type': 'integer', 'minimum': 1},
             'line_count': {'type': 'integer', 'minimum': 1, 'maximum': 300}}, ['path'])),
    ('list_files', 'List candidate paths matching a glob, optionally after a path for pagination.',
     schema({'pattern': S, 'after': S})),
    ('search', 'Find a literal string in candidate text; bounded results, optionally after a path.',
     schema({'query': S, 'pattern': S, 'after': S}, ['query'])),
    ('edit_file', 'Replace one uniquely matching literal block. Read the file first.',
     schema({'path': S, 'old': S, 'new': S}, ['path', 'old', 'new'])),
    ('write_file', 'Write UTF-8 candidate contents. Use edit_file for small changes.',
     schema({'path': S, 'content': S}, ['path', 'content'])),
    ('delete_file', 'Delete one regular candidate file.', schema({'path': S}, ['path'])),
]


class FileTools:
    def __init__(self, root: Path, *, editable=False, journal: Path | None = None,
                 patch: Path | None = None, logs: Path | None = None):
        self.root, self.editable, self.journal, self.patch = root.resolve(), editable, journal, patch
        self.logs = logs

    def definitions(self):
        return [{'name': name, 'description': description, 'inputSchema': spec,
                 'annotations': {'readOnlyHint': name in {'read_file', 'list_files', 'search'}}}
                for name, description, spec in TOOLS if self.editable or name in {'read_file', 'list_files', 'search'}]

    def call(self, name: str, args: dict):
        definitions = {item['name']: item for item in self.definitions()}
        if name not in definitions:
            raise ValueError('Tool unavailable in this session')
        # MCP arguments are untrusted even when the caller advertises a schema.
        spec = definitions[name]['inputSchema']
        if not isinstance(args, dict) or set(args) - set(spec['properties']) or set(spec['required']) - set(args):
            raise ValueError('Invalid tool arguments')
        for key, value in args.items():
            kind = spec['properties'][key]['type']
            if (kind == 'string' and not isinstance(value, str)) or (kind == 'integer' and type(value) is not int):
                raise ValueError('Invalid tool argument type')
        if name in {'list_files', 'search'}:
            result = []
            used = 0
            for relative, path in sorted(files(self.root)):
                if relative <= args.get('after', '') or not fnmatch.fnmatch(relative, args.get('pattern', '*')):
                    continue
                if name == 'list_files':
                    matches = iter([relative])
                else:
                    if not args['query']:
                        raise ValueError('Search query must not be empty')
                    try:
                        matches = (f'{relative}:{i}: {line[:500]}' for i, line in enumerate(read_text(path).splitlines(), 1)
                                   if args['query'] in line)
                    except (ValueError, UnicodeError):
                        continue
                for match in matches:
                    if len(result) >= 100 or used + len(match) > MAX_REPLY:
                        return {'matches': result, 'truncated': True, 'next_after': relative,
                                'hint': 'Use read_file or a narrower query for more matches within this file.'}
                    result.append(match)
                    used += len(match) + 1
            return {'matches': result, 'truncated': False}
        if args['path'].startswith('@'):
            if name != 'read_file':
                raise ValueError('Runner artifacts are read-only')
            if args['path'] == '@patch' and self.patch:
                path = self.patch
            elif self.logs and re.fullmatch(r'@checks/\d+-gate-\d+-[\w.-]+\.transcript\.log', args['path']):
                path = self.logs / args['path'].split('/', 1)[1]
                if path.is_symlink():
                    raise ValueError('Symlinks are unsupported')
            else:
                raise ValueError('Unknown runner artifact')
        else:
            path = path_in(self.root, args['path'])
        if name == 'read_file':
            start, count = args.get('start_line', 1), args.get('line_count', 200)
            if start < 1 or not 1 <= count <= 300:
                raise ValueError('Invalid line range')
            if args['path'].startswith('@checks/'):
                return {'path': args['path'], **log_range(path, start, count)}
            lines = read_text(path).splitlines()
            selected, used = [], 0
            for i in range(start - 1, min(len(lines), start - 1 + count)):
                line = f'{i + 1}: {lines[i]}'
                if used + len(line) > MAX_REPLY:
                    if not selected:
                        raise ValueError('Line exceeds response budget; use search for a bounded excerpt')
                    break
                selected.append(line)
                used += len(line) + 1
            return {'path': args['path'], 'text': '\n'.join(selected), 'total_lines': len(lines),
                    'next_line': start + len(selected) if start + len(selected) <= len(lines) else None}
        if name == 'delete_file':
            path.unlink()
        else:
            if name == 'edit_file':
                original = read_text(path)
                if not args['old'] or original.count(args['old']) != 1:
                    raise ValueError('old must match exactly once; read current contents and retry this edit')
                content = original.replace(args['old'], args['new'], 1)
            else:
                content = args['content']
            raw = content.encode('utf-8')
            if len(raw) > MAX_FILE or b'\x00' in raw:
                raise ValueError('Only UTF-8 text up to 1 MiB is supported')
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(raw)
        if self.journal:
            with self.journal.open('a', encoding='utf-8') as output:
                output.write(json.dumps({'path': args['path'], 'operation': name}) + '\n')
        return {'changed': args['path']}


def serve(server: FileTools, source=sys.stdin, destination=sys.stdout):
    for line in source:
        request = None
        try:
            if len(line) > 2_000_000:
                raise ValueError('Request exceeds limit')
            request = json.loads(line)
            method, params = request.get('method'), request.get('params', {})
            if 'id' not in request:
                continue
            if method == 'initialize':
                result = {'protocolVersion': params.get('protocolVersion', '2024-11-05'),
                          'capabilities': {'tools': {}}, 'serverInfo': {'name': 'candidate-files', 'version': '1.0'}}
            elif method == 'ping':
                result = {}
            elif method == 'tools/list':
                result = {'tools': server.definitions()}
            elif method == 'tools/call':
                try:
                    value = server.call(params['name'], params.get('arguments', {}))
                    result = {'content': [{'type': 'text', 'text': json.dumps(value, ensure_ascii=False)}]}
                except (OSError, ValueError, KeyError, TypeError) as exc:
                    result = {'isError': True, 'content': [{'type': 'text', 'text': str(exc)}]}
            else:
                raise ValueError(f'Unsupported method: {method}')
            response = {'jsonrpc': '2.0', 'id': request['id'], 'result': result}
        except (ValueError, TypeError, AttributeError) as exc:
            response = {'jsonrpc': '2.0', 'id': request.get('id') if isinstance(request, dict) else None,
                        'error': {'code': -32600, 'message': str(exc)}}
        destination.write(json.dumps(response, ensure_ascii=False) + '\n')
        destination.flush()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--editable', action='store_true')
    parser.add_argument('--journal', type=Path)
    parser.add_argument('--patch', type=Path)
    parser.add_argument('--logs', type=Path)
    options = parser.parse_args()
    serve(FileTools(options.root, editable=options.editable, journal=options.journal, patch=options.patch, logs=options.logs))
