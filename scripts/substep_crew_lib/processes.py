"""Subprocess transport: complete transcripts, bounded excerpts, and timeout cleanup."""

from __future__ import annotations

import codecs
import os
import selectors
import signal
import subprocess
import sys
import tempfile
import time
from collections import deque
from contextlib import nullcontext
from dataclasses import dataclass
from pathlib import Path

from . import config


@dataclass
class Result:
    """Transport outcome; model adapters replace excerpts with full final responses."""

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


def run_process(
    argv: list[str],
    timeout: int,
    label: str,
    env: dict | None = None,
    *,
    log_path: Path | None = None,
    stdout_path: Path | None = None,
    input_text: str | None = None,
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

    stdin_context = tempfile.TemporaryFile() if input_text is not None else nullcontext(None)
    with log_context as log, stdout_context as stdout_capture, stdin_context as stdin:
        if stdin is not None:
            stdin.write(input_text.encode("utf-8"))
            stdin.seek(0)
        proc = subprocess.Popen(
            argv,
            cwd=config.REPO,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE if stdout_capture is not None else subprocess.STDOUT,
            stdin=stdin,
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
