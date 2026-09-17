#!/usr/bin/env python3
"""
substep_crew.py — implement/review/fix loop for one phase substep.

Mirrors the manual workflow in docs/planning/prompt_templates.txt. The default
``gates-first`` policy avoids semantic review until required gates pass;
``--review-policy always`` restores review on every validation round.

    PROMPT 1 (implement)  ->  deterministic gates  ->  PROMPT 2 (review)
                                      ^                      |
                                      |                   NOT_READY
                                      +------ fix <---------- +

Roles and who runs them:

    implementer  claude -p            writes code   (acceptEdits)
    reviewer     codex exec           read-only sandbox, emits a JSON verdict
    fixer        claude -p            writes code   (acceptEdits)
    condenser    ollama (local HTTP)  squeezes gate logs, no tools
    triage       ollama (local HTTP)  oscillation detection, no tools

The cross-model split is deliberate: the reviewer is a different model family
from the author, and the gates (fmt/clippy/test) are run by THIS script rather
than by an agent claiming it ran them.

Nothing here commits, pushes, or stages. The final staging decision stays yours.

Each noisy invocation stores a complete ``*.transcript.log`` and retains only
a bounded in-memory excerpt. Model calls also save a complete ``*.final.*``
response. Excerpt and summary limits are Unicode character counts, not tokens.

Claude invocations honor ``CLAUDE_CONFIG_DIR``. When it is unset, the harness
uses ``~/.claude-personal`` if that directory exists, otherwise ``~/.claude``;
``PAI_DIR`` is kept aligned so hooks load from the same Claude home.

On POSIX, child processes run in a separate session. Timeout or interruption
terminates the whole process group, escalating to SIGKILL after a short grace.
Process-group cleanup is POSIX-only; non-POSIX child signaling is best-effort,
and selector support for subprocess pipes is platform-dependent.

Usage:
    scripts/substep_crew.py --dry-run                 # resolve substep, print prompts
    scripts/substep_crew.py                           # next substep, auto-detected
    scripts/substep_crew.py --substep S5 --step 8
    scripts/substep_crew.py --profile local           # free harness test on Ollama
    scripts/substep_crew.py --gates terraform --max-rounds 4
"""

from __future__ import annotations

import argparse
import codecs
from collections import deque
from contextlib import nullcontext
import difflib
import io
import json
import os
import re
import selectors
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RUNS = REPO / ".agents" / "runs"
OLLAMA = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434")


def default_claude_config_dir() -> Path:
    """Prefer the personal Claude home when present; honor an explicit override."""
    configured = os.environ.get("CLAUDE_CONFIG_DIR")
    if configured:
        return Path(configured).expanduser()
    personal = Path.home() / ".claude-personal"
    return personal if personal.is_dir() else Path.home() / ".claude"


# --------------------------------------------------------------------------
# guide parsing — the phase docs are the source of truth for "what is next"
# --------------------------------------------------------------------------

SUBSTEP_RE = re.compile(r"^##\s+S(\d+)\s*[—–-]\s*(.+?)\s*$", re.M)
RECORD_RE = re.compile(r"^##\s+Implementation record\s*[—–-]\s*S(\d+)", re.M | re.I)
COMMIT_RE = re.compile(r"phase\s*(\d+)\s*[-–—]?\s*step\s*(\d+)\s*[-–—:]?\s*s(\d+)", re.I)


@dataclass
class Substep:
    id: str  # "S5"
    num: int
    title: str
    line: int  # 1-indexed heading line
    text: str  # heading through the next "## "


@dataclass
class Guide:
    path: Path
    phase: int
    step: int
    substeps: list[Substep]
    recorded: set[int]  # substeps with an "Implementation record" section

    @property
    def rel(self) -> str:
        return str(self.path.relative_to(REPO))

    def get(self, sid: str) -> Substep:
        want = int(sid.lstrip("Ss"))
        for s in self.substeps:
            if s.num == want:
                return s
        raise SystemExit(f"{sid} not found in {self.rel}")


def parse_guide(path: Path, phase: int, step: int, body: str | None = None) -> Guide:
    body = path.read_text(encoding="utf-8") if body is None else body
    marks = [(m.start(), int(m.group(1)), m.group(2)) for m in SUBSTEP_RE.finditer(body)]
    substeps: list[Substep] = []
    previous_offset = 0
    line = 1
    for i, (off, num, title) in enumerate(marks):
        line += body.count("\n", previous_offset, off)
        previous_offset = off
        end = marks[i + 1][0] if i + 1 < len(marks) else len(body)
        # stop at the next top-level "## " that is not a substep
        nxt = re.search(r"^## (?!S\d+\s*[—–-])", body[off + 3 : end], re.M)
        if nxt:
            end = off + 3 + nxt.start()
        substeps.append(
            Substep(
                id=f"S{num}",
                num=num,
                title=title,
                line=line,
                text=body[off:end].rstrip(),
            )
        )
    recorded = {int(m.group(1)) for m in RECORD_RE.finditer(body)}
    return Guide(path=path, phase=phase, step=step, substeps=substeps, recorded=recorded)


def find_guide(
    phase: int, step: int, *, include_body: bool = False
) -> Path | tuple[Path, str]:
    plan = REPO / "docs" / "planning"
    for pat in (
        f"phase{phase}-step{step}-implementation-steps.md",
        f"phase{phase}-step{step}-*.md",
        f"phase{phase}.md",
    ):
        hits = sorted(plan.glob(pat))
        for h in hits:
            body = h.read_text(encoding="utf-8")
            if SUBSTEP_RE.search(body):
                return (h, body) if include_body else h
    raise SystemExit(f"no guide with '## S<n>' headings for phase {phase} step {step} in {plan}")


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO, capture_output=True, text=True, check=False
    ).stdout.strip()


def detect_phase_step(history: list[str] | None = None) -> tuple[int, int]:
    """Read the most recent PHASE<n> STEP<m> S<k> commit subject."""
    lines = history if history is not None else git("log", "--pretty=%s", "-40").splitlines()
    for line in lines[:40]:
        m = COMMIT_RE.search(line)
        if m:
            return int(m.group(1)), int(m.group(2))
    raise SystemExit("could not detect phase/step from git log; pass --phase and --step")


def committed_substeps(
    phase: int, step: int, history: list[str] | None = None
) -> set[int]:
    done = set()
    lines = history if history is not None else git("log", "--pretty=%s", "-200").splitlines()
    for line in lines[:200]:
        m = COMMIT_RE.search(line)
        if m and int(m.group(1)) == phase and int(m.group(2)) == step:
            done.add(int(m.group(3)))
    return done


def resolve_substep(
    guide: Guide, explicit: str | None, history: list[str] | None = None
) -> Substep:
    """Pick the substep to work on from two independent completion signals."""
    committed = committed_substeps(guide.phase, guide.step, history)
    done = guide.recorded | committed
    print(f"  guide records : {sorted(guide.recorded) or '-'}")
    print(f"  git log       : {sorted(committed) or '-'}")
    if explicit:
        return guide.get(explicit)
    for s in guide.substeps:
        if s.num not in done:
            return s
    raise SystemExit(f"every substep in {guide.rel} looks complete; pass --substep to override")


# --------------------------------------------------------------------------
# engines
# --------------------------------------------------------------------------


@dataclass
class Result:
    ok: bool
    output: str
    seconds: float
    note: str = ""
    log_path: Path | None = None


STREAM_READ_BYTES = 16 * 1024
STREAM_HEAD_CHARS = 1500
STREAM_TAIL_CHARS = 4000
STREAM_FLUSH_BYTES = 64 * 1024
STREAM_FLUSH_SECONDS = 0.25
TERMINATE_GRACE_SECONDS = 2.0


class _Excerpt:
    """Character-bounded head/tail capture; chunk sizes are bounded by the reader."""

    def __init__(self, head_limit: int, tail_limit: int):
        self.head_limit = head_limit
        self.tail_limit = tail_limit
        self.head = ""
        self.tail: deque[str] = deque()
        self.tail_size = 0
        self.total = 0

    def add(self, text: str) -> None:
        self.total += len(text)
        if len(self.head) < self.head_limit:
            take = min(self.head_limit - len(self.head), len(text))
            self.head += text[:take]
        if self.tail_limit:
            chunk = text[-self.tail_limit :]
            self.tail.append(chunk)
            self.tail_size += len(chunk)
            while self.tail_size > self.tail_limit:
                excess = self.tail_size - self.tail_limit
                first = self.tail.popleft()
                if len(first) > excess:
                    self.tail.appendleft(first[excess:])
                    self.tail_size -= excess
                else:
                    self.tail_size -= len(first)

    def render(self, log_path: Path) -> str:
        if self.total <= self.head_limit + self.tail_limit:
            if self.total <= self.head_limit:
                return self.head
            tail = "".join(self.tail)
            overlap = len(self.head) + len(tail) - self.total
            return self.head + tail[max(0, overlap):]
        omitted = self.total - self.head_limit - self.tail_limit
        marker = (
            f"\n[...{omitted} Unicode characters omitted; complete transcript: "
            f"{log_path}...]\n"
        )
        return self.head + marker + "".join(self.tail)


def _signal_owned_processes(proc: subprocess.Popen, sig: int) -> None:
    """Signal the owned POSIX group, or only the child on non-POSIX systems."""
    try:
        if os.name == "posix":
            os.killpg(proc.pid, sig)
        elif sig == signal.SIGTERM:
            proc.terminate()
        else:
            proc.kill()
    except (ProcessLookupError, PermissionError):
        pass


def _owned_processes_exist(proc: subprocess.Popen) -> bool:
    if os.name != "posix":
        return proc.poll() is None
    try:
        os.killpg(proc.pid, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def _finish_termination(proc: subprocess.Popen, term_at: float) -> None:
    """Allow a bounded SIGTERM grace even if the leader exits before descendants."""
    end = term_at + TERMINATE_GRACE_SECONDS
    while time.monotonic() < end:
        proc.poll()  # reap the leader promptly; descendants may still own the group
        if not _owned_processes_exist(proc):
            break
        time.sleep(0.02)
    if _owned_processes_exist(proc):
        _signal_owned_processes(proc, signal.SIGKILL)
    try:
        proc.wait(timeout=TERMINATE_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        _signal_owned_processes(proc, signal.SIGKILL)
        try:
            proc.wait(timeout=TERMINATE_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            pass


def _stream_console(label: str, text: str, at_line_start: bool) -> bool:
    """Prefix bounded decoded chunks without retaining a potentially huge line."""
    for piece in text.splitlines(keepends=True):
        if at_line_start:
            sys.stdout.write(f"  {label} | ")
        sys.stdout.write(piece)
        at_line_start = piece.endswith(("\n", "\r"))
    return at_line_start


def _stream(
    argv: list[str],
    timeout: int,
    label: str,
    env: dict | None = None,
    *,
    log_path: Path | None = None,
    stdout_path: Path | None = None,
) -> Result:
    """Run *argv*, tee combined output, and return an exit-status result.

    With ``log_path``, raw stdout/stderr bytes are written incrementally as the
    complete transcript and ``output`` is a head/tail excerpt bounded to 1,500
    and 4,000 Unicode characters (plus an omission marker). UTF-8 is decoded
    independently per pipe so split multibyte sequences remain valid. An
    internal ``stdout_path`` can additionally preserve stdout as a complete
    model final response without retaining the execution transcript in memory.
    Without a log path, full decoded output is retained for compatibility;
    noisy production call sites therefore always supply one.
    """
    start = time.monotonic()
    deadline = start + timeout
    resolved = log_path.resolve() if log_path is not None else None
    resolved_stdout = stdout_path.resolve() if stdout_path is not None else None
    if resolved is not None:
        resolved.parent.mkdir(parents=True, exist_ok=True)
    if resolved_stdout is not None:
        resolved_stdout.parent.mkdir(parents=True, exist_ok=True)
    log_context = resolved.open("wb") if resolved is not None else nullcontext(None)
    stdout_context = (
        resolved_stdout.open("wb") if resolved_stdout is not None else nullcontext(None)
    )
    full: list[str] | None = [] if resolved is None else None
    excerpt = _Excerpt(STREAM_HEAD_CHARS, STREAM_TAIL_CHARS)
    timed_out = False
    term_at: float | None = None
    forced_at: float | None = None
    code = -1
    console_line_start = True
    console_pending = 0
    last_console_flush = start
    disk_pending = 0
    last_disk_flush = start

    with log_context as log, stdout_context as stdout_capture:
        proc = subprocess.Popen(
            argv,
            cwd=REPO,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE if stdout_capture is not None else subprocess.STDOUT,
            bufsize=0,
            env={**os.environ, **(env or {})},
            start_new_session=os.name == "posix",
        )
        assert proc.stdout is not None
        pipes = [proc.stdout]
        if proc.stderr is not None:
            pipes.append(proc.stderr)
        selector = selectors.DefaultSelector()
        decoders = {}
        for pipe in pipes:
            selector.register(pipe, selectors.EVENT_READ, pipe is proc.stdout)
            decoders[pipe.fileno()] = codecs.getincrementaldecoder("utf-8")(errors="replace")
        try:
            open_pipes = len(pipes)
            while open_pipes:
                now = time.monotonic()
                if not timed_out and now >= deadline:
                    timed_out = True
                    term_at = now
                    _signal_owned_processes(proc, signal.SIGTERM)
                if timed_out and term_at is not None and forced_at is None and now >= term_at + TERMINATE_GRACE_SECONDS:
                    forced_at = now
                    _signal_owned_processes(proc, signal.SIGKILL)
                if forced_at is not None and now >= forced_at + 1.0:
                    # A foreign or unkillable descendant must not hold our pipe forever.
                    break

                wake = 0.1
                if not timed_out:
                    wake = min(wake, max(0.0, deadline - now))
                events = selector.select(wake)
                if not events:
                    continue
                for key, _ in events:
                    pipe = key.fileobj
                    chunk = os.read(pipe.fileno(), STREAM_READ_BYTES)
                    if not chunk:
                        selector.unregister(pipe)
                        open_pipes -= 1
                        final = decoders[pipe.fileno()].decode(b"", final=True)
                        if final:
                            if full is not None:
                                full.append(final)
                            else:
                                excerpt.add(final)
                            console_line_start = _stream_console(label, final, console_line_start)
                        continue
                    if log is not None:
                        log.write(chunk)
                        disk_pending += len(chunk)
                    if stdout_capture is not None and key.data:
                        stdout_capture.write(chunk)
                    text = decoders[pipe.fileno()].decode(chunk)
                    if full is not None:
                        full.append(text)
                    else:
                        excerpt.add(text)
                    console_line_start = _stream_console(label, text, console_line_start)
                    console_pending += len(chunk)
                    now = time.monotonic()
                    if console_pending >= STREAM_FLUSH_BYTES or now - last_console_flush >= STREAM_FLUSH_SECONDS:
                        sys.stdout.flush()
                        console_pending = 0
                        last_console_flush = now
                    if disk_pending >= STREAM_FLUSH_BYTES or now - last_disk_flush >= STREAM_FLUSH_SECONDS:
                        if log is not None:
                            log.flush()
                        if stdout_capture is not None:
                            stdout_capture.flush()
                        disk_pending = 0
                        last_disk_flush = now

            if timed_out:
                assert term_at is not None
                _finish_termination(proc, term_at)
                code = proc.returncode if proc.returncode is not None else -1
            else:
                code = proc.wait()
        except BaseException:
            interrupted_at = time.monotonic()
            _signal_owned_processes(proc, signal.SIGTERM)
            _finish_termination(proc, interrupted_at)
            raise
        finally:
            selector.close()
            for pipe in pipes:
                pipe.close()
            if log is not None:
                log.flush()
            if stdout_capture is not None:
                stdout_capture.flush()
            sys.stdout.flush()

    output = "".join(full) if full is not None else excerpt.render(resolved)
    return Result(
        ok=code == 0 and not timed_out,
        output=output,
        seconds=time.monotonic() - start,
        note="timed out" if timed_out else (f"exit {code}" if code else ""),
        log_path=resolved,
    )


class Engine:
    name = "engine"
    tools = False

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> Result:
        raise NotImplementedError


def _invocation_final_path(log_path: Path | None, suffix: str) -> Path:
    if log_path is None:
        RUNS.mkdir(parents=True, exist_ok=True)
        return RUNS / f"standalone-{os.getpid()}-{time.time_ns()}.final{suffix}"
    name = log_path.name.removesuffix(".transcript.log")
    return log_path.with_name(f"{name}.final{suffix}")


class ClaudeEngine(Engine):
    """Tool-using Claude CLI with separate complete stdout/final capture."""

    tools = True

    def __init__(
        self,
        model: str = "opus",
        permission_mode: str = "acceptEdits",
        config_dir: Path | None = None,
    ):
        self.model, self.permission_mode = model, permission_mode
        self.config_dir = config_dir or default_claude_config_dir()
        self.name = f"claude:{model}"

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> Result:
        if not shutil.which("claude"):
            return Result(False, "", 0.0, "claude CLI not found", log_path)
        argv = [
            "claude", "-p", prompt,
            "--model", self.model,
            "--permission-mode", self.permission_mode,
            "--output-format", "text",
            "--append-system-prompt",
            "Never run git commit, git push, git stash, git reset, git checkout --, "
            "or any command that discards working-tree changes. Leave all changes "
            "unstaged and uncommitted for human review.",
        ]
        if schema is not None:
            argv += ["--json-schema", json.dumps(json.loads(schema.read_text(encoding="utf-8")))]
        final_path = _invocation_final_path(log_path, ".json" if schema else ".md")
        final_path.unlink(missing_ok=True)
        res = _stream(
            argv,
            timeout,
            "claude",
            env={
                "CLAUDE_CONFIG_DIR": str(self.config_dir),
                "PAI_DIR": str(self.config_dir),
            },
            log_path=log_path,
            stdout_path=final_path,
        )
        try:
            # Claude print/text writes the final response on stdout. Capturing it
            # separately prevents stderr diagnostics in the combined transcript
            # from corrupting reviewer JSON. Result.output is the complete final
            # response, unlike _stream's bounded combined-output excerpt.
            res.output = final_path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            res.ok = False
            res.note = f"cannot preserve Claude final response: {exc}"
        return res


class CodexEngine(Engine):
    """Tool-using Codex CLI. Default reviewer sandbox is read-only."""

    tools = True

    def __init__(
        self,
        model: str | None = None,
        sandbox: str = "read-only",
        oss: bool = False,
        local_provider: str = "ollama",
    ):
        self.model, self.sandbox, self.oss, self.local_provider = (
            model, sandbox, oss, local_provider,
        )
        self.name = f"codex:{'oss/' if oss else ''}{model or 'default'}"

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> Result:
        if not shutil.which("codex"):
            return Result(False, "", 0.0, "codex CLI not found", log_path)
        final_path = _invocation_final_path(log_path, ".json" if schema else ".md")
        final_path.parent.mkdir(parents=True, exist_ok=True)
        final_path.unlink(missing_ok=True)
        argv = ["codex", "exec", "--cd", str(REPO), "-s", self.sandbox, "--color", "never"]
        if self.oss:
            argv += ["--oss", "--local-provider", self.local_provider]
        if self.model:
            argv += ["-m", self.model]
        if schema:
            argv += ["--output-schema", str(schema)]
        argv += ["-o", str(final_path), prompt]
        res = _stream(argv, timeout, "codex", log_path=log_path)
        if final_path.exists():
            try:
                res.output = final_path.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as exc:
                res.ok = False
                res.note = f"cannot read Codex final response: {exc}"
        elif res.ok:
            res.ok = False
            res.note = "Codex did not save a final response"
        return res


class OllamaEngine(Engine):
    """Tool-free local chat. For condensing and triage only — never editing."""

    tools = False

    def __init__(
        self,
        model: str = "qwen3.5:4b",
        host: str = OLLAMA,
        num_ctx: int = 16384,
        num_predict: int = 1024,
    ):
        self.model, self.host = model, host
        self.num_ctx, self.num_predict = num_ctx, num_predict
        self.name = f"ollama:{model}"

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> Result:
        payload = {
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
            "think": False,
            "options": {
                "temperature": 0.1,
                "num_ctx": self.num_ctx,
                "num_predict": self.num_predict,
            },
        }
        if schema:
            payload["format"] = json.loads(schema.read_text(encoding="utf-8"))
        req = urllib.request.Request(
            f"{self.host}/api/chat",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )
        start = time.monotonic()
        resolved = log_path.resolve() if log_path is not None else None
        try:
            with urllib.request.urlopen(req, timeout=timeout) as response:
                raw = response.read()
            if resolved is not None:
                resolved.parent.mkdir(parents=True, exist_ok=True)
                resolved.write_bytes(raw)
            body = json.loads(raw.decode("utf-8"))
            output = body["message"]["content"]
            if not isinstance(output, str):
                raise TypeError("message.content is not a string")
            if resolved is not None:
                final_path = _invocation_final_path(resolved, ".json" if schema else ".md")
                final_path.write_text(output, encoding="utf-8")
            return Result(True, output, time.monotonic() - start, log_path=resolved)
        except (urllib.error.URLError, OSError, KeyError, TypeError, UnicodeError, json.JSONDecodeError) as exc:
            return Result(False, "", time.monotonic() - start, f"ollama unavailable: {exc}", resolved)


PROFILES: dict[str, dict[str, object]] = {
    # cross-model: Claude writes, Codex reviews, local model does the cheap roles
    "default": {
        "implementer": lambda: ClaudeEngine("opus"),
        "reviewer": lambda: CodexEngine(sandbox="read-only"),
        "fixer": lambda: ClaudeEngine("opus"),
        "condenser": lambda: OllamaEngine(),
        "triage": lambda: OllamaEngine(),
    },
    # everything local — costs nothing, use it to debug this script's wiring
    "local": {
        "implementer": lambda: CodexEngine(
            model="qwen3.5:4b", sandbox="workspace-write", oss=True
        ),
        "reviewer": lambda: CodexEngine(model="qwen3.5:4b", sandbox="read-only", oss=True),
        "fixer": lambda: CodexEngine(
            model="qwen3.5:4b", sandbox="workspace-write", oss=True
        ),
        "condenser": lambda: OllamaEngine(),
        "triage": lambda: OllamaEngine(),
    },
    "claude-only": {
        "implementer": lambda: ClaudeEngine("opus"),
        "reviewer": lambda: ClaudeEngine("opus", permission_mode="plan"),
        "fixer": lambda: ClaudeEngine("opus"),
        "condenser": lambda: OllamaEngine(),
        "triage": lambda: OllamaEngine(),
    },
}


# --------------------------------------------------------------------------
# gates — run by this script, not by an agent claiming it ran them
# --------------------------------------------------------------------------

GATE_SETS: dict[str, list[tuple[str, list[str]]]] = {
    "rust": [
        ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        ("test", ["cargo", "test", "--workspace"]),
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
    name: str
    ok: bool
    output: str
    seconds: float
    log_path: Path | None = None


def dotenv() -> dict[str, str]:
    """cargo test --workspace needs DATABASE_URL from .env."""
    env: dict[str, str] = {}
    f = REPO / ".env"
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
        r = _stream(argv, timeout, name, env=env, log_path=log_path)
        out.append(Gate(name, r.ok, r.output, r.seconds, log_path=log_path))
        print(f"  gate: {name} -> {'pass' if r.ok else 'FAIL'} ({r.seconds:.0f}s)")
    return out


# A small local model is good at extraction and bad at obeying format
# instructions. So it only fills this schema; Python does all the formatting.
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
    d = RUNS / ".schemas"
    d.mkdir(parents=True, exist_ok=True)
    f = d / f"{name}.json"
    f.write_text(json.dumps(obj, indent=2), encoding="utf-8")
    return f


# Deterministic formats supported by rustc/clippy, cargo test, rustfmt, and Terraform.
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
        chunk = stream.read(STREAM_READ_BYTES)
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
            loc = loc.replace(f"{REPO}/", "")
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
    engine: Engine,
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


# --------------------------------------------------------------------------
# prompts — parameterized from docs/planning/prompt_templates.txt
# --------------------------------------------------------------------------

VERDICT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["verdict", "summary", "blockers", "substeps_covered"],
    "properties": {
        "verdict": {"type": "string", "enum": ["READY", "NOT_READY"]},
        "summary": {"type": "string"},
        "substeps_covered": {"type": "array", "items": {"type": "string"}},
        "blockers": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["severity", "file", "problem", "required_fix"],
                "properties": {
                    "severity": {
                        "type": "string",
                        "enum": ["critical", "high", "medium", "low"],
                    },
                    "file": {"type": "string"},
                    "problem": {"type": "string"},
                    "required_fix": {"type": "string"},
                },
            },
        },
    },
}

NO_VCS = (
    "Do not commit, push, stage, unstage, stash, or revert anything. "
    "Leave every change in the working tree for human review."
)


def implement_prompt(g: Guide, s: Substep, sha: str, pre: str) -> str:
    return f"""Analyze the codebase and implement Phase {g.phase}, Step {g.step}, substep {s.id} \
({s.title}) exactly as defined in {g.rel}.

The substep section begins at {g.rel}:{s.line}. Read it in the file — the text below is a copy \
for grounding, and the file is authoritative.

Baseline: HEAD is {sha}. These paths were already modified before this run; they are NOT yours \
to revert, and some may be unrelated to {s.id}:
{pre or "  (clean tree)"}

--- {s.id} (verbatim from the guide) ---
{s.text[:20000]}
--- end {s.id} ---

Complete the implementation, verification, and documentation in this task. Make reasonable \
decisions using repository conventions; ask for clarification only when a material ambiguity \
prevents correct progress.

1. Understand the application and {s.id}
- Read CLAUDE.md, the implementation guide, and relevant architecture documentation.
- Inspect the codebase broadly to understand structure, existing capabilities, and patterns. \
Trace {s.id}'s dependencies and affected workflows in detail.
- Identify {s.id}'s requirements, acceptance criteria, prerequisites, and its relationship to \
preceding and subsequent substeps.
- Determine what already exists, what is missing, and what needs modification. Reuse suitable \
existing functionality.
- Briefly state your understanding of {s.id} and the intended implementation before editing.

2. Implement {s.id}
- Complete all applicable {s.id} requirements and integrate them into the application's actual \
workflows.
- Follow established conventions for interfaces, data models, security, error handling, and \
configuration.
- Address relevant edge cases, failure paths, and compatibility concerns.
- Resolve prerequisite gaps necessary for {s.id} where feasible. Clearly identify any larger \
dependency that blocks completion.
- Keep changes focused on {s.id} and its necessary integration work. Account for documented \
future substeps without implementing speculative functionality.
- Preserve unrelated changes already present in the working tree.

3. Verify the implementation
- Validate {s.id} against each acceptance criterion and trace the affected workflow end to end.
- Add or update meaningful tests for new behavior and relevant failure cases.
- Run repository checks appropriate to the change.
- Inspect the final diff for regressions and inconsistencies between code and documentation.
- Report actual verification results and any limitations.

4. Update the guide's {s.id} section in {g.rel}
Preserve the guide's structure and other substeps. Add an "## Implementation record — {s.id} \
({datetime.now():%Y-%m-%d})" section explaining: what was implemented with real file and \
function references; why it is needed; where it fits including upstream inputs and downstream \
consumers; how it behaves on normal and failure paths; a concrete walkthrough with synthetic \
example data; a concise Mermaid diagram consistent with that walkthrough; how it was verified \
and what limitations remain.

Clearly distinguish implemented behavior from planned future behavior. Document and justify any \
necessary deviation from {s.id}'s original requirements.

Finish with a concise summary of the implementation, files changed, verification results, \
documentation updates, and any unresolved blockers. {NO_VCS}"""


def review_prompt(g: Guide, s: Substep, sha: str, gates: str, pre: str) -> str:
    return f"""Perform a pre-commit review of the complete set of new changes in this repository.

Do not review only the most recent commit. Treat HEAD ({sha}) as the last committed baseline and \
the current working tree as the proposed commit batch. The batch was intended to implement \
Phase {g.phase} Step {g.step} {s.id} ({s.title}), defined at {g.rel}:{s.line} — but verify that \
claim rather than assuming it. The batch may touch more substeps than one.

These paths were already modified before the implementer ran; treat them as pre-existing unless \
the batch made them incorrect:
{pre or "  (clean tree)"}

Gates were already executed by the orchestrator, not by an agent. Actual results:

{gates}

You are in a read-only sandbox. Do not attempt to fix anything — report precisely enough that a \
separate agent can.

1. Scope: inspect full repository status; review every change relative to HEAD including staged, \
unstaged, untracked, deletions, renames, source, tests, migrations, config, and docs. If the \
implementation guide itself was modified, compare it against its version at HEAD so documentation \
edits cannot hide deviations from the original plan.

2. Guide coverage: extract {s.id}'s requirements, prerequisites, and acceptance criteria. Map \
every applicable requirement to specific implementation evidence. Classify each as complete, \
partial, missing, incorrect, or unverifiable. Check prerequisites hold and that the work stays \
compatible with documented later substeps. Identify undocumented deviations and judge whether \
each improves the application or adds unnecessary risk.

3. Correctness and integration: trace affected workflows from entry points through business logic, \
persistence, and downstream consumers. Check interfaces, data models, migrations, API contracts, \
configuration, and error handling. Verify boundary conditions, failure paths, concurrency, and \
data-integrity guarantees. Distinguish defects introduced by this batch from pre-existing issues \
merely exposed by it. Confirm tests validate externally meaningful behavior rather than mirroring \
implementation details.

4. HIPAA-focused security review: authentication and session handling; server-side authorization \
and object-level access control; patient/org/tenant isolation; least privilege; PHI in transit, \
at rest, in URLs, logs, errors, telemetry, caches, exports, and external services; secrets \
handling; audit-event coverage and integrity; retention and deletion; input validation and \
injection; fail-open behavior and information leakage. For each finding cite file and line, give \
the realistic abuse scenario and impact, and distinguish confirmed vulnerabilities from \
defense-in-depth suggestions. Do not claim HIPAA compliance because tests pass.

Return ONLY the JSON verdict object. Mark READY only if every intended requirement is implemented \
correctly, acceptance criteria are satisfied, no unresolved critical or high findings remain, \
gates pass, and code and documentation agree. Otherwise NOT_READY with the exact blockers."""


def fix_prompt(g: Guide, s: Substep, verdict: dict, gates: str, loop: str) -> str:
    blockers = json.dumps(verdict.get("blockers", []), indent=2)
    return f"""A read-only reviewer examined your Phase {g.phase} Step {g.step} {s.id} \
implementation and marked it NOT_READY.

Reviewer summary: {verdict.get("summary", "(none)")}

Confirmed blockers to fix:
{blockers}

Current gate results:

{gates}
{loop}
Fix the confirmed correctness, integration, security, and documentation problems within this \
batch's scope. Add missing functionality or safeguards required to complete {s.id}. Add or update \
meaningful tests for corrected behavior including authorization failures, invalid inputs, and \
other negative paths. Retain useful additions outside the original plan when they provide a \
concrete security, reliability, or maintainability benefit; remove additions whose risk outweighs \
their benefit.

If you believe a reported blocker is wrong, do not silently ignore it — explain precisely why the \
reviewer is mistaken, with evidence from the code.

Re-run the checks appropriate to the affected workflows and inspect the final diff. Do not \
repeatedly broaden testing once appropriate checks pass. Keep {g.rel}'s implementation record \
accurate with whatever you changed. {NO_VCS}"""


def gate_fix_prompt(
    g: Guide, s: Substep, gates: str, *, review_occurred: bool = False
) -> str:
    review_context = (
        "A semantic reviewer returned READY, but gates independently prevent approval."
        if review_occurred
        else "No semantic code review was performed in this round."
    )
    return f"""The orchestrator ran required validation gates for Phase {g.phase} Step {g.step} \
{s.id} ({s.title}), and at least one gate failed. {review_context} Fix the gate failures using the \
bounded summary below and the referenced complete transcripts. Keep the work focused, preserve \
unrelated working-tree changes, update relevant tests or documentation, and do not claim review \
findings that do not exist.

Current gate results:

{gates}

After your edits, the orchestrator—not you—will run a fresh validation round. {NO_VCS}"""


# --------------------------------------------------------------------------
# verdict + oscillation
# --------------------------------------------------------------------------


def _schema_error(value, schema: dict, path: str = "$" ) -> str | None:
    expected = schema.get("type")
    types = {
        "object": dict,
        "array": list,
        "string": str,
        "boolean": bool,
        "number": (int, float),
        "integer": int,
    }
    if expected in types and (not isinstance(value, types[expected]) or expected in {"number", "integer"} and isinstance(value, bool)):
        return f"{path} must be {expected}"
    if "enum" in schema and value not in schema["enum"]:
        return f"{path} is not an allowed value"
    if expected == "object":
        required = schema.get("required", [])
        missing = [key for key in required if key not in value]
        if missing:
            return f"{path} missing required fields: {', '.join(missing)}"
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            extra = sorted(set(value) - set(properties))
            if extra:
                return f"{path} has additional fields: {', '.join(extra)}"
        for key, child in properties.items():
            if key in value:
                error = _schema_error(value[key], child, f"{path}.{key}")
                if error:
                    return error
    if expected == "array":
        child = schema.get("items", {})
        for index, item in enumerate(value):
            error = _schema_error(item, child, f"{path}[{index}]")
            if error:
                return error
    return None


def _protocol_verdict(detail: str) -> dict:
    return {
        "verdict": "NOT_READY",
        "summary": f"review protocol failure: {detail}",
        "blockers": [],
        "substeps_covered": [],
        "_protocol_error": True,
    }


def parse_verdict(raw: str) -> dict:
    """Parse one complete schema-valid verdict; malformed prose fails closed."""
    candidates = [raw]
    candidates.extend(re.findall(r"```(?:json)?\s*(.*?)```", raw, re.S))
    decoder = json.JSONDecoder()
    for match in re.finditer(r"\{", raw):
        try:
            value, _ = decoder.raw_decode(raw, match.start())
            candidates.append(json.dumps(value))
        except json.JSONDecodeError:
            continue
    last_error = "no complete JSON verdict object"
    for candidate in candidates:
        try:
            value = json.loads(candidate.strip())
        except (json.JSONDecodeError, AttributeError):
            continue
        error = _schema_error(value, VERDICT_SCHEMA)
        if error:
            last_error = error
            continue
        if value["verdict"] == "READY" and value["blockers"]:
            last_error = "READY contradicts non-empty blockers"
            continue
        return value
    return _protocol_verdict(last_error)


def repeated_blockers(prev: dict, cur: dict, thresh: float = 0.72) -> list[str]:
    """Return current blockers matching prior same-file findings.

    The index costs O(previous blocker text) memory. Exact normalized matches are
    constant-time. Reworded findings still use SequenceMatcher with current text
    first; difficult comparisons within one file remain quadratic worst-case.
    real_quick_ratio/quick_ratio prefilters were benchmarked but are not used:
    representative small and large same-file inputs showed overhead without a
    timing win, while exact matches benefit substantially from the set index.
    """
    by_file: dict[str, list[str]] = {}
    exact_by_file: dict[str, set[str]] = {}
    for blocker in prev.get("blockers", []):
        file_name = blocker.get("file", "?")
        problem = (blocker.get("problem", "") or "").lower()
        by_file.setdefault(file_name, []).append(problem)
        exact_by_file.setdefault(file_name, set()).add(problem)

    out: list[str] = []
    for blocker in cur.get("blockers", []):
        file_name = blocker.get("file", "?")
        current = (blocker.get("problem", "") or "").lower()
        candidates = by_file.get(file_name, ())
        matched = current in exact_by_file.get(file_name, set())
        if not matched:
            for previous in candidates:
                matcher = difflib.SequenceMatcher(None, current, previous)
                if matcher.ratio() >= thresh:
                    matched = True
                    break
        if matched:
            out.append(f"{file_name}: {blocker.get('problem', '')[:120]}")
    return out


def check_oscillation(
    prev: dict | None, cur: dict, engine: Engine, *, log_path: Path | None = None
) -> str:
    """Detect the fixer thrashing on the same blocker across rounds."""
    if not prev:
        return ""
    repeat = repeated_blockers(prev, cur)
    if not repeat:
        return ""
    warn = (
        "\nOSCILLATION WARNING: the following blockers survived your previous fix "
        "attempt unchanged:\n"
        + "\n".join(f"  - {r}" for r in sorted(repeat))
        + "\nDo not retry the same approach. Either fix the underlying cause or state "
        "plainly why this cannot be fixed within this substep's scope.\n"
    )
    hint = engine.run(
        "Two rounds of code review produced overlapping blockers, meaning the fix attempt did "
        "not work. In under 60 words, state what the fixer most likely misunderstood. No "
        "preamble.\n\nRound N-1:\n"
        + json.dumps(prev.get("blockers", []))[:2500]
        + "\n\nRound N:\n"
        + json.dumps(cur.get("blockers", []))[:2500],
        timeout=120,
        log_path=log_path,
    )
    if hint.ok and hint.output.strip():
        warn += f"Triage hypothesis: {hint.output.strip()[:400]}\n"
    return warn


# --------------------------------------------------------------------------
# the loop
# --------------------------------------------------------------------------


def _unique_run_dir(step: int, substep: str) -> Path:
    base = RUNS / f"{datetime.now(timezone.utc):%Y%m%dT%H%M%S.%fZ}-step{step}-{substep}"
    candidate = base
    serial = 1
    while candidate.exists():
        candidate = base.with_name(f"{base.name}-{serial}")
        serial += 1
    candidate.mkdir(parents=True)
    return candidate


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--phase", type=int, help="default: detected from the 40 newest commit subjects")
    p.add_argument("--step", type=int, help="default: detected from the 40 newest commit subjects")
    p.add_argument("--substep", help="e.g. S5; default: first not recorded and not committed")
    p.add_argument("--profile", choices=sorted(PROFILES), default="default")
    p.add_argument("--gates", choices=sorted(GATE_SETS), default="rust", help="gate set; 'none' intentionally disables deterministic gates")
    p.add_argument(
        "--review-policy",
        choices=("gates-first", "always"),
        default="gates-first",
        help="gates-first (default) fixes failed gates before review; always reviews every round",
    )
    p.add_argument("--max-rounds", type=int, default=3, help="validation rounds including the final no-edit round")
    p.add_argument("--agent-timeout", type=int, default=3600, help="seconds per model invocation")
    p.add_argument("--gate-timeout", type=int, default=1800, help="seconds per gate process group")
    p.add_argument("--local-model", help="override the Ollama model for condense/triage")
    p.add_argument("--skip-implement", action="store_true", help="validate and review the existing tree without an implement call")
    p.add_argument("--dry-run", action="store_true", help="resolve and print prompts; run no gates or models and create no artifacts")
    a = p.parse_args()
    if a.max_rounds < 1:
        p.error("--max-rounds must be at least 1")

    history = git("log", "--pretty=%s", "-200").splitlines()
    phase, step = (a.phase, a.step) if a.phase and a.step else detect_phase_step(history)
    found = find_guide(phase, step, include_body=True)
    assert isinstance(found, tuple)
    guide_path, guide_body = found
    guide = parse_guide(guide_path, phase, step, guide_body)

    print(f"\n=== phase {phase} step {step} — {guide.rel}")
    sub = resolve_substep(guide, a.substep, history)
    print(f"  target        : {sub.id} — {sub.title}  ({guide.rel}:{sub.line})")

    # These are immutable baseline snapshots. Mutable tree state is left to each
    # model invocation to inspect afresh after implementation/fix operations.
    sha = git("rev-parse", "HEAD")
    pre = "\n".join(f"  {line}" for line in git("status", "--porcelain").splitlines())
    roles = {key: factory() for key, factory in PROFILES[a.profile].items()}
    if a.local_model:
        roles = {
            key: (OllamaEngine(a.local_model) if isinstance(engine, OllamaEngine) else engine)
            for key, engine in roles.items()
        }
    print(f"  baseline      : {sha[:12]}")
    print(f"  profile       : {a.profile}")
    print(f"  review policy : {a.review_policy}")
    for role, engine in roles.items():
        print(f"    {role:<12}: {engine.name}")
    gate_count = len(GATE_SETS[a.gates])
    print(f"  gates         : {a.gates} ({gate_count})" + (" — intentionally none" if not gate_count else ""))

    if a.dry_run:
        print("\n--- implement prompt ---\n")
        print(implement_prompt(guide, sub, sha, pre))
        print("\n--- gate-fix prompt (failed-gate stub) ---\n")
        print(gate_fix_prompt(guide, sub, "(gates not yet run)"))
        print("\n--- review prompt (gates stubbed) ---\n")
        print(review_prompt(guide, sub, sha, "(gates not yet run)", pre))
        return 0

    run = _unique_run_dir(step, sub.id)
    schema = run / "verdict.schema.json"
    schema.write_text(json.dumps(VERDICT_SCHEMA, indent=2), encoding="utf-8")
    (run / "baseline.diff").write_text(git("diff", "HEAD"), encoding="utf-8")
    print(f"  artifacts     : {run.relative_to(REPO)}\n")

    def save(name: str, text: str) -> None:
        (run / name).write_text(text, encoding="utf-8")

    def ensure_final(name: str, result: Result) -> None:
        destination = run / name
        if result.ok and not destination.exists():
            destination.write_text(result.output, encoding="utf-8")

    if not a.skip_implement:
        print("=== implement")
        prompt = implement_prompt(guide, sub, sha, pre)
        save("00-implement.prompt.md", prompt)
        result = roles["implementer"].run(
            prompt,
            a.agent_timeout,
            log_path=run / "00-implement.transcript.log",
        )
        ensure_final("00-implement.final.md", result)
        if not result.ok:
            print(f"\n!! implementer failed ({result.note}); validating the tree as it stands")

    previous_semantic_verdict: dict | None = None
    verdict: dict = {}
    final_gates: list[Gate] = []

    for round_num in range(1, a.max_rounds + 1):
        print(f"\n=== round {round_num}/{a.max_rounds}: gates")
        gates = run_gates(a.gates, a.gate_timeout, run_dir=run, round_num=round_num)
        final_gates = gates
        gate_text = condense(
            gates,
            roles["condenser"],
            helper_log_path=run / f"{round_num:02d}-gate-condense.transcript.log",
        ) or "(no gates configured; deterministic validation intentionally skipped)"
        save(f"{round_num:02d}-gates.summary.md", gate_text)
        gates_pass = all(gate.ok for gate in gates)

        if a.review_policy == "gates-first" and not gates_pass:
            print("\n  required gates failed; semantic review deferred by gates-first policy")
            if round_num == a.max_rounds:
                break
            print(f"\n=== round {round_num}/{a.max_rounds}: gate fix")
            prompt = gate_fix_prompt(guide, sub, gate_text)
            save(f"{round_num:02d}-gate-fix.prompt.md", prompt)
            fix = roles["fixer"].run(
                prompt,
                a.agent_timeout,
                log_path=run / f"{round_num:02d}-gate-fix.transcript.log",
            )
            ensure_final(f"{round_num:02d}-gate-fix.final.md", fix)
            if not fix.ok:
                print(f"\n!! gate fixer failed ({fix.note}); the next round still revalidates current state")
            continue

        print(f"\n=== round {round_num}/{a.max_rounds}: review")
        prompt = review_prompt(guide, sub, sha, gate_text, pre)
        save(f"{round_num:02d}-review.prompt.md", prompt)
        review = roles["reviewer"].run(
            prompt,
            a.agent_timeout,
            schema=schema,
            log_path=run / f"{round_num:02d}-review.transcript.log",
        )
        ensure_final(f"{round_num:02d}-review.final.json", review)
        if not review.ok:
            verdict = _protocol_verdict(f"reviewer invocation failed: {review.note or 'unknown error'}")
            save(f"{round_num:02d}-verdict.validated.json", json.dumps(verdict, indent=2))
            print(f"\n!! {verdict['summary']}; failed invocations can never approve")
            return 2

        verdict = parse_verdict(review.output)
        save(f"{round_num:02d}-verdict.validated.json", json.dumps(verdict, indent=2))
        blockers = verdict.get("blockers", [])
        print(f"\n  verdict  : {verdict['verdict']}")
        print(f"  summary  : {verdict.get('summary', '')[:300]}")
        for blocker in blockers[:12]:
            print(f"    [{blocker.get('severity', '?'):<8}] {blocker.get('file', '?')}: {blocker.get('problem', '')[:110]}")
        if verdict.get("_protocol_error"):
            print("  (invalid or incomplete verdict; approval fails closed)")
            return 2

        if verdict["verdict"] == "READY" and gates_pass and not blockers:
            print(f"\n=== {sub.id} READY after {round_num} round(s). Nothing was committed.")
            print("    review:  git diff HEAD && git status")
            print(f"    artifacts: {run.relative_to(REPO)}")
            return 0
        if verdict["verdict"] == "READY" and not gates_pass:
            print("\n  reviewer said READY but required gates failed — approval rejected")
            if round_num == a.max_rounds:
                break
            print(f"\n=== round {round_num}/{a.max_rounds}: gate fix")
            prompt = gate_fix_prompt(guide, sub, gate_text, review_occurred=True)
            save(f"{round_num:02d}-gate-fix.prompt.md", prompt)
            fix = roles["fixer"].run(
                prompt,
                a.agent_timeout,
                log_path=run / f"{round_num:02d}-gate-fix.transcript.log",
            )
            ensure_final(f"{round_num:02d}-gate-fix.final.md", fix)
            if not fix.ok:
                print(f"\n!! gate fixer failed ({fix.note}); the next round still revalidates current state")
            previous_semantic_verdict = verdict
            continue

        if round_num == a.max_rounds:
            break

        print(f"\n=== round {round_num}/{a.max_rounds}: semantic fix")
        loop_warning = check_oscillation(
            previous_semantic_verdict,
            verdict,
            roles["triage"],
            log_path=run / f"{round_num:02d}-triage.transcript.log",
        )
        if loop_warning:
            print(loop_warning)
        prompt = fix_prompt(guide, sub, verdict, gate_text, loop_warning)
        save(f"{round_num:02d}-fix.prompt.md", prompt)
        fix = roles["fixer"].run(
            prompt,
            a.agent_timeout,
            log_path=run / f"{round_num:02d}-fix.transcript.log",
        )
        ensure_final(f"{round_num:02d}-fix.final.md", fix)
        if not fix.ok:
            print(f"\n!! fixer failed ({fix.note}); the next round still revalidates current state")
        previous_semantic_verdict = verdict

    print(f"\n=== {sub.id} NOT READY after {a.max_rounds} round(s) — handing back to you.")
    failed = [gate for gate in final_gates if not gate.ok]
    if failed:
        print("  Required gate failures:")
        for gate in failed:
            print(f"    {gate.name}: {gate.log_path or 'no complete transcript'}")
    elif not final_gates:
        print("  No deterministic gates were configured.")
    for blocker in verdict.get("blockers", []):
        print(f"  [{blocker.get('severity', '?')}] {blocker.get('file', '?')}: {blocker.get('problem', '')}")
    print(f"    artifacts: {run.relative_to(REPO)}")
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\ninterrupted — working tree left as-is, nothing committed")
        sys.exit(130)
