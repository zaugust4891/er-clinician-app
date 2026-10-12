"""Substep definitions and completion tracking from planning guides and git history."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

from . import config


SUBSTEP_RE = re.compile(r"^##\s+S(\d+)\s*[—–-]\s*(.+?)\s*$", re.M)

RECORD_RE = re.compile(r"^##\s+Implementation record\s*[—–-]\s*S(\d+)", re.M | re.I)

COMMIT_RE = re.compile(r"phase\s*(\d+)\s*[-–—]?\s*step\s*(\d+)\s*[-–—:]?\s*s(\d+)", re.I)


@dataclass
class Substep:
    """One objective and its original completion criteria from a guide section."""

    id: str  # "S5"
    num: int
    title: str
    line: int  # 1-indexed heading line
    text: str  # heading through the next "## "


@dataclass
class Guide:
    """A planning document plus parsed substeps and recorded completions."""

    path: Path
    phase: int
    step: int
    substeps: list[Substep]
    recorded: set[int]  # substeps with an "Implementation record" section

    @property
    def rel(self) -> str:
        return str(self.path.relative_to(config.REPO))

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
    plan = config.REPO / "docs" / "planning"
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
        ["git", *args], cwd=config.REPO, capture_output=True, text=True, check=False
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
