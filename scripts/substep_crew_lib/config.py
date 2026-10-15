"""Shared repository paths, artifact paths, and Claude authentication home."""

from __future__ import annotations

import os
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]

RUNS = REPO / ".agents" / "runs"


def default_claude_config_dir() -> Path:
    """Prefer the personal Claude home when present; honor an explicit override."""
    configured = os.environ.get("CLAUDE_CONFIG_DIR")
    if configured:
        return Path(configured).expanduser()
    personal = Path.home() / ".claude-personal"
    return personal if personal.is_dir() else Path.home() / ".claude"
