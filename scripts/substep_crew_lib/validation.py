"""Harness-owned validation gates and bounded failure-log summaries."""

from __future__ import annotations

import codecs
import io
import re
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path

from . import config, processes, protocol


GATE_SETS: dict[str, list[tuple[str, list[str]]]] = {
    "rust": [
        ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        ("clippy", ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        ("test", ["cargo", "test", "--locked", "--workspace"]),
    ],
    "terraform": [
        ("tf-manifest", ["bash", "scripts/tf-manifest-guards.sh"]),
        ("tf-network", ["bash", "scripts/tf-network-check.sh"]),
        ("tf-data-identity", ["bash", "scripts/tf-data-check.sh"]),
        ("db-secrets", ["python3", "-m", "unittest", "scripts/test_db_secrets.py"]),
    ],
    "none": [],
    "python": [("python", ["python3", "-B", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py"])],
    "auto": [],
}

GATE_SETS["all"] = GATE_SETS["rust"] + GATE_SETS["terraform"] + GATE_SETS["python"]


@dataclass
class Gate:
    """An actual command result, with its complete transcript stored separately."""

    name: str
    ok: bool
    output: str
    seconds: float
    log_path: Path | None = None
    environment_problem: bool = False


def dotenv() -> dict[str, str]:
    """cargo test --workspace needs DATABASE_URL from .env."""
    env: dict[str, str] = {}
    f = config.REPO / ".env"
    if f.exists():
        for line in f.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, _, v = line.partition("=")
                env[k.strip()] = v.strip().strip('"').strip("'")
    return env


def select_gates(names: str, paths, *, final: bool, root: Path) -> list[tuple[str, list[str]]]:
    """Path-driven intermediate checks; final checks include explicitly required sets."""
    if names == 'none':
        return []
    paths = set(paths)
    rust = any(p.endswith('.rs') or p in {'Cargo.toml', 'Cargo.lock', 'Dockerfile'}
               or p.startswith('.sqlx/') or p.endswith('/Cargo.toml') for p in paths)
    terraform = any(p.startswith('infra/') or p.startswith('scripts/tf-') for p in paths)
    python = any(p.endswith('.py') for p in paths)
    selected = dict(GATE_SETS[names] if final and names != 'auto' else [])
    if rust:
        rust_gates = GATE_SETS['rust']
        packages = set()
        if not final and all(p.startswith('crates/') for p in paths if not p.endswith('.md')):
            for p in paths:
                parts = Path(p).parts
                if len(parts) >= 3 and parts[0] == 'crates':
                    manifest = root / 'crates' / parts[1] / 'Cargo.toml'
                    if manifest.exists():
                        packages.add(tomllib.loads(manifest.read_text())['package']['name'])
        for name, argv in rust_gates:
            argv = list(argv)
            if packages and '--workspace' in argv:
                pos = argv.index('--workspace')
                argv[pos:pos + 1] = [arg for package in sorted(packages) for arg in ['-p', package]]
            selected[name] = argv
    if terraform:
        selected.update(GATE_SETS['terraform'])
    if python:
        selected.update(GATE_SETS['python'])
    # Also covers documentation-only and unknown file types without a compiler run.
    return [('diff-check', ['git', 'diff', '--check', 'HEAD']), *selected.items()]


def run_gates(names: str, timeout: int, run_dir: Path, round_num: int, *, on_gate=None,
              cwd: Path | None = None, commands=None, deadline: float | None = None) -> list[Gate]:
    env = dotenv()
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    if cwd is not None:
        # Reuse compiler artifacts without copying them into the candidate diff.
        env['CARGO_TARGET_DIR'] = str(config.REPO / 'target')
    out: list[Gate] = []
    for invocation, (name, argv) in enumerate(commands if commands is not None else GATE_SETS[names], 1):
        remaining = timeout if deadline is None else min(timeout, deadline - time.monotonic())
        if remaining <= 0:
            raise protocol.CrewError('Run time budget exhausted; candidate retained for resume')
        if on_gate is not None:
            on_gate(name, 'running')
        print(f"\n  gate: {name}")
        safe_name = re.sub(r"[^A-Za-z0-9_.-]+", "-", name)
        log_path = run_dir / (
            f"{round_num:02d}-gate-{invocation:02d}-{safe_name}.transcript.log"
        )
        r = processes.run_process(argv, remaining, name, env=env, log_path=log_path, cwd=cwd)
        unavailable = not r.ok and any(text in r.output for text in (
            'error communicating with database: Connection refused', 'command not found',
            'Failed to query available provider packages', 'could not connect to server: Connection refused',
        ))
        out.append(Gate(name, r.ok, r.output, r.seconds, log_path=log_path, environment_problem=unavailable))
        if on_gate is not None:
            on_gate(name, 'pass' if r.ok else 'FAIL')
        print(f"  gate: {name} -> {'pass' if r.ok else 'FAIL'} ({r.seconds:.0f}s)")
        if not r.ok:
            break  # repair the first failing prerequisite before expensive later gates
    return out


FAILURE_PATTERNS = [
    re.compile(r"^(error(?:\[E\d+\])?|warning):\s*(?P<msg>.+)$"),
    re.compile(r"^\s*--> (?P<loc>\S+:\d+:\d+)\s*$"),
    re.compile(r"^(?P<loc>\S+) \.\.\. FAILED$"),
    re.compile(r"^\s{4}(?P<loc>[\w:]+::[\w:]+)$"),
    re.compile(r"^thread '.*' panicked at (?P<loc>\S+):"),
    re.compile(r"^Diff in (?P<loc>\S+?):\d+:$"),
    re.compile(r"^(?:Error|╷|│ Error): (?P<msg>.+)$"),
]

MAX_DIAGNOSTICS = 60

MAX_DIAGNOSTIC_CHARS = 1000

MAX_INPUT_LINE_CHARS = 2048

MODEL_FALLBACK_CHARS = 6000


def _bounded_lines_from_stream(
    stream: io.BufferedReader, line_limit: int = MAX_INPUT_LINE_CHARS
):
    """Yield UTF-8 lines while bounding partial-line state in characters."""
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    partial = ""
    discarded = False
    while True:
        chunk = stream.read(processes.STREAM_READ_BYTES)
        text = decoder.decode(chunk, final=not chunk)
        cursor = 0
        while True:
            newline = text.find("\n", cursor)
            if newline < 0:
                fragment = text[cursor:]
                if not discarded:
                    room = line_limit - len(partial)
                    partial += fragment[: max(0, room)]
                    discarded = len(fragment) > max(0, room)
                break
            fragment = text[cursor:newline]
            if not discarded:
                room = line_limit - len(partial)
                partial += fragment[: max(0, room)]
                discarded = len(fragment) > max(0, room)
            yield partial.rstrip("\r") + (" […line truncated…]" if discarded else "")
            partial = ""
            discarded = False
            cursor = newline + 1
        if not chunk:
            if partial or discarded:
                yield partial.rstrip("\r") + (" […line truncated…]" if discarded else "")
            return


def _bounded_text_lines(text: str):
    with io.BytesIO(text.encode("utf-8")) as stream:
        yield from _bounded_lines_from_stream(stream)


def _emit_failure(
    entry: str,
    out: list[str],
    seen: set[str],
    max_diagnostics: int,
) -> bool:
    entry = entry[:MAX_DIAGNOSTIC_CHARS]
    key = entry.lower()
    if key in seen:
        return False
    if len(out) >= max_diagnostics:
        return True
    seen.add(key)
    out.append(entry)
    return False


def _extract_failures(
    lines, max_diagnostics: int = MAX_DIAGNOSTICS
) -> list[str]:
    """Incrementally extract bounded, ordered diagnostics.

    The final omission marker deliberately avoids claiming an exact distinct
    count: uniqueness is tracked only for retained diagnostics.
    """
    out: list[str] = []
    seen: set[str] = set()
    pending: str | None = None
    omitted = False

    def emit(entry: str) -> None:
        nonlocal omitted
        omitted = _emit_failure(entry, out, seen, max_diagnostics) or omitted

    for raw_line in lines:
        line = str(raw_line)[:MAX_INPUT_LINE_CHARS].rstrip()
        header = FAILURE_PATTERNS[0].match(line)
        if header and not line.startswith("warning:"):
            if pending is not None:
                emit(f"- {pending}")
            pending = line.strip()[:MAX_DIAGNOSTIC_CHARS - 2]
            continue
        loc = None
        for pattern in FAILURE_PATTERNS[1:]:
            match = pattern.match(line)
            if match:
                loc = match.groupdict().get("loc") or match.groupdict().get("msg")
                break
        if loc:
            loc = loc.replace(f"{config.REPO}/", "")
            emit(f"- {loc}  {pending or ''}".rstrip())
            pending = None
        elif pending and not line.strip():
            emit(f"- {pending}")
            pending = None
        if omitted:
            break
    if pending is not None and not omitted:
        emit(f"- {pending}")
    if omitted:
        out.append("- [...additional distinct diagnostics omitted; see complete transcript...]")
    return out


def _fit_summary(text: str, budget: int, reference: str = "") -> str:
    if len(text) <= budget:
        return text
    notice = f"\n[...summary omitted; complete transcript: {reference}...]" if reference else "\n[...summary omitted...]"
    if len(notice) >= budget:
        return notice[:budget]
    return text[: budget - len(notice)] + notice


def condense(
    gates: list[Gate],
    budget: int = 4000,
) -> str:
    """Return a gate summary bounded by *budget* Unicode characters."""
    if budget <= 0:
        return ""
    parts: list[str] = []
    used = 0
    processed = 0
    for gate_index, g in enumerate(gates, 1):
        has_log = g.log_path is not None and g.log_path.exists()
        reference = str(g.log_path) if has_log else ""
        if g.ok:
            section = f"### {g.name}: pass ({g.seconds:.0f}s)"
        else:
            if has_log:
                with g.log_path.open("rb") as stream:
                    found = _extract_failures(_bounded_lines_from_stream(stream))
            else:
                found = _extract_failures(_bounded_text_lines(g.output))
            body = "\n".join(found)
            if not found:
                body = _fit_summary(g.output, min(MODEL_FALLBACK_CHARS, budget), reference)
            log_note = f"\nComplete transcript: {reference}" if reference else "\nNo complete transcript was saved; using retained excerpt."
            section = f"### {g.name}: FAIL ({g.seconds:.0f}s)\n{body}{log_note}"
        separator = "\n\n" if parts else ""
        remaining = budget - used - len(separator)
        if remaining <= 0:
            break
        fitted = _fit_summary(section, remaining, reference)
        parts.append(fitted)
        processed += 1
        used += len(separator) + len(fitted)
        if len(fitted) < len(section):
            break
    summary = "\n\n".join(parts)
    if processed < len(gates):
        notice = f"\n\n[...{len(gates) - processed} additional gate summaries omitted by the {budget}-character budget...]"
        if len(notice) >= budget:
            return notice[:budget]
        summary = summary[: budget - len(notice)] + notice
    return summary
