#!/usr/bin/env python3
"""Isolated correctness tests for substep_crew; no real model sessions."""

import contextlib
import difflib
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
    from .substep_crew_lib import cli, config, engines, guides, orchestration, patches, processes, protocol, validation
    from . import benchmark_substep_crew as benchmarks
else:
    from substep_crew_lib import cli, config, engines, guides, orchestration, patches, processes, protocol, validation
    import benchmark_substep_crew as benchmarks


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
            summary = validation.condense(gates, FakeEngine(), budget=120)
        self.assertLessEqual(len(summary), 120)
        self.assertIn("additional gate", summary)

    def test_condense_reads_log_and_enforces_budget_on_malformed_model(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "gate.log"
            log.write_text("unrecognized " + "z" * 10000, encoding="utf-8")
            gate = validation.Gate("custom", False, "small excerpt", 1, log)
            engine = FakeEngine(["not json"])
            with mock.patch.object(config, "RUNS", Path(td) / "runs"):
                summary = validation.condense([gate], engine, budget=300, helper_log_path=Path(td) / "helper.transcript.log")
            self.assertLessEqual(len(summary), 300)
            self.assertIn("complete transcript", summary.lower())


class EngineTests(unittest.TestCase):
    def test_claude_config_prefers_override_then_personal_then_default(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            personal = home / ".claude-personal"
            fallback = home / ".claude"
            fallback.mkdir()

            with mock.patch.object(Path, "home", return_value=home), mock.patch.dict(
                os.environ, {}, clear=True
            ):
                self.assertEqual(config.default_claude_config_dir(), fallback)
                personal.mkdir()
                self.assertEqual(config.default_claude_config_dir(), personal)

                override = home / "explicit-claude-home"
                os.environ["CLAUDE_CONFIG_DIR"] = str(override)
                self.assertEqual(config.default_claude_config_dir(), override)

    def test_claude_schema_and_complete_final_response(self):
        with tempfile.TemporaryDirectory() as td:
            transcript = Path(td) / "01-review.transcript.log"
            schema = Path(td) / "schema.json"
            schema.write_text(json.dumps(protocol.PATCH_REVIEW_SCHEMA))
            # Use a simple valid object for the engine transport test; verdict validation is separate.
            final = json.dumps({"payload": "x" * 8000})
            captured = {}
            def fake_stream(argv, timeout, label, env=None, *, log_path=None, stdout_path=None, input_text=None):
                captured["argv"] = argv
                captured["env"] = env
                captured["input_text"] = input_text
                Path(log_path).write_text("stderr diagnostic\n" + final, encoding="utf-8")
                Path(stdout_path).write_text(final, encoding="utf-8")
                return processes.Result(True, "bounded excerpt", .1, log_path=Path(log_path))
            with mock.patch.object(engines.shutil, "which", return_value="/bin/claude"), mock.patch.object(processes, "run_process", fake_stream):
                result = engines.ClaudeEngine(permission_mode="plan").run("review", 5, schema=schema, log_path=transcript)
            self.assertEqual(result.output, final)
            self.assertIn("--json-schema", captured["argv"])
            self.assertEqual(captured["input_text"], "review")
            self.assertEqual(captured["argv"][captured["argv"].index("--tools") + 1], "Read,Glob,Grep")
            self.assertEqual(captured["argv"][captured["argv"].index("--disallowedTools") + 1], "mcp__*")
            self.assertIn("--safe-mode", captured["argv"])
            self.assertIn("--strict-mcp-config", captured["argv"])
            self.assertIn('{"disableAllHooks":true}', captured["argv"])
            self.assertEqual(captured["env"]["CLAUDE_CONFIG_DIR"], str(config.default_claude_config_dir()))
            self.assertEqual(captured["env"]["PAI_DIR"], str(config.default_claude_config_dir()))
            final_path = Path(td) / "01-review.final.json"
            self.assertTrue(final_path.exists())
            self.assertNotEqual(transcript.read_text(), final_path.read_text())

    def test_codex_complete_final_message_is_not_excerpt(self):
        with tempfile.TemporaryDirectory() as td:
            transcript = Path(td) / "01-review.transcript.log"
            final = json.dumps({"decision": "APPROVED", "patch": "diff", "explanation": "x" * 8000, "context_requests": []})
            captured = {}
            def fake_stream(argv, timeout, label, env=None, *, log_path=None, input_text=None):
                captured['argv'], captured['input_text'] = argv, input_text
                output_path = Path(argv[argv.index("-o") + 1])
                output_path.write_text(final, encoding="utf-8")
                Path(log_path).write_text("event" * 3000, encoding="utf-8")
                return processes.Result(True, "bounded excerpt", .1, log_path=Path(log_path))
            with mock.patch.object(engines.shutil, "which", return_value="/bin/codex"), mock.patch.object(processes, "run_process", fake_stream):
                result = engines.CodexEngine().run("review", 5, log_path=transcript)
            self.assertEqual(result.output, final)
            self.assertEqual(protocol.parse_patch_review(result.output, "diff")["decision"], "APPROVED")
            self.assertTrue((Path(td) / "01-review.final.md").exists())
            self.assertEqual(captured['argv'][captured['argv'].index('-s') + 1], 'read-only')
            self.assertEqual(captured['input_text'], 'review')
            for setting in ('approval_policy="never"', 'mcp_servers={}', 'plugins={}', 'hooks={}', 'notify=[]', 'features.apps=false'):
                self.assertIn(setting, captured['argv'])

    def test_claude_structured_output_envelope_and_error_responses(self):
        for envelope, expected_ok in (
            ({'type': 'result', 'subtype': 'success', 'is_error': False,
              'structured_output': json.loads(REVIEW_OK)}, True),
            ({'type': 'result', 'subtype': 'error_max_turns', 'is_error': True,
              'structured_output': json.loads(REVIEW_OK)}, False),
            ({'type': 'result', 'subtype': 'success', 'is_error': False}, False),
        ):
            with self.subTest(envelope=envelope), tempfile.TemporaryDirectory() as td:
                schema = Path(td) / 'schema.json'
                schema.write_text(json.dumps(protocol.PATCH_REVIEW_SCHEMA))
                raw = json.dumps(envelope)
                def fake_stream(*args, **kwargs):
                    kwargs['stdout_path'].write_text(raw)
                    return processes.Result(True, 'excerpt', .1)
                with mock.patch.object(engines.shutil, 'which', return_value='/bin/claude'), mock.patch.object(processes, 'run_process', fake_stream):
                    result = engines.ClaudeEngine().run('review', 5, schema=schema,
                                                   log_path=Path(td) / 'review.transcript.log')
                self.assertEqual(result.ok, expected_ok)
                if expected_ok:
                    self.assertEqual(json.loads(result.output), json.loads(REVIEW_OK))
                self.assertEqual((Path(td) / 'review.final.json').read_text(), raw)

    def test_codex_rejects_write_enabled_sandbox_before_invocation(self):
        with mock.patch.object(engines.shutil, 'which', return_value='/bin/codex'), mock.patch.object(processes, 'run_process') as stream:
            result = engines.CodexEngine(sandbox='workspace-write').run('proposal', 5)
        self.assertFalse(result.ok)
        stream.assert_not_called()

    def test_profiles_all_use_restricted_author_and_reviewer_engines(self):
        for profile in engines.PROFILES.values():
            for role in ('implementer', 'reviewer'):
                engine = profile[role]()
                if isinstance(engine, engines.CodexEngine):
                    self.assertEqual(engine.sandbox, 'read-only')
                else:
                    self.assertIsInstance(engine, engines.ClaudeEngine)
                    self.assertIn(engine.permission_mode, ('dontAsk', 'plan'))


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

    def test_indexed_fuzzy_matching_matches_reference(self):
        previous = {"blockers": [
            {"file":"a.py", "problem":"validate caller organization before presigning"},
            {"file":"b.py", "problem":"close the inherited output pipe on timeout"},
        ]}
        current = {"blockers": [
            {"file":"a.py", "problem":"validate caller org before presigning"},
            {"file":"b.py", "problem":"unrelated wording"},
        ]}
        expected = []
        import difflib
        for c in current["blockers"]:
            for p in previous["blockers"]:
                if p["file"] == c["file"] and difflib.SequenceMatcher(None, c["problem"].lower(), p["problem"].lower()).ratio() >= .72:
                    expected.append(f"{c['file']}: {c['problem'][:120]}")
                    break
        self.assertEqual(benchmarks.repeated_blockers(previous, current), expected)

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


def proposal(patch, **kwargs):
    return json.dumps({
        'action': 'PATCH', 'patch': patch, 'explanation': 'Coherent implementation change',
        'context': 'Current substep context', 'context_paths': [], 'completion_evidence': [],
        **kwargs,
    })


def application(patch, **kwargs):
    return json.dumps({'action': 'APPLY', 'patch': patch, 'explanation': 'Apply returned patch', **kwargs})


def approval(patch, **kwargs):
    return json.dumps({'decision': 'APPROVED', 'patch': patch, 'explanation': 'Correct', 'context_requests': [], **kwargs})


COMPLETE = proposal('', action='COMPLETE', completion_evidence=[
    {'criterion': 'Substep objective implemented and documented', 'evidence': 'Inspected current source, guide record and gate results'},
])


class ScriptedEngine(FakeEngine):
    """Each callable observes the real temp working tree before returning a response."""
    def run(self, prompt, timeout, schema=None, *, log_path=None):
        self.prompts.append((prompt, schema, log_path))
        if not self.outputs:
            raise AssertionError('unexpected model invocation: ' + prompt[-200:])
        response = self.outputs.pop(0)
        if callable(response):
            response = response(prompt)
        if isinstance(response, BaseException):
            raise response
        if isinstance(response, processes.Result):
            return response
        return processes.Result(True, response, .01, log_path=log_path)


class PatchLoopTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.guide_path = self.root / 'guide.md'
        self.guide_path.write_text('## S1 — One\nImplement value handling.\n## Implementation record — S1\nBaseline.\n')
        (self.root / 'a.py').write_text('value = 0\n')
        (self.root / '.gitignore').write_text('.agents/runs/\ntarget/\n')
        for args in (
            ['init', '-q'], ['add', '.'],
            ['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
             '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'phase 1 step 1 s0'],
        ):
            subprocess.run(['git', *args], cwd=self.root, check=True, capture_output=True)
        self.repo_patch = mock.patch.object(config, 'REPO', self.root)
        self.repo_patch.start()
        self.addCleanup(self.repo_patch.stop)
        self.runs_patch = mock.patch.object(config, 'RUNS', self.root / '.agents/runs')
        self.runs_patch.start()
        self.addCleanup(self.runs_patch.stop)
        self.console = contextlib.redirect_stdout(io.StringIO())
        self.console.__enter__()
        self.addCleanup(self.console.__exit__, None, None, None)
        self.first = text_patch('a.py', 'value = 0\n', 'value = 1\n')
        self.second = text_patch('a.py', 'value = 1\n', 'value = 2\n')
        self.gate_states = []
        self.applied_bytes = []
        original = patches.checked_git
        def checked(*args, **kwargs):
            if args == ('apply', '--whitespace=nowarn'):
                self.applied_bytes.append(kwargs['input_text'])
            return original(*args, **kwargs)
        self.git_patch = mock.patch.object(patches, 'checked_git', side_effect=checked)
        self.git_patch.start()
        self.addCleanup(self.git_patch.stop)

    def make_loop(self, author, reviewer, *, attempts=3, max_edits=4, max_rounds=3, gate_results=()):
        run = self.root / '.agents/runs' / str(time.time_ns())
        run.mkdir(parents=True)
        guide = guides.parse_guide(self.guide_path, 1, 1)
        self.author = ScriptedEngine(author)
        self.reviewer = ScriptedEngine(reviewer)
        results = iter(gate_results)
        def gates(names, timeout, run_dir, round_num):
            self.gate_states.append((self.root / 'a.py').read_text())
            ok = next(results, True)
            return [] if names == 'none' else [validation.Gate('test', ok, 'ok' if ok else 'error: failed', .01)]
        self.gates = mock.patch.object(validation, 'run_gates', side_effect=gates)
        self.gates.start()
        self.addCleanup(self.gates.stop)
        return orchestration.PatchLoop(
            guide, guide.get('S1'), guides.git('rev-parse', 'HEAD'), '',
            {'implementer': self.author, 'reviewer': self.reviewer, 'condenser': FakeEngine()},
            run, agent_timeout=5, gate_timeout=5, gate_set='rust',
            attempts=attempts, max_edits=max_edits, max_rounds=max_rounds,
        )

    def observe(self, expected, response, contains=()):
        def answer(prompt):
            self.assertEqual((self.root / 'a.py').read_text(), expected)
            for text in contains:
                self.assertIn(text, prompt)
            return response
        return answer

    def test_review_precedes_write_and_approved_patch_is_unchanged(self):
        loop = self.make_loop(
            [proposal(self.first), self.observe('value = 0\n', application(self.first)), COMPLETE],
            [self.observe('value = 0\n', approval(self.first), ('existing_code', 'value = 0', 'Implement value handling'))],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.applied_bytes, [self.first])
        self.assertEqual((loop.run_dir / '001-applied.patch').read_bytes(), self.first.encode())
        self.assertEqual(self.gate_states, ['value = 1\n'])
        self.assertEqual(loop.history[0]['decision'], 'APPROVED')

    def test_revised_multifile_patch_is_applied_exactly(self):
        revised = text_patch('a.py', 'value = 0\n', 'value = 7\n')
        revised += text_patch('helper.py', None, 'def validate(value):\n    return value >= 0\n')
        loop = self.make_loop(
            [proposal(self.first), self.observe('value = 0\n', application(revised), ('value = 7', 'helper.py')), COMPLETE],
            [approval(revised, decision='REVISED', explanation='Include validation and integration')],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.applied_bytes, [revised])
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 7\n')
        self.assertEqual((self.root / 'helper.py').read_text(), 'def validate(value):\n    return value >= 0\n')
        self.assertEqual(loop.history[0]['patch'], revised)

    def test_successive_edits_see_updated_code_history_and_validation(self):
        loop = self.make_loop(
            [proposal(self.first), application(self.first),
             self.observe('value = 1\n', proposal(self.second), ('earlier_applied_changes', '### test: pass', 'resulting_state')),
             application(self.second), COMPLETE],
            [approval(self.first), self.observe('value = 1\n', approval(self.second), ('value = 1', 'earlier_applied_changes'))],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.applied_bytes, [self.first, self.second])
        self.assertEqual(self.gate_states, ['value = 1\n', 'value = 2\n'])
        self.assertEqual(loop.history[0]['resulting_state'], loop.history[1]['base_state'])

    def test_author_modification_is_reviewed_again_before_any_write(self):
        changed = text_patch('a.py', 'value = 0\n', 'value = 4\n')
        loop = self.make_loop(
            [proposal(self.first), application(changed, action='REPROPOSE', explanation='Improve implementation'),
             application(changed), COMPLETE],
            [approval(self.first), self.observe('value = 0\n', approval(changed), ('value = 4', 'Improve implementation'))],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.reviewer.prompts), 2)
        self.assertEqual(self.applied_bytes, [changed])

    def test_altered_apply_action_cannot_bypass_review(self):
        changed = text_patch('a.py', 'value = 0\n', 'value = 9\n')
        loop = self.make_loop([proposal(self.first)] + [application(changed)] * 3, [approval(self.first)])
        with self.assertRaisesRegex(protocol.CrewError, 'modified patch must use REPROPOSE'):
            loop.run()
        self.assertEqual(self.applied_bytes, [])
        self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_missing_context_is_supplied_before_implementation(self):
        (self.root / 'helper.py').write_text('MINIMUM = 0\n')
        needs = approval('', decision='NEEDS_CONTEXT', context_requests=['What is the minimum value?'])
        context = json.dumps({'context': 'helper.py defines MINIMUM = 0', 'context_paths': ['helper.py']})
        loop = self.make_loop(
            [proposal(self.first), self.observe('value = 0\n', context, ('What is the minimum value?',)),
             application(self.first), COMPLETE],
            [needs, self.observe('value = 0\n', approval(self.first), ('MINIMUM = 0', 'helper.py'))],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.reviewer.prompts), 2)
        self.assertEqual(self.applied_bytes, [self.first])

    def test_unresolved_context_requests_exhaust_bound_without_writes(self):
        needs = approval('', decision='NEEDS_CONTEXT', context_requests=['Need dependency evidence'])
        context = json.dumps({'context': 'Still investigating', 'context_paths': []})
        loop = self.make_loop([proposal(self.first)] + [context] * 3, [needs] * 3)
        with self.assertRaisesRegex(protocol.CrewError, 'exhausted 3'):
            loop.run()
        self.assertEqual(len(self.reviewer.prompts), 3)
        self.assertEqual(self.applied_bytes, [])

    def test_failed_and_timed_out_review_invocations_prevent_writes(self):
        for response in (
            processes.Result(False, approval(self.first), .1, 'exit 1'),
            processes.Result(False, approval(self.first), .1, 'timed out'),
            TimeoutError('review timeout'),
        ):
            with self.subTest(response=response):
                loop = self.make_loop([proposal(self.first)], [response] * 3)
                with self.assertRaisesRegex(protocol.CrewError, 'exhausted 3'):
                    loop.run()
                self.assertEqual(len(self.reviewer.prompts), 3)
                self.assertEqual(len(self.author.prompts), 1)
                self.assertEqual(self.applied_bytes, [])
                self.assertEqual((self.root / 'a.py').read_text(), 'value = 0\n')

    def test_malformed_and_inapplicable_reviews_fail_closed(self):
        invalid = [
            'APPROVED', '{"decision":"APPROVED"}', '```json\n' + approval(self.first) + '\n```',
            approval(self.second), approval(self.first, extra='unexpected'),
            approval(self.first, decision='NEEDS_CONTEXT', context_requests=['need code']),
            approval(self.first, decision='REVISED', explanation='No actual revision'),
            approval(self.second, decision='REVISED', explanation=''),
            approval('not a diff', decision='REVISED', explanation='Incomplete patch'),
            '{"decision":"REVISED",' + approval(self.first)[1:],
        ]
        for response in invalid:
            with self.subTest(response=response):
                loop = self.make_loop([proposal(self.first)], [response] * 3)
                with self.assertRaisesRegex(protocol.CrewError, 'exhausted 3'):
                    loop.run()
                self.assertEqual(self.applied_bytes, [])
                self.assertEqual(len(self.author.prompts), 1)

    def test_transient_bad_review_retries_same_patch_without_early_write(self):
        loop = self.make_loop([proposal(self.first), application(self.first), COMPLETE],
                              ['not JSON', self.observe('value = 0\n', approval(self.first), ('Previous review failed',))])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(len(self.reviewer.prompts), 2)
        self.assertEqual(self.applied_bytes, [self.first])

    def test_stale_patch_during_review_or_application_is_never_applied(self):
        for stage in ('review', 'application'):
            with self.subTest(stage=stage):
                (self.root / 'a.py').write_text('value = 0\n')
                def mutate(prompt):
                    (self.root / 'a.py').write_text('value = 99\n')
                    return approval(self.first) if stage == 'review' else application(self.first)
                author = [proposal(self.first), mutate] if stage == 'application' else [proposal(self.first)]
                review = [mutate] if stage == 'review' else [approval(self.first)]
                loop = self.make_loop(author, review)
                with self.assertRaisesRegex(protocol.CrewError, 'stale patch'):
                    loop.run()
                self.assertEqual(self.applied_bytes, [])
                self.assertEqual((self.root / 'a.py').read_text(), 'value = 99\n')

    def test_unrelated_change_also_invalidates_review(self):
        def mutate(prompt):
            (self.root / 'new_dependency.py').write_text('changed = True\n')
            return approval(self.first)
        loop = self.make_loop([proposal(self.first)], [mutate])
        with self.assertRaisesRegex(protocol.CrewError, 'stale patch'):
            loop.run()
        self.assertEqual(self.applied_bytes, [])

    def test_proposal_cannot_write_before_review(self):
        def mutate(prompt):
            (self.root / 'a.py').write_text('value = 99\n')
            return proposal(self.first)
        loop = self.make_loop([mutate], [])
        with self.assertRaisesRegex(protocol.CrewError, 'stale patch'):
            loop.run()
        self.assertEqual(self.reviewer.prompts, [])
        self.assertEqual(self.applied_bytes, [])

    def test_write_boundary_refuses_missing_mismatched_stale_and_reused_approval(self):
        loop = self.make_loop([], [])
        record = patches.ReviewedPatch(self.first, loop.state(), 'APPROVED', 'ok')
        with self.assertRaisesRegex(protocol.CrewError, 'missing approval'):
            loop.apply_reviewed(record, self.first)
        loop.pending = record
        with self.assertRaisesRegex(protocol.CrewError, 'differs from reviewed bytes'):
            loop.apply_reviewed(record, self.first + '\n')
        self.assertEqual(self.applied_bytes, [])
        loop.apply_reviewed(record, self.first)
        with self.assertRaisesRegex(protocol.CrewError, 'missing approval'):
            loop.apply_reviewed(record, self.first)
        self.assertEqual(self.applied_bytes, [self.first])

    def test_invalid_proposal_and_unsafe_revised_paths_prevent_writes(self):
        unsafe = text_patch('../outside.py', None, 'unsafe\n')
        loop = self.make_loop([proposal(self.first)], [approval(unsafe, decision='REVISED', explanation='unsafe')] * 3)
        with self.assertRaisesRegex(protocol.CrewError, 'exhausted 3'):
            loop.run()
        self.assertFalse((self.root.parent / 'outside.py').exists())
        self.assertEqual(self.applied_bytes, [])

    def test_invalid_proposal_retries_author_before_codex_or_writes(self):
        loop = self.make_loop(
            [proposal('invalid diff'), proposal(self.first), application(self.first), COMPLETE],
            [approval(self.first)],
        )
        self.assertEqual(loop.run(), 0)
        self.assertIn('Previous response failed', self.author.prompts[1][0])
        self.assertEqual(len(self.reviewer.prompts), 1)
        self.assertEqual(self.applied_bytes, [self.first])

    def test_binary_symlink_ignored_and_artifact_changes_are_refused(self):
        patches = [
            text_patch('.agents/runs/ticket.py', None, 'fake approval\n'),
            text_patch('target/generated.py', None, 'ignored\n'),
            'diff --git a/link b/link\nnew file mode 120000\n--- /dev/null\n+++ b/link\n@@ -0,0 +1 @@\n+/tmp/outside\n',
            'diff --git a/a.py b/a.py\nGIT binary patch\nliteral 1\nX\n',
        ]
        for patch in patches:
            with self.subTest(patch=patch):
                loop = self.make_loop([proposal(patch)] * 3, [])
                with self.assertRaisesRegex(protocol.CrewError, 'failed after 3'):
                    loop.run()
                self.assertEqual(self.reviewer.prompts, [])
                self.assertEqual(self.applied_bytes, [])

    def test_regular_file_deletion_and_trailing_whitespace_are_exact(self):
        (self.root / 'obsolete.py').write_text('obsolete = True\n')
        patch = text_patch('a.py', 'value = 0\n', 'value = 1  \n')
        patch += text_patch('obsolete.py', 'obsolete = True\n', None)
        loop = self.make_loop([proposal(patch), application(patch), COMPLETE], [approval(patch)])
        self.assertEqual(loop.run(), 0)
        self.assertFalse((self.root / 'obsolete.py').exists())
        self.assertEqual((self.root / 'a.py').read_bytes(), b'value = 1  \n')
        self.assertEqual(self.applied_bytes, [patch])

    def test_next_edit_is_refused_until_previous_edit_has_been_validated(self):
        loop = self.make_loop([], [])
        first = patches.ReviewedPatch(self.first, loop.state(), 'APPROVED', 'ok')
        loop.pending = first
        loop.apply_reviewed(first, self.first)
        second = patches.ReviewedPatch(self.second, loop.state(), 'APPROVED', 'ok')
        loop.pending = second
        with self.assertRaisesRegex(protocol.CrewError, 'validate the previously applied patch'):
            loop.apply_reviewed(second, self.second)
        with self.assertRaisesRegex(protocol.CrewError, 'validate the previously applied patch'):
            loop.review_and_apply(json.loads(proposal(self.second)), loop.state())
        self.assertEqual(self.applied_bytes, [self.first])

    def test_change_between_model_calls_invalidates_existing_validation(self):
        loop = self.make_loop([COMPLETE], [])
        loop.validate()
        (self.root / 'a.py').write_text('value = 99\n')
        with self.assertRaisesRegex(protocol.CrewError, 'stale patch'):
            loop.run()
        self.assertEqual(self.author.prompts, [])
        self.assertEqual(self.applied_bytes, [])

    def test_unchanged_tree_requires_completion_evidence_after_validation(self):
        loop = self.make_loop(
            [COMPLETE, self.observe('value = 0\n', COMPLETE, ('### test: pass',))], [],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.gate_states, ['value = 0\n'])
        self.assertEqual(self.applied_bytes, [])

    def test_failed_gate_fix_uses_same_review_boundary_and_fresh_validation(self):
        loop = self.make_loop(
            [proposal(self.first), application(self.first),
             self.observe('value = 1\n', proposal(self.second), ('error: failed',)), application(self.second), COMPLETE],
            [approval(self.first), approval(self.second)], gate_results=[False, True],
        )
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.applied_bytes, [self.first, self.second])
        self.assertEqual(self.gate_states, ['value = 1\n', 'value = 2\n'])

    def test_final_failed_validation_has_no_unchecked_fix(self):
        loop = self.make_loop([proposal(self.first), application(self.first)], [approval(self.first)],
                              max_rounds=1, gate_results=[False])
        self.assertEqual(loop.run(), 1)
        self.assertEqual(len(self.author.prompts), 2)
        self.assertEqual(self.gate_states, ['value = 1\n'])

    def test_edit_limit_still_validates_last_edit_and_allows_completion(self):
        loop = self.make_loop([proposal(self.first), application(self.first), COMPLETE], [approval(self.first)], max_edits=1)
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.gate_states, ['value = 1\n'])

    def test_edit_limit_never_applies_an_additional_patch(self):
        loop = self.make_loop([proposal(self.first), application(self.first), proposal(self.second)], [approval(self.first)], max_edits=1)
        with self.assertRaisesRegex(protocol.CrewError, 'edit limit'):
            loop.run()
        self.assertEqual(self.applied_bytes, [self.first])
        self.assertEqual(self.gate_states, ['value = 1\n'])

    def test_completion_requires_record_and_cannot_override_failed_gates(self):
        loop = self.make_loop([COMPLETE], [], max_rounds=1, gate_results=[False])
        self.assertEqual(loop.run(), 1)
        self.guide_path.write_text('## S1 — One\nImplement value handling.\n')
        loop = self.make_loop([COMPLETE], [])
        with self.assertRaisesRegex(protocol.CrewError, 'missing Implementation record'):
            loop.run()
        self.assertEqual(self.applied_bytes, [])

    def test_guide_record_itself_goes_through_review(self):
        before = '## S1 — One\nImplement value handling.\n'
        after = before + '## Implementation record — S1\nImplemented and verified value handling.\n'
        self.guide_path.write_text(before)
        patch = self.first + text_patch('guide.md', before, after)
        loop = self.make_loop([proposal(patch), application(patch), COMPLETE], [approval(patch)])
        self.assertEqual(loop.run(), 0)
        self.assertEqual(self.guide_path.read_text(), after)
        self.assertEqual(self.applied_bytes, [patch])

    def test_no_gates_is_explicit_and_skip_implement_validates_current_tree(self):
        loop = self.make_loop([COMPLETE], [])
        loop.gate_set = 'none'
        self.assertEqual(loop.run(skip_implement=True), 0)
        self.assertIn('no gates configured', self.author.prompts[0][0])
        self.assertEqual(self.gate_states, ['value = 0\n'])
        self.assertEqual(self.applied_bytes, [])

    def test_main_handles_failure_and_dry_run_without_models_or_artifacts(self):
        guide_body = self.guide_path.read_text()
        argv = ['substep_crew.py', '--phase', '1', '--step', '1', '--substep', 'S1', '--gates', 'none']
        roles = {'implementer': lambda: ScriptedEngine([proposal(self.first)]),
                 'reviewer': lambda: ScriptedEngine(['invalid'] * 3), 'condenser': lambda: FakeEngine()}
        with mock.patch.object(engines, 'PROFILES', {'default': roles}), mock.patch.object(
            guides, 'find_guide', return_value=(self.guide_path, guide_body)
        ), mock.patch.object(sys, 'argv', argv + ['--dry-run']):
            self.assertEqual(cli.main(), 0)
            self.assertFalse((self.root / '.agents').exists())
        with mock.patch.object(engines, 'PROFILES', {'default': roles}), mock.patch.object(
            guides, 'find_guide', return_value=(self.guide_path, guide_body)
        ), mock.patch.object(sys, 'argv', argv):
            self.assertEqual(cli.main(), 2)
        errors = list((self.root / '.agents/runs').glob('*/error.txt'))
        self.assertEqual(len(errors), 1)
        self.assertIn('exhausted 3', errors[0].read_text())
        self.assertEqual(self.applied_bytes, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
