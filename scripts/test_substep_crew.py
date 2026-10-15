#!/usr/bin/env python3
"""Isolated correctness tests for substep_crew; no real model sessions."""

import contextlib
import difflib
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

if __package__:
    from .substep_crew_lib import checkpoints, cli, config, engines, guides, orchestration, patches, processes, progress, prompts, protocol, validation
else:
    from substep_crew_lib import checkpoints, cli, config, engines, guides, orchestration, patches, processes, progress, prompts, protocol, validation


class ModuleAndLauncherTests(unittest.TestCase):
    def test_default_paths_still_point_to_repository_root(self):
        root = Path(__file__).resolve().parent.parent
        self.assertEqual(config.REPO, root)
        self.assertEqual(config.RUNS, root / '.agents/runs')

    def test_importing_launcher_does_not_run_cli_models_or_create_artifacts(self):
        with tempfile.TemporaryDirectory() as td, mock.patch.object(config, 'RUNS', Path(td) / 'runs'), mock.patch.object(
            cli, 'main', side_effect=AssertionError('CLI ran during import')
        ) as main, mock.patch.object(processes, 'run_process', side_effect=AssertionError('process started during import')):
            # Use the package import branch so this works in both test launch modes.
            namespace = runpy.run_module('scripts.substep_crew', run_name='imported_crew') if __package__ else runpy.run_path(
                str(Path(__file__).with_name('substep_crew.py')), run_name='imported_crew'
            )
            self.assertIs(namespace['main'], main)
            main.assert_not_called()
            self.assertFalse((Path(td) / 'runs').exists())

    def test_direct_launcher_help_from_another_directory(self):
        with tempfile.TemporaryDirectory() as td:
            result = subprocess.run(
                [sys.executable, str(Path(__file__).with_name('substep_crew.py').resolve()), '--help'],
                cwd=td, capture_output=True, text=True, timeout=10,
                env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'},
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--review-attempts', result.stdout)
        self.assertIn('--dry-run', result.stdout)

    def test_module_launcher_help(self):
        result = subprocess.run(
            [sys.executable, '-m', 'scripts.substep_crew', '--help'],
            cwd=Path(__file__).resolve().parent.parent, capture_output=True, text=True, timeout=10,
            env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--review-attempts', result.stdout)
        self.assertIn('--dry-run', result.stdout)


class FakeEngine(engines.Engine):
    def __init__(self, outputs=()):
        self.name = "fake"
        self.outputs = list(outputs)
        self.prompts = []

    def run(self, prompt, timeout, schema=None, *, log_path=None):
        self.prompts.append((prompt, schema, log_path))
        output = self.outputs.pop(0) if self.outputs else ""
        if log_path:
            log_path.parent.mkdir(parents=True, exist_ok=True)
            log_path.write_text("fake transcript", encoding="utf-8")
        return processes.Result(True, output, 0.01, log_path=log_path)


REVIEW_OK = json.dumps({
    "decision": "APPROVED", "patch": "diff", "explanation": "ok", "context_requests": [],
})

class StreamTests(unittest.TestCase):
    def test_quiet_console_keeps_complete_transcript_and_final_response(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / 'output.log'
            final = Path(td) / 'final.json'
            output = io.StringIO()
            with contextlib.redirect_stdout(output), processes.console_output(False):
                result = processes.run_process(
                    [sys.executable, '-c', 'import sys; print("model JSON"); print("diagnostic", file=sys.stderr)'],
                    5, 'model', log_path=log, stdout_path=final,
                )
            self.assertTrue(result.ok)
            self.assertEqual(output.getvalue(), '')
            self.assertEqual(final.read_text(), 'model JSON\n')
            self.assertIn('model JSON', log.read_text())
            self.assertIn('diagnostic', log.read_text())

    def test_large_prompt_is_delivered_on_stdin(self):
        payload = "🙂" * 100_000
        with contextlib.redirect_stdout(io.StringIO()):
            result = processes.run_process(
                [sys.executable, "-c", "import sys; print(len(sys.stdin.read()))"],
                5, "stdin", input_text=payload,
            )
        self.assertTrue(result.ok)
        self.assertEqual(result.output.strip(), "100000")

    def test_complete_log_bounded_excerpt_unicode_and_stderr(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "large.log"
            payload = "HEAD:" + "x" * 9000 + "🙂TAIL"
            code = (
                "import os; b=" + repr(payload.encode()) + "; "
                "os.write(1,b[:9007]); os.write(1,b[9007:]); os.write(2,b'\\nERR\\n')"
            )
            with contextlib.redirect_stdout(io.StringIO()):
                result = processes.run_process([sys.executable, "-c", code], 5, "test", log_path=log)
            self.assertTrue(result.ok)
            self.assertEqual(log.read_text(encoding="utf-8"), payload + "\nERR\n")
            self.assertLessEqual(len(result.output), processes.STREAM_HEAD_CHARS + processes.STREAM_TAIL_CHARS + 200)
            self.assertIn("complete transcript", result.output)
            self.assertIn("🙂TAIL", result.output)
            self.assertIn("ERR", result.output)

    def test_run_gates_uses_unique_complete_transcripts(self):
        with tempfile.TemporaryDirectory() as td, contextlib.redirect_stdout(io.StringIO()), mock.patch.object(
            validation, "GATE_SETS", {"fake": [("same", [sys.executable, "-c", "print('one')"]), ("same", [sys.executable, "-c", "print('two')"])]}
        ):
            gates = validation.run_gates("fake", 5, Path(td), 3)
            self.assertEqual(len({gate.log_path for gate in gates}), 2)
            self.assertEqual([gate.log_path.name for gate in gates], [
                "03-gate-01-same.transcript.log", "03-gate-02-same.transcript.log"
            ])
            self.assertEqual([gate.log_path.read_text().strip() for gate in gates], ["one", "two"])

    def test_without_log_preserves_full_output_and_exit_status(self):
        with contextlib.redirect_stdout(io.StringIO()):
            result = processes.run_process([sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr); raise SystemExit(7)"], 5, "test")
        self.assertFalse(result.ok)
        self.assertEqual(result.note, "exit 7")
        self.assertIn("out", result.output)
        self.assertIn("err", result.output)

    @unittest.skipUnless(os.name == "posix", "process-group behavior is POSIX-specific")
    def test_timeout_covers_descendant_holding_pipe_and_forces_group(self):
        with tempfile.TemporaryDirectory() as td:
            pid_file = Path(td) / "pid"
            log = Path(td) / "timeout.log"
            child = "import os,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); open(%r,'w').write(str(os.getpid())); print('child',flush=True); time.sleep(30)" % str(pid_file)
            parent = "import subprocess,sys; subprocess.Popen([sys.executable,'-c',%r]); print('parent',flush=True)" % child
            start = time.monotonic()
            with contextlib.redirect_stdout(io.StringIO()):
                result = processes.run_process([sys.executable, "-c", parent], 0.3, "test", log_path=log)
            self.assertFalse(result.ok)
            self.assertEqual(result.note, "timed out")
            self.assertLess(time.monotonic() - start, 5)
            self.assertIn("parent", log.read_text())
            deadline = time.monotonic() + 2
            pid = int(pid_file.read_text())
            while time.monotonic() < deadline:
                try:
                    state = Path(f"/proc/{pid}/stat").read_text().split()[2]
                except FileNotFoundError:
                    break
                if state == "Z":
                    break
                time.sleep(0.02)
            else:
                self.fail("unresponsive descendant survived forced process-group termination")

    @unittest.skipUnless(os.name == "posix", "process-group behavior is POSIX-specific")
    def test_keyboard_interrupt_cleans_process_group(self):
        original = processes._stream_console
        calls = 0
        def interrupt(label, text, at_line_start):
            nonlocal calls
            calls += 1
            if calls == 1:
                raise KeyboardInterrupt
            return original(label, text, at_line_start)
        with tempfile.TemporaryDirectory() as td, mock.patch.object(processes, "_stream_console", interrupt):
            log = Path(td) / "interrupt.log"
            pid_file = Path(td) / "pid"
            child = "import os,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); open(%r,'w').write(str(os.getpid())); time.sleep(30)" % str(pid_file)
            parent = "import subprocess,sys,time; c=subprocess.Popen([sys.executable,'-c',%r]); open(%r,'w').write(str(c.pid)); time.sleep(.1); print('ready',flush=True); time.sleep(30)" % (child, str(pid_file))
            with self.assertRaises(KeyboardInterrupt):
                processes.run_process([sys.executable, "-c", parent], 20, "test", log_path=log)
            pid = int(pid_file.read_text())
            deadline = time.monotonic() + 2
            state = "unknown"
            while time.monotonic() < deadline:
                try:
                    state = Path(f"/proc/{pid}/stat").read_text().split()[2]
                except FileNotFoundError:
                    state = "gone"
                if state in {"gone", "Z"}:
                    break
                time.sleep(.02)
            self.assertIn(state, {"gone", "Z"})


class ExtractionAndSummaryTests(unittest.TestCase):
    def test_pending_errors_consecutive_and_eof(self):
        found = validation._extract_failures(iter(["error: first", "error[E1]: second", "", "error: final"]))
        self.assertEqual(found, ["- error: first", "- error[E1]: second", "- error: final"])

    def test_long_line_and_diagnostic_count_are_bounded(self):
        lines = (f"error: {i}" + "x" * 10000 for i in range(100))
        found = validation._extract_failures(lines, max_diagnostics=3)
        self.assertEqual(len(found), 4)
        self.assertIn("additional distinct", found[-1])
        self.assertTrue(all(len(item) <= validation.MAX_DIAGNOSTIC_CHARS for item in found[:-1]))

    def test_condense_combined_budget_reports_omitted_gates(self):
        gates = [validation.Gate("a-long-gate-name", False, "x" * 1000, 1), validation.Gate("another-gate", True, "", 1)]
        with tempfile.TemporaryDirectory() as td, mock.patch.object(config, "RUNS", Path(td) / "runs"):
            summary = validation.condense(gates, budget=120)
        self.assertLessEqual(len(summary), 120)
        self.assertIn("additional gate", summary)

    def test_condense_reads_log_and_enforces_budget_on_malformed_model(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "gate.log"
            log.write_text("unrecognized " + "z" * 10000, encoding="utf-8")
            gate = validation.Gate("custom", False, "small excerpt", 1, log)
            engine = FakeEngine(["not json"])
            with mock.patch.object(config, "RUNS", Path(td) / "runs"):
                summary = validation.condense([gate], budget=300)
            self.assertLessEqual(len(summary), 300)
            self.assertIn("complete transcript", summary.lower())
            self.assertEqual(engine.prompts, [])


class ProgressTests(unittest.TestCase):
    def test_snapshot_has_stage_summaries_counts_and_a_stage_bar(self):
        with tempfile.TemporaryDirectory() as td:
            run = Path(td)
            output = io.StringIO()
            reporter = progress.Reporter(run, 4, 8, 'S6', 'Permissions and secrets',
                                         applied_edits=2, stream=output)
            reporter.update('patch-review', edit_number=3,
                            edit_summary='Add least-privilege database grants.',
                            review_summary='Checking ownership boundaries.', files=['src/grants.rs'])
            state = json.loads((run / 'progress.json').read_text())
            self.assertEqual(state['stage'], 'patch-review')
            self.assertEqual(state['applied_edits'], 2)
            self.assertIn('stage 2/5', (run / 'progress.md').read_text())
            self.assertIn('Current edit #3: Add least-privilege', output.getvalue())
            self.assertIn('Current review: Checking ownership', output.getvalue())
            self.assertIn('====>', progress.bar(state))
            with mock.patch.object(progress.time, 'monotonic', return_value=reporter.started + 65):
                reporter.heartbeat()
            self.assertGreaterEqual(json.loads((run / 'progress.json').read_text())['run_seconds'], 65)
            self.assertIn('runner active', output.getvalue())
            reporter.update('ready', status='ready')
            self.assertEqual(progress.bar(reporter.state), '[' + '=' * 20 + ']')

    def test_error_is_saved_and_heartbeat_thread_stops(self):
        with tempfile.TemporaryDirectory() as td:
            reporter = progress.Reporter(Path(td), 1, 1, 'S1', 'One', stream=io.StringIO(), interval=.01)
            with self.assertRaisesRegex(protocol.CrewError, 'quota'):
                with reporter:
                    raise protocol.CrewError('quota reached; reset at 9:10pm')
            state = json.loads((Path(td) / 'progress.json').read_text())
            self.assertEqual(state['status'], 'failed')
            self.assertIn('9:10pm', state['error'])
            self.assertFalse(reporter.thread.is_alive())

    def test_heartbeat_updates_the_report_while_the_main_thread_waits(self):
        with tempfile.TemporaryDirectory() as td:
            reporter = progress.Reporter(Path(td), 1, 1, 'S1', 'One', stream=io.StringIO(), interval=.01)
            updated = progress.threading.Event()
            heartbeat = reporter.heartbeat
            def observed_heartbeat():
                heartbeat()
                updated.set()
            with mock.patch.object(reporter, 'heartbeat', side_effect=observed_heartbeat), reporter:
                self.assertTrue(updated.wait(timeout=2))
                state = json.loads((Path(td) / 'progress.json').read_text())
                self.assertGreater(state['run_seconds'], 0)
                reporter.update('ready', status='ready')

    @unittest.skipUnless(os.name == 'posix', 'POSIX PID liveness')
    def test_watch_reports_a_dead_runner_without_writing_its_snapshot(self):
        with tempfile.TemporaryDirectory() as td, mock.patch.object(config, 'RUNS', Path(td)):
            run = Path(td) / 'run'
            run.mkdir()
            progress.Reporter(run, 1, 1, 'S1', 'One', stream=io.StringIO()).update()
            before = (run / 'progress.json').read_bytes()
            with mock.patch.object(progress.os, 'kill', side_effect=ProcessLookupError), contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(progress.monitor(str(run), watch=True), 0)
            self.assertIn('INTERRUPTED', output.getvalue())
            self.assertEqual((run / 'progress.json').read_bytes(), before)

    def test_interrupting_watch_only_stops_the_monitor(self):
        with tempfile.TemporaryDirectory() as td, mock.patch.object(config, 'RUNS', Path(td)):
            run = Path(td) / 'run'
            run.mkdir()
            progress.Reporter(run, 1, 1, 'S1', 'One', stream=io.StringIO()).update()
            before = (run / 'progress.json').read_bytes()
            with mock.patch.object(progress.time, 'sleep', side_effect=KeyboardInterrupt), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(progress.monitor(str(run), watch=True), 0)
            self.assertEqual((run / 'progress.json').read_bytes(), before)

    def test_status_does_not_mistake_another_pid_namespace_for_a_dead_runner(self):
        with tempfile.TemporaryDirectory() as td, mock.patch.object(config, 'RUNS', Path(td)):
            run = Path(td) / 'run'
            run.mkdir()
            reporter = progress.Reporter(run, 1, 1, 'S1', 'One', stream=io.StringIO())
            reporter.state['pid_namespace'] = 'source-namespace'
            reporter.update()
            with mock.patch.object(progress, 'process_namespace', return_value='monitor-namespace'), mock.patch.object(
                progress.os, 'kill', side_effect=AssertionError('probed a PID in another namespace')
            ), contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(progress.monitor(str(run)), 0)
            self.assertIn('RUNNING', output.getvalue())

    def test_status_and_watch_do_not_invoke_agents_or_inspect_git(self):
        with tempfile.TemporaryDirectory() as td, mock.patch.object(config, 'RUNS', Path(td)):
            run = Path(td) / '20260101-run'
            run.mkdir()
            progress.Reporter(run, 1, 1, 'S1', 'One', stream=io.StringIO()).update('ready', status='ready')
            for flag in ('--status', '--watch'):
                with self.subTest(flag=flag), contextlib.redirect_stdout(io.StringIO()) as output, mock.patch.object(
                    sys, 'argv', ['substep_crew.py', flag]
                ), mock.patch.object(guides, 'git', side_effect=AssertionError('monitor inspected git')), mock.patch.object(
                    processes, 'run_process', side_effect=AssertionError('monitor started a process')
                ):
                    self.assertEqual(cli.main(), 0)
                    self.assertIn('READY', output.getvalue())

    def test_summary_removes_terminal_control_characters(self):
        value = progress.summary('\x1b[31mReview\x1b[0m\nnext\x00step')
        self.assertEqual(value, 'Review nextstep')


class ParserAndMatchingTests(unittest.TestCase):
    def test_guide_lines_and_sections(self):
        body = "preface\n\n## S1 — One\na\n\n## S2 - Two\nb\n## Other\nc\n## Implementation record — S1\n"
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "guide.md"
            path.write_text("different", encoding="utf-8")
            guide = guides.parse_guide(path, 4, 1, body)
        self.assertEqual([(s.id, s.line, s.text) for s in guide.substeps], [
            ("S1", 3, "## S1 — One\na"),
            ("S2", 6, "## S2 - Two\nb"),
        ])
        self.assertEqual(guide.recorded, {1})

    def test_positional_result_and_gate_compatibility(self):
        self.assertEqual(processes.Result(False, "x", 1, "note").note, "note")
        self.assertIsNone(validation.Gate("g", True, "", 1).log_path)


def text_patch(name, before, after):
    return ''.join(difflib.unified_diff(
        [] if before is None else before.splitlines(keepends=True),
        [] if after is None else after.splitlines(keepends=True),
        fromfile='/dev/null' if before is None else 'a/' + name,
        tofile='/dev/null' if after is None else 'b/' + name,
    ))


def git_patch(name, before, after):
    mode = 'new file mode 100644\n' if before is None else 'deleted file mode 100644\n' if after is None else ''
    return f'diff --git a/{name} b/{name}\n' + mode + text_patch(name, before, after)


# Candidate workflow integration and adapter tests live in test_candidate_crew.py.

if __name__ == "__main__":
    unittest.main(verbosity=2)
