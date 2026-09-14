#!/usr/bin/env python3
"""
substep_crew.py — implement/review/fix loop for one phase substep.

Mirrors the manual workflow in docs/planning/prompt_templates.txt:

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

Usage:
    scripts/substep_crew.py --dry-run                 # resolve substep, print prompts
    scripts/substep_crew.py                           # next substep, auto-detected
    scripts/substep_crew.py --substep S5 --step 8
    scripts/substep_crew.py --profile local           # free harness test on Ollama
    scripts/substep_crew.py --gates terraform --max-rounds 4
"""

from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RUNS = REPO / ".agents" / "runs"
OLLAMA = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434")


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


def parse_guide(path: Path, phase: int, step: int) -> Guide:
    body = path.read_text(encoding="utf-8")
    marks = [(m.start(), int(m.group(1)), m.group(2)) for m in SUBSTEP_RE.finditer(body)]
    substeps: list[Substep] = []
    for i, (off, num, title) in enumerate(marks):
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
                line=body.count("\n", 0, off) + 1,
                text=body[off:end].rstrip(),
            )
        )
    recorded = {int(m.group(1)) for m in RECORD_RE.finditer(body)}
    return Guide(path=path, phase=phase, step=step, substeps=substeps, recorded=recorded)


def find_guide(phase: int, step: int) -> Path:
    plan = REPO / "docs" / "planning"
    for pat in (
        f"phase{phase}-step{step}-implementation-steps.md",
        f"phase{phase}-step{step}-*.md",
        f"phase{phase}.md",
    ):
        hits = sorted(plan.glob(pat))
        for h in hits:
            if SUBSTEP_RE.search(h.read_text(encoding="utf-8")):
                return h
    raise SystemExit(f"no guide with '## S<n>' headings for phase {phase} step {step} in {plan}")


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO, capture_output=True, text=True, check=False
    ).stdout.strip()


def detect_phase_step() -> tuple[int, int]:
    """Read the most recent PHASE<n> STEP<m> S<k> commit subject."""
    for line in git("log", "--pretty=%s", "-40").splitlines():
        m = COMMIT_RE.search(line)
        if m:
            return int(m.group(1)), int(m.group(2))
    raise SystemExit("could not detect phase/step from git log; pass --phase and --step")


def committed_substeps(phase: int, step: int) -> set[int]:
    done = set()
    for line in git("log", "--pretty=%s", "-200").splitlines():
        m = COMMIT_RE.search(line)
        if m and int(m.group(1)) == phase and int(m.group(2)) == step:
            done.add(int(m.group(3)))
    return done


def resolve_substep(guide: Guide, explicit: str | None) -> Substep:
    """Pick the substep to work on from two independent completion signals."""
    committed = committed_substeps(guide.phase, guide.step)
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


def _stream(argv: list[str], timeout: int, label: str, env: dict | None = None) -> Result:
    """Run argv, tee stdout to the console, return the transcript."""
    start = time.monotonic()
    proc = subprocess.Popen(
        argv,
        cwd=REPO,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        env={**os.environ, **(env or {})},
    )
    killer = threading.Timer(timeout, proc.kill)
    killer.start()
    lines: list[str] = []
    try:
        assert proc.stdout is not None
        for line in proc.stdout:
            lines.append(line)
            sys.stdout.write(f"  {label} | {line}")
            sys.stdout.flush()
        code = proc.wait()
    finally:
        killer.cancel()
    took = time.monotonic() - start
    timed_out = code < 0 and took >= timeout - 1
    return Result(
        ok=code == 0,
        output="".join(lines),
        seconds=took,
        note="timed out" if timed_out else (f"exit {code}" if code else ""),
    )


class Engine:
    name = "engine"
    tools = False

    def run(self, prompt: str, timeout: int, schema: Path | None = None) -> Result:
        raise NotImplementedError


class ClaudeEngine(Engine):
    """Tool-using, can edit files. Used for implement and fix."""

    tools = True

    def __init__(self, model: str = "opus", permission_mode: str = "acceptEdits"):
        self.model, self.permission_mode = model, permission_mode
        self.name = f"claude:{model}"

    def run(self, prompt: str, timeout: int, schema: Path | None = None) -> Result:
        if not shutil.which("claude"):
            return Result(False, "", 0.0, "claude CLI not found")
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
        return _stream(argv, timeout, "claude")


class CodexEngine(Engine):
    """Tool-using. Default read-only: the reviewer inspects, it does not patch."""

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

    def run(self, prompt: str, timeout: int, schema: Path | None = None) -> Result:
        if not shutil.which("codex"):
            return Result(False, "", 0.0, "codex CLI not found")
        last = RUNS / ".codex-last-message.json"
        last.parent.mkdir(parents=True, exist_ok=True)
        last.unlink(missing_ok=True)
        argv = ["codex", "exec", "--cd", str(REPO), "-s", self.sandbox, "--color", "never"]
        if self.oss:
            argv += ["--oss", "--local-provider", self.local_provider]
        if self.model:
            argv += ["-m", self.model]
        if schema:
            argv += ["--output-schema", str(schema)]
        argv += ["-o", str(last), prompt]
        res = _stream(argv, timeout, "codex")
        if last.exists():
            res.output = last.read_text(encoding="utf-8") or res.output
        return res


class OllamaEngine(Engine):
    """Tool-free local chat. For condensing and triage only — never for editing."""

    tools = False

    def __init__(
        self,
        model: str = "qwen3.5:4b",
        host: str = OLLAMA,
        num_ctx: int = 16384,
        num_predict: int = 1024,
    ):
        # num_ctx matters: Ollama defaults to a small window and silently drops
        # the overflow, which makes a schema-constrained model emit confident
        # nonsense instead of erroring. Keep prompts to this engine short anyway.
        self.model, self.host = model, host
        self.num_ctx, self.num_predict = num_ctx, num_predict
        self.name = f"ollama:{model}"

    def run(self, prompt: str, timeout: int, schema: Path | None = None) -> Result:
        payload = {
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
            # think=false matters as much as num_ctx: qwen3.5 is a reasoning
            # model and spends ~19s of chain-of-thought before answering "OK".
            # These roles are extraction and triage, not reasoning.
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
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                body = json.loads(r.read().decode())
            return Result(True, body["message"]["content"], time.monotonic() - start)
        except (urllib.error.URLError, OSError, KeyError, json.JSONDecodeError) as e:
            return Result(False, "", time.monotonic() - start, f"ollama unavailable: {e}")


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


def run_gates(names: str, timeout: int) -> list[Gate]:
    env = dotenv()
    out: list[Gate] = []
    for name, argv in GATE_SETS[names]:
        print(f"\n  gate: {name}")
        r = _stream(argv, timeout, name, env=env)
        out.append(Gate(name, r.ok, r.output, r.seconds))
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


def _truncate(raw: str) -> str:
    return raw[:1500] + "\n[...truncated...]\n" + raw[-2500:]


# Deterministic first. A regex cannot hallucinate a failure that is not in the
# log, and cargo/clippy/rustc/terraform output is regular enough to parse.
FAILURE_PATTERNS = [
    re.compile(r"^(error(?:\[E\d+\])?|warning):\s*(?P<msg>.+)$"),
    re.compile(r"^\s*--> (?P<loc>\S+:\d+:\d+)\s*$"),
    re.compile(r"^(?P<loc>\S+) \.\.\. FAILED$"),
    re.compile(r"^\s{4}(?P<loc>[\w:]+::[\w:]+)$"),  # cargo test "failures:" block
    re.compile(r"^thread '.*' panicked at (?P<loc>\S+):"),
    re.compile(r"^Diff in (?P<loc>\S+?):\d+:$"),  # rustfmt --check
    re.compile(r"^(?:Error|╷|│ Error): (?P<msg>.+)$"),  # terraform
]


def _extract_failures(raw: str) -> list[str]:
    """Pull distinct failure lines out of a build log, preserving order."""
    out: list[str] = []
    seen: set[str] = set()
    pending: str | None = None
    for line in raw.splitlines():
        line = line.rstrip()
        m = FAILURE_PATTERNS[0].match(line)
        if m and not line.startswith("warning:"):
            pending = line.strip()
            continue
        loc = None
        for pat in FAILURE_PATTERNS[1:]:
            mm = pat.match(line)
            if mm:
                loc = mm.groupdict().get("loc") or mm.groupdict().get("msg")
                break
        if loc:
            loc = loc.replace(f"{REPO}/", "")
            entry = f"- {loc}  {pending or ''}".rstrip()
            pending = None
        elif pending and not line.strip():
            entry = f"- {pending}"
            pending = None
        else:
            continue
        key = entry.lower()
        if key not in seen:
            seen.add(key)
            out.append(entry)
    return out


def condense(gates: list[Gate], engine: Engine, budget: int = 4000) -> str:
    """Shrink gate logs before they enter an expensive prompt."""
    parts = []
    for g in gates:
        if g.ok:
            parts.append(f"### {g.name}: pass ({g.seconds:.0f}s)")
            continue
        raw, body = g.output, None
        if len(raw) > budget:
            found = _extract_failures(raw)
            if found:
                body = "\n".join(found[:60])
                body += f"\n({len(found)} distinct failure(s); full log in artifacts)"
            else:
                # Unrecognized log format — fall back to the local model, with a
                # tail that fits its context window rather than silently overflowing.
                r = engine.run(
                    "Extract every distinct failure from this log into the schema. "
                    "Deduplicate. Extract only what the log literally states — if you "
                    "cannot find a failure, return an empty list.\n\n"
                    f"--- {g.name} log (tail) ---\n{raw[-6000:]}",
                    timeout=300,
                    schema=_schema_file("gate", GATE_SCHEMA),
                )
                try:
                    items = json.loads(r.output).get("failures", []) if r.ok else []
                except (json.JSONDecodeError, AttributeError, TypeError):
                    items = []
                if items:
                    body = "\n".join(
                        f"- {i.get('location','?')}"
                        f"{'  [' + i['code'] + ']' if i.get('code') else ''}"
                        f"  {i.get('detail','')}"
                        for i in items[:60]
                    )
            body = body or _truncate(raw)
        else:
            body = raw
        parts.append(f"### {g.name}: FAIL ({g.seconds:.0f}s)\n{body}")
    return "\n\n".join(parts)


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


# --------------------------------------------------------------------------
# verdict + oscillation
# --------------------------------------------------------------------------


def parse_verdict(raw: str) -> dict:
    for candidate in (raw, *re.findall(r"```(?:json)?\s*(.*?)```", raw, re.S)):
        try:
            v = json.loads(candidate.strip())
            if isinstance(v, dict) and "verdict" in v:
                return v
        except json.JSONDecodeError:
            pass
    m = re.search(r"\{.*\"verdict\".*\}", raw, re.S)
    if m:
        try:
            return json.loads(m.group(0))
        except json.JSONDecodeError:
            pass
    # A reviewer that cannot produce a verdict is not an approval.
    ready = bool(re.search(r"\bREADY TO COMMIT\b", raw, re.I)) and not re.search(
        r"\bNOT READY\b", raw, re.I
    )
    return {
        "verdict": "READY" if ready else "NOT_READY",
        "summary": "verdict JSON unparseable; inferred from text",
        "blockers": [],
        "substeps_covered": [],
        "_unparsed": True,
    }


def repeated_blockers(prev: dict, cur: dict, thresh: float = 0.72) -> list[str]:
    """Blockers present in both rounds.

    Fuzzy, not exact: a reviewer rewording the same finding ("...caller org" ->
    "...caller org before presigning") is still the same unfixed problem, and
    exact matching would let exactly the case we care about slip through.
    """
    out: list[str] = []
    for c in cur.get("blockers", []):
        cf = c.get("file", "?")
        cp = (c.get("problem", "") or "").lower()
        for p in prev.get("blockers", []):
            if p.get("file", "?") != cf:
                continue
            if difflib.SequenceMatcher(None, cp, (p.get("problem", "") or "").lower()).ratio() >= thresh:
                out.append(f"{cf}: {c.get('problem', '')[:120]}")
                break
    return out


def check_oscillation(prev: dict | None, cur: dict, engine: Engine) -> str:
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
    )
    if hint.ok and hint.output.strip():
        warn += f"Triage hypothesis: {hint.output.strip()[:400]}\n"
    return warn


# --------------------------------------------------------------------------
# the loop
# --------------------------------------------------------------------------


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--phase", type=int, help="default: detected from git log")
    p.add_argument("--step", type=int, help="default: detected from git log")
    p.add_argument("--substep", help="e.g. S5; default: first not recorded and not committed")
    p.add_argument("--profile", choices=sorted(PROFILES), default="default")
    p.add_argument("--gates", choices=sorted(GATE_SETS), default="rust")
    p.add_argument("--max-rounds", type=int, default=3, help="review/fix cycles after implement")
    p.add_argument("--agent-timeout", type=int, default=3600, help="seconds per agent call")
    p.add_argument("--gate-timeout", type=int, default=1800, help="seconds per gate")
    p.add_argument("--local-model", help="override the Ollama model for condense/triage")
    p.add_argument("--skip-implement", action="store_true", help="review the existing tree")
    p.add_argument("--dry-run", action="store_true", help="resolve and print prompts, run nothing")
    a = p.parse_args()

    phase, step = (a.phase, a.step) if a.phase and a.step else detect_phase_step()
    guide = parse_guide(find_guide(phase, step), phase, step)

    print(f"\n=== phase {phase} step {step} — {guide.rel}")
    sub = resolve_substep(guide, a.substep)
    print(f"  target        : {sub.id} — {sub.title}  ({guide.rel}:{sub.line})")

    sha = git("rev-parse", "HEAD")
    pre = "\n".join(f"  {l}" for l in git("status", "--porcelain").splitlines())
    roles = {k: v() for k, v in PROFILES[a.profile].items()}
    if a.local_model:
        roles = {
            k: (OllamaEngine(a.local_model) if isinstance(e, OllamaEngine) else e)
            for k, e in roles.items()
        }
    print(f"  baseline      : {sha[:12]}")
    print(f"  profile       : {a.profile}")
    for r, e in roles.items():
        print(f"    {r:<12}: {e.name}")
    print(f"  gates         : {a.gates} ({len(GATE_SETS[a.gates])})")

    if a.dry_run:
        print("\n--- implement prompt ---\n")
        print(implement_prompt(guide, sub, sha, pre))
        print("\n--- review prompt (gates stubbed) ---\n")
        print(review_prompt(guide, sub, sha, "(gates not yet run)", pre))
        return 0

    run = RUNS / f"{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}-step{step}-{sub.id}"
    run.mkdir(parents=True, exist_ok=True)
    schema = run / "verdict.schema.json"
    schema.write_text(json.dumps(VERDICT_SCHEMA, indent=2), encoding="utf-8")
    (run / "baseline.diff").write_text(git("diff", "HEAD"), encoding="utf-8")
    print(f"  artifacts     : {run.relative_to(REPO)}\n")

    def save(name: str, text: str) -> None:
        (run / name).write_text(text, encoding="utf-8")

    # ---- round 0: implement
    if not a.skip_implement:
        print("=== implement")
        pr = implement_prompt(guide, sub, sha, pre)
        save("00-implement.prompt.md", pr)
        r = roles["implementer"].run(pr, a.agent_timeout)
        save("00-implement.out.md", r.output)
        if not r.ok:
            print(f"\n!! implementer failed ({r.note}); reviewing the tree as it stands")

    prev_verdict: dict | None = None
    verdict: dict = {}

    for rnd in range(1, a.max_rounds + 1):
        print(f"\n=== round {rnd}/{a.max_rounds}: gates")
        gates = run_gates(a.gates, a.gate_timeout)
        gate_text = condense(gates, roles["condenser"]) or "(no gates configured)"
        save(f"{rnd:02d}-gates.md", gate_text)
        gates_pass = all(g.ok for g in gates)

        print(f"\n=== round {rnd}/{a.max_rounds}: review")
        pr = review_prompt(guide, sub, sha, gate_text, pre)
        save(f"{rnd:02d}-review.prompt.md", pr)
        rv = roles["reviewer"].run(pr, a.agent_timeout, schema=schema)
        save(f"{rnd:02d}-review.out.json", rv.output)
        if not rv.ok and not rv.output.strip():
            print(f"\n!! reviewer failed ({rv.note}); stopping — an absent review is not a pass")
            return 2

        verdict = parse_verdict(rv.output)
        save(f"{rnd:02d}-verdict.json", json.dumps(verdict, indent=2))
        blockers = verdict.get("blockers", [])
        print(f"\n  verdict  : {verdict['verdict']}")
        print(f"  summary  : {verdict.get('summary','')[:300]}")
        for b in blockers[:12]:
            print(f"    [{b.get('severity','?'):<8}] {b.get('file','?')}: {b.get('problem','')[:110]}")
        if verdict.get("_unparsed"):
            print("  (verdict JSON was unparseable — inferred from prose, treat with suspicion)")

        if verdict["verdict"] == "READY" and gates_pass:
            print(f"\n=== {sub.id} READY after {rnd} round(s). Nothing was committed.")
            print(f"    review:  git diff HEAD && git status")
            print(f"    artifacts: {run.relative_to(REPO)}")
            return 0
        if verdict["verdict"] == "READY" and not gates_pass:
            print("\n  reviewer said READY but gates failed — gates win, continuing")

        if rnd == a.max_rounds:
            break

        print(f"\n=== round {rnd}/{a.max_rounds}: fix")
        loop_warn = check_oscillation(prev_verdict, verdict, roles["triage"])
        if loop_warn:
            print(loop_warn)
        pr = fix_prompt(guide, sub, verdict, gate_text, loop_warn)
        save(f"{rnd:02d}-fix.prompt.md", pr)
        fx = roles["fixer"].run(pr, a.agent_timeout)
        save(f"{rnd:02d}-fix.out.md", fx.output)
        if not fx.ok:
            print(f"\n!! fixer failed ({fx.note}); re-reviewing anyway")
        prev_verdict = verdict

    print(f"\n=== {sub.id} NOT READY after {a.max_rounds} round(s) — handing back to you.")
    for b in verdict.get("blockers", []):
        print(f"  [{b.get('severity','?')}] {b.get('file','?')}: {b.get('problem','')}")
    print(f"    artifacts: {run.relative_to(REPO)}")
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\ninterrupted — working tree left as-is, nothing committed")
        sys.exit(130)
