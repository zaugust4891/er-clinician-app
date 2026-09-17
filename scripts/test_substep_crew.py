#!/usr/bin/env python3
"""Isolated correctness tests for substep_crew; no real model sessions."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("substep_crew", Path(__file__).with_name("substep_crew.py"))
crew = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = crew
SPEC.loader.exec_module(crew)


class FakeEngine(crew.Engine):
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
        return crew.Result(True, output, 0.01, log_path=log_path)


READY = json.dumps({"verdict": "READY", "summary": "ok", "blockers": [], "substeps_covered": ["S1"]})
NOT_READY = json.dumps({
    "verdict": "NOT_READY", "summary": "fix it", "substeps_covered": ["S1"],
    "blockers": [{"severity": "high", "file": "x.py", "problem": "broken", "required_fix": "repair"}],
})


class StreamTests(unittest.TestCase):
    def test_complete_log_bounded_excerpt_unicode_and_stderr(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "large.log"
            payload = "HEAD:" + "x" * 9000 + "🙂TAIL"
            code = (
                "import os; b=" + repr(payload.encode()) + "; "
                "os.write(1,b[:9007]); os.write(1,b[9007:]); os.write(2,b'\\nERR\\n')"
            )
            with contextlib.redirect_stdout(io.StringIO()):
                result = crew._stream([sys.executable, "-c", code], 5, "test", log_path=log)
            self.assertTrue(result.ok)
            self.assertEqual(log.read_text(encoding="utf-8"), payload + "\nERR\n")
            self.assertLessEqual(len(result.output), crew.STREAM_HEAD_CHARS + crew.STREAM_TAIL_CHARS + 200)
            self.assertIn("complete transcript", result.output)
            self.assertIn("🙂TAIL", result.output)
            self.assertIn("ERR", result.output)

    def test_run_gates_uses_unique_complete_transcripts(self):
        with tempfile.TemporaryDirectory() as td, contextlib.redirect_stdout(io.StringIO()), mock.patch.object(
            crew, "GATE_SETS", {"fake": [("same", [sys.executable, "-c", "print('one')"]), ("same", [sys.executable, "-c", "print('two')"])]}
        ):
            gates = crew.run_gates("fake", 5, Path(td), 3)
            self.assertEqual(len({gate.log_path for gate in gates}), 2)
            self.assertEqual([gate.log_path.name for gate in gates], [
                "03-gate-01-same.transcript.log", "03-gate-02-same.transcript.log"
            ])
            self.assertEqual([gate.log_path.read_text().strip() for gate in gates], ["one", "two"])

    def test_without_log_preserves_full_output_and_exit_status(self):
        with contextlib.redirect_stdout(io.StringIO()):
            result = crew._stream([sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr); raise SystemExit(7)"], 5, "test")
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
                result = crew._stream([sys.executable, "-c", parent], 0.3, "test", log_path=log)
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
        original = crew._stream_console
        calls = 0
        def interrupt(label, text, at_line_start):
            nonlocal calls
            calls += 1
            if calls == 1:
                raise KeyboardInterrupt
            return original(label, text, at_line_start)
        with tempfile.TemporaryDirectory() as td, mock.patch.object(crew, "_stream_console", interrupt):
            log = Path(td) / "interrupt.log"
            pid_file = Path(td) / "pid"
            child = "import os,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); open(%r,'w').write(str(os.getpid())); time.sleep(30)" % str(pid_file)
            parent = "import subprocess,sys,time; c=subprocess.Popen([sys.executable,'-c',%r]); open(%r,'w').write(str(c.pid)); time.sleep(.1); print('ready',flush=True); time.sleep(30)" % (child, str(pid_file))
            with self.assertRaises(KeyboardInterrupt):
                crew._stream([sys.executable, "-c", parent], 20, "test", log_path=log)
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
        found = crew._extract_failures(iter(["error: first", "error[E1]: second", "", "error: final"]))
        self.assertEqual(found, ["- error: first", "- error[E1]: second", "- error: final"])

    def test_long_line_and_diagnostic_count_are_bounded(self):
        lines = (f"error: {i}" + "x" * 10000 for i in range(100))
        found = crew._extract_failures(lines, max_diagnostics=3)
        self.assertEqual(len(found), 4)
        self.assertIn("additional distinct", found[-1])
        self.assertTrue(all(len(item) <= crew.MAX_DIAGNOSTIC_CHARS for item in found[:-1]))

    def test_condense_combined_budget_reports_omitted_gates(self):
        gates = [crew.Gate("a-long-gate-name", False, "x" * 1000, 1), crew.Gate("another-gate", True, "", 1)]
        with tempfile.TemporaryDirectory() as td, mock.patch.object(crew, "RUNS", Path(td) / "runs"):
            summary = crew.condense(gates, FakeEngine(), budget=120)
        self.assertLessEqual(len(summary), 120)
        self.assertIn("additional gate", summary)

    def test_condense_reads_log_and_enforces_budget_on_malformed_model(self):
        with tempfile.TemporaryDirectory() as td:
            log = Path(td) / "gate.log"
            log.write_text("unrecognized " + "z" * 10000, encoding="utf-8")
            gate = crew.Gate("custom", False, "small excerpt", 1, log)
            engine = FakeEngine(["not json"])
            with mock.patch.object(crew, "RUNS", Path(td) / "runs"):
                summary = crew.condense([gate], engine, budget=300, helper_log_path=Path(td) / "helper.transcript.log")
            self.assertLessEqual(len(summary), 300)
            self.assertIn("complete transcript", summary.lower())


class VerdictAndEngineTests(unittest.TestCase):
    def test_claude_config_prefers_override_then_personal_then_default(self):
        with tempfile.TemporaryDirectory() as td:
            home = Path(td)
            personal = home / ".claude-personal"
            fallback = home / ".claude"
            fallback.mkdir()

            with mock.patch.object(crew.Path, "home", return_value=home), mock.patch.dict(
                os.environ, {}, clear=True
            ):
                self.assertEqual(crew.default_claude_config_dir(), fallback)
                personal.mkdir()
                self.assertEqual(crew.default_claude_config_dir(), personal)

                override = home / "explicit-claude-home"
                os.environ["CLAUDE_CONFIG_DIR"] = str(override)
                self.assertEqual(crew.default_claude_config_dir(), override)

    def test_verdict_validation_fails_closed(self):
        invalid = [
            "READY TO COMMIT",
            '{"verdict":"READY"}',
            json.dumps({"verdict":"READY", "summary":"x", "blockers":[], "substeps_covered":[], "extra":1}),
            json.dumps({"verdict":"READY", "summary":"x", "blockers":[{"severity":"high","file":"x","problem":"p","required_fix":"f"}], "substeps_covered":[]}),
        ]
        for raw in invalid:
            with self.subTest(raw=raw):
                verdict = crew.parse_verdict(raw)
                self.assertEqual(verdict["verdict"], "NOT_READY")
                self.assertTrue(verdict["_protocol_error"])
        self.assertEqual(crew.parse_verdict(READY)["verdict"], "READY")

    def test_claude_schema_and_complete_final_response(self):
        with tempfile.TemporaryDirectory() as td:
            transcript = Path(td) / "01-review.transcript.log"
            schema = Path(td) / "schema.json"
            schema.write_text(json.dumps(crew.VERDICT_SCHEMA))
            final = READY[:-1] + ', "summary": "' + "x" * 8000 + '"}'
            # Use a simple valid object for the engine transport test; verdict validation is separate.
            final = json.dumps({"payload": "x" * 8000})
            captured = {}
            def fake_stream(argv, timeout, label, env=None, *, log_path=None, stdout_path=None):
                captured["argv"] = argv
                captured["env"] = env
                Path(log_path).write_text("stderr diagnostic\n" + final, encoding="utf-8")
                Path(stdout_path).write_text(final, encoding="utf-8")
                return crew.Result(True, "bounded excerpt", .1, log_path=Path(log_path))
            with mock.patch.object(crew.shutil, "which", return_value="/bin/claude"), mock.patch.object(crew, "_stream", fake_stream):
                result = crew.ClaudeEngine(permission_mode="plan").run("review", 5, schema=schema, log_path=transcript)
            self.assertEqual(result.output, final)
            self.assertIn("--json-schema", captured["argv"])
            self.assertEqual(captured["env"]["CLAUDE_CONFIG_DIR"], str(crew.default_claude_config_dir()))
            self.assertEqual(captured["env"]["PAI_DIR"], str(crew.default_claude_config_dir()))
            final_path = Path(td) / "01-review.final.json"
            self.assertTrue(final_path.exists())
            self.assertNotEqual(transcript.read_text(), final_path.read_text())

    def test_codex_complete_final_message_is_not_excerpt(self):
        with tempfile.TemporaryDirectory() as td:
            transcript = Path(td) / "01-review.transcript.log"
            final = json.dumps({"verdict": "READY", "summary": "x" * 8000, "blockers": [], "substeps_covered": ["S1"]})
            def fake_stream(argv, timeout, label, env=None, *, log_path=None):
                output_path = Path(argv[argv.index("-o") + 1])
                output_path.write_text(final, encoding="utf-8")
                Path(log_path).write_text("event" * 3000, encoding="utf-8")
                return crew.Result(True, "bounded excerpt", .1, log_path=Path(log_path))
            with mock.patch.object(crew.shutil, "which", return_value="/bin/codex"), mock.patch.object(crew, "_stream", fake_stream):
                result = crew.CodexEngine().run("review", 5, log_path=transcript)
            self.assertEqual(result.output, final)
            self.assertEqual(crew.parse_verdict(result.output)["verdict"], "READY")
            self.assertTrue((Path(td) / "01-review.final.md").exists())


class ParserAndMatchingTests(unittest.TestCase):
    def test_guide_lines_and_sections(self):
        body = "preface\n\n## S1 — One\na\n\n## S2 - Two\nb\n## Other\nc\n## Implementation record — S1\n"
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "guide.md"
            path.write_text("different", encoding="utf-8")
            guide = crew.parse_guide(path, 4, 1, body)
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
        self.assertEqual(crew.repeated_blockers(previous, current), expected)

    def test_positional_result_and_gate_compatibility(self):
        self.assertEqual(crew.Result(False, "x", 1, "note").note, "note")
        self.assertIsNone(crew.Gate("g", True, "", 1).log_path)


class SchedulingTests(unittest.TestCase):
    def run_main(self, policy, gate_results, reviewer_outputs, rounds=2, gates="rust", reviewer_engine=None):
        implementer, fixer = FakeEngine(), FakeEngine()
        reviewer = reviewer_engine or FakeEngine(reviewer_outputs)
        condenser, triage = FakeEngine(), FakeEngine()
        profile = {"implementer": lambda: implementer, "reviewer": lambda: reviewer, "fixer": lambda: fixer, "condenser": lambda: condenser, "triage": lambda: triage}
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            guide_path = root / "guide.md"
            body = "## S1 — One\nDo it\n"
            guide_path.write_text(body)
            sequence = iter(gate_results)
            def fake_git(*args):
                if args[0] == "log": return "phase 1 step 1 s0"
                if args[0] == "rev-parse": return "a" * 40
                return ""
            def fake_gates(names, timeout, run_dir, round_num):
                ok = next(sequence)
                log = run_dir / f"{round_num:02d}-gate.transcript.log"
                log.write_text("ok" if ok else "error: failed")
                return [crew.Gate("test", ok, "error: failed" if not ok else "ok", .1, log)] if names != "none" else []
            argv = ["substep_crew.py", "--phase", "1", "--step", "1", "--substep", "S1", "--skip-implement", "--review-policy", policy, "--max-rounds", str(rounds), "--gates", gates]
            with mock.patch.object(sys, "argv", argv), mock.patch.object(crew, "REPO", root), mock.patch.object(crew, "RUNS", root / ".agents/runs"), mock.patch.object(crew, "PROFILES", {"test": profile}), mock.patch.object(crew, "GATE_SETS", {"rust": [("test", [])], "none": []}), mock.patch.object(crew, "find_guide", return_value=(guide_path, body)), mock.patch.object(crew, "git", side_effect=fake_git), mock.patch.object(crew, "run_gates", side_effect=fake_gates), contextlib.redirect_stdout(io.StringIO()):
                sys.argv += ["--profile", "test"]
                code = crew.main()
        return code, reviewer, fixer

    def test_gates_first_defers_review_then_validates_fix(self):
        code, reviewer, fixer = self.run_main("gates-first", [False, True], [READY])
        self.assertEqual(code, 0)
        self.assertEqual(len(reviewer.prompts), 1)
        self.assertEqual(len(fixer.prompts), 1)
        self.assertIn("No semantic code review was performed", fixer.prompts[0][0])

    def test_always_reviews_failed_gates_but_cannot_approve(self):
        code, reviewer, fixer = self.run_main("always", [False], [READY], rounds=1)
        self.assertEqual(code, 1)
        self.assertEqual(len(reviewer.prompts), 1)
        self.assertEqual(len(fixer.prompts), 0)

    def test_always_ready_with_failed_gates_uses_gate_fix_wording(self):
        code, reviewer, fixer = self.run_main("always", [False, True], [READY, READY], rounds=2)
        self.assertEqual(code, 0)
        self.assertEqual(len(reviewer.prompts), 2)
        self.assertEqual(len(fixer.prompts), 1)
        self.assertIn("reviewer returned READY", fixer.prompts[0][0])
        self.assertNotIn("marked it NOT_READY", fixer.prompts[0][0])

    def test_final_failed_gate_gets_no_unchecked_fix(self):
        code, reviewer, fixer = self.run_main("gates-first", [False], [], rounds=1)
        self.assertEqual(code, 1)
        self.assertEqual(len(reviewer.prompts), 0)
        self.assertEqual(len(fixer.prompts), 0)

    def test_no_gates_is_reported_but_valid_review_can_approve(self):
        code, reviewer, fixer = self.run_main("gates-first", [True], [READY], rounds=1, gates="none")
        self.assertEqual(code, 0)
        self.assertIn("no gates configured", reviewer.prompts[0][0])
        self.assertEqual(len(fixer.prompts), 0)

    def test_failed_reviewer_process_never_approves(self):
        class Failed(FakeEngine):
            def run(self, *args, **kwargs):
                self.prompts.append((args[0], kwargs.get("schema"), kwargs.get("log_path")))
                return crew.Result(False, READY, .1, "exit 1", kwargs.get("log_path"))
        failed = Failed()
        code, reviewer, fixer = self.run_main(
            "gates-first", [True], [], rounds=1, reviewer_engine=failed
        )
        self.assertEqual(code, 2)
        self.assertEqual(len(reviewer.prompts), 1)
        self.assertEqual(len(fixer.prompts), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
