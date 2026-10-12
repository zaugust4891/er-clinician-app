"""Harness-owned validation gates and bounded failure-log summaries."""

from __future__ import annotations

import codecs
import io
import json
import re
from dataclasses import dataclass
from pathlib import Path

from . import config, engines, processes


GATE_SETS: dict[str, list[tuple[str, list[str]]]] = {
    "rust": [
        ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        ("clippy", ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        ("test", ["cargo", "test", "--locked", "--workspace"]),
    ],
    "terraform": [
        ("tf-manifest", ["bash", "scripts/tf-manifest-guards.sh"]),
        ("tf-network", ["bash", "scripts/tf-network-check.sh"]),
    ],
    "none": [],
}

GATE_SETS["all"] = GATE_SETS["rust"] + GATE_SETS["terraform"]


@dataclass
class Gate:
    """An actual command result, with its complete transcript stored separately."""

    name: str
    ok: bool
    output: str
    seconds: float
    log_path: Path | None = None


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


def run_gates(names: str, timeout: int, run_dir: Path, round_num: int) -> list[Gate]:
    env = dotenv()
    out: list[Gate] = []
    for invocation, (name, argv) in enumerate(GATE_SETS[names], 1):
        print(f"\n  gate: {name}")
        safe_name = re.sub(r"[^A-Za-z0-9_.-]+", "-", name)
        log_path = run_dir / (
            f"{round_num:02d}-gate-{invocation:02d}-{safe_name}.transcript.log"
        )
        r = processes.run_process(argv, timeout, name, env=env, log_path=log_path)
        out.append(Gate(name, r.ok, r.output, r.seconds, log_path=log_path))
        print(f"  gate: {name} -> {'pass' if r.ok else 'FAIL'} ({r.seconds:.0f}s)")
    return out


GATE_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["failures"],
    "properties": {
        "failures": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["location", "code", "detail"],
                "properties": {
                    "location": {"type": "string", "description": "file:line:col or test name"},
                    "code": {"type": "string", "description": "e.g. E0308, or empty"},
                    "detail": {"type": "string", "description": "the message, one line"},
                },
            },
        }
    },
}


def _schema_file(name: str, obj: dict) -> Path:
    d = config.RUNS / ".schemas"
    d.mkdir(parents=True, exist_ok=True)
    f = d / f"{name}.json"
    f.write_text(json.dumps(obj, indent=2), encoding="utf-8")
    return f


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


def _valid_gate_fallback(raw: str) -> list[dict[str, str]]:
    try:
        value = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return []
    if not isinstance(value, dict) or set(value) != {"failures"}:
        return []
    failures = value["failures"]
    if not isinstance(failures, list):
        return []
    valid: list[dict[str, str]] = []
    for item in failures[:MAX_DIAGNOSTICS]:
        if (
            isinstance(item, dict)
            and set(item) == {"location", "code", "detail"}
            and all(isinstance(item[key], str) for key in item)
        ):
            valid.append(item)
        else:
            return []
    return valid


def _fit_summary(text: str, budget: int, reference: str = "") -> str:
    if len(text) <= budget:
        return text
    notice = f"\n[...summary omitted; complete transcript: {reference}...]" if reference else "\n[...summary omitted...]"
    if len(notice) >= budget:
        return notice[:budget]
    return text[: budget - len(notice)] + notice


def condense(
    gates: list[Gate],
    engine: engines.Engine,
    budget: int = 4000,
    *,
    helper_log_path: Path | None = None,
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
                fallback_prompt = (
                    "Extract distinct failures into the supplied schema. Use only literal log "
                    "evidence.\n\n--- bounded log excerpt ---\n"
                    + g.output[:MODEL_FALLBACK_CHARS]
                )
                helper_path = helper_log_path
                if helper_path is not None:
                    safe_name = re.sub(r"[^A-Za-z0-9_.-]+", "-", g.name)
                    base = helper_path.name.removesuffix(".transcript.log")
                    helper_path = helper_path.with_name(
                        f"{base}-{gate_index:02d}-{safe_name}.transcript.log"
                    )
                result = engine.run(
                    fallback_prompt,
                    timeout=300,
                    schema=_schema_file("gate", GATE_SCHEMA),
                    log_path=helper_path,
                )
                items = _valid_gate_fallback(result.output) if result.ok else []
                if items:
                    body = "\n".join(
                        _fit_summary(
                            f"- {item['location']}"
                            f"{'  [' + item['code'] + ']' if item['code'] else ''}"
                            f"  {item['detail']}",
                            MAX_DIAGNOSTIC_CHARS,
                        )
                        for item in items
                    )
                else:
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
