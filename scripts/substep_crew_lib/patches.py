"""Git patch checks, safe repository paths, state fingerprints, and approval receipts."""

from __future__ import annotations

import hashlib
import os
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

from . import config, protocol


def checked_git(*args: str, input_text: str | None = None, repo: Path | None = None) -> bytes:
    try:
        result = subprocess.run(
            ["git", *args], cwd=repo or config.REPO,
            input=input_text.encode("utf-8") if input_text is not None else None,
            capture_output=True, timeout=30,
            env={**{key: value for key, value in os.environ.items() if not key.startswith('GIT_')},
                 "GIT_OPTIONAL_LOCKS": "0"},
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise protocol.CrewError(f"git {' '.join(args)} failed: {exc}") from exc
    if result.returncode:
        raise protocol.CrewError(f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def safe_path(name: str, guide_path: str, *, repo: Path | None = None) -> Path:
    root = repo or config.REPO
    relative = Path(name)
    if (
        not name or relative.is_absolute() or '..' in relative.parts
        or str(relative) != name or '\\' in name
        or any(part.lower() == '.git' for part in relative.parts)
        or relative.parts[:2] == ('.agents', 'runs')
    ):
        raise protocol.CrewError(f"unsafe patch/context path: {name!r}")
    path = root / relative
    if not path.resolve().is_relative_to(root.resolve()) or any(
        parent.is_symlink() for parent in [path, *path.parents] if parent != root
    ):
        raise protocol.CrewError(f"symlink patch/context path is unsupported: {name}")
    if path.exists() and not path.is_file():
        raise protocol.CrewError(f"patch/context path is not a regular file: {name}")
    # Avoid source edits to ignored build products and local configuration. The
    # selected guide is often intentionally ignored but still must be updated.
    ignored = subprocess.run(
        ["git", "check-ignore", "-q", "--", name], cwd=root,
        capture_output=True, timeout=30,
        env={key: value for key, value in os.environ.items() if not key.startswith('GIT_')},
    )
    if ignored.returncode not in (0, 1):
        raise protocol.CrewError(f"cannot check ignored path: {name}")
    if ignored.returncode == 0 and name != guide_path:
        raise protocol.CrewError(f"ignored patch/context path is unsupported: {name}")
    return path


def inspect_patch(patch: str, guide_path: str, *, check: bool = True) -> tuple[list[str], tuple[str, ...]]:
    if not patch.strip() or len(patch) > 200_000 or '\x00' in patch:
        raise protocol.ProtocolError("patch must be non-empty UTF-8 text, at most 200,000 characters")
    if re.search(
        r"^(?:GIT binary patch|Binary files |rename from |rename to |copy from |copy to |"
        r"(?:old mode|new mode|new file mode|deleted file mode) (?!100644$|100755$)|"
        r"index \S+ (?!100644$|100755$)\d{6}$)",
        patch, re.M,
    ):
        raise protocol.ProtocolError("binary, symlink, submodule, rename/copy diffs are unsupported; use text delete/add")
    error = None
    # Recount is a fallback: git's recount parser does not reliably distinguish
    # adjacent traditional (non-diff--git) file headers in otherwise valid diffs.
    for flags in ((), ('--recount',)):
        try:
            stats = checked_git('apply', *flags, '--numstat', '-z', input_text=patch)
        except protocol.CrewError as exc:
            error = exc
            continue
        try:
            names = []
            for entry in stats.split(b'\x00'):
                if not entry:
                    continue
                added, removed, name = entry.decode('utf-8').split('\t', 2)
                if not added.isdigit() or not removed.isdigit() or not name:
                    raise protocol.ProtocolError('patch must contain regular text file changes')
                safe_path(name, guide_path)
                names.append(name)
            if not names or len(names) > 32 or len(set(names)) != len(names):
                raise protocol.ProtocolError('patch must contain 1 to 32 distinct regular files')
        except (protocol.CrewError, UnicodeError, ValueError) as exc:
            raise protocol.ProtocolError(f'invalid or inapplicable patch: {exc}') from exc
        if check:
            try:
                checked_git('apply', *flags, '--check', '--whitespace=nowarn', input_text=patch)
            except protocol.CrewError as exc:
                error = exc
                continue
        return names, flags
    raise protocol.ProtocolError(f'invalid or inapplicable patch: {error}') from error


def patch_paths(patch: str, guide_path: str, *, check: bool = True) -> list[str]:
    return inspect_patch(patch, guide_path, check=check)[0]


def repository_state(extra_paths=()) -> str:
    """Hash HEAD, the index and all tracked/nonignored files, including deletions.

    Run artifacts are excluded even in repositories without the harness ignore
    rule. Known context files and the selected (possibly ignored) guide are added.
    Git status alone is insufficient: two dirty versions have identical status.
    """
    digest = hashlib.sha256()
    digest.update(checked_git("rev-parse", "HEAD"))
    digest.update(checked_git("ls-files", "--stage", "-z"))
    entries = checked_git("ls-files", "--cached", "--others", "--exclude-standard", "-z")
    names = {os.fsdecode(entry) for entry in entries.split(b"\x00") if entry}
    names.update(extra_paths)
    for name in sorted(names):
        if Path(name).parts[:2] == ('.agents', 'runs'):
            continue
        path = config.REPO / name
        digest.update(os.fsencode(name) + b"\x00")
        try:
            status = path.lstat()
        except FileNotFoundError:
            digest.update(b"missing\x00")
            continue
        digest.update(str(status.st_mode).encode() + b"\x00")
        if path.is_symlink():
            digest.update(os.fsencode(os.readlink(path)))
        elif path.is_file():
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(64 * 1024), b""):
                    digest.update(chunk)
        else:
            raise protocol.CrewError(f"snapshot requires regular files: {name}")
        digest.update(b"\x00")
    return digest.hexdigest()


@dataclass(frozen=True)
class ReviewedPatch:
    """Immutable, single-use approval tied to exact patch text and a repository state."""

    patch: str
    state: str
    decision: str
    explanation: str
