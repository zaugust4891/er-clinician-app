#!/usr/bin/env python3
"""Regression checks for the fail-closed dependency/secret policy boundary."""
import contextlib
import datetime
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


audit = load("ci-audit")
security = load("ci-security")


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "ci").mkdir()
        self.exception = dict(id="RUSTSEC-2023-0071", package="rsa", owner="maintainer",
                              expires="2099-01-01", impact="timing", mitigation="verification only",
                              evidence="review.md")

    def check_audit(self, findings=(), code=1, exception=None, raw=None):
        (self.root / "ci/security-exceptions.json").write_text(json.dumps([exception or self.exception]))
        report = json.dumps({"vulnerabilities": {"list": [
            {"advisory": {"id": identifier}, "package": {"name": package}}
            for identifier, package in findings]}, "warnings": {}}) if raw is None else raw
        with patch.object(audit, "ROOT", self.root), patch.object(audit.subprocess, "run",
                return_value=subprocess.CompletedProcess([], code, report, "")), contextlib.redirect_stdout(io.StringIO()):
            audit.main()

    def test_exact_reviewed_exception_passes(self):
        self.check_audit([("RUSTSEC-2023-0071", "rsa")])

    def test_new_advisory_blocks_even_with_allowed_one(self):
        with self.assertRaises(SystemExit):
            self.check_audit([("RUSTSEC-2023-0071", "rsa"), ("RUSTSEC-2099-0001", "tls")])

    def test_exception_does_not_cover_another_package(self):
        with self.assertRaises(SystemExit):
            self.check_audit([("RUSTSEC-2023-0071", "other")])

    def test_expiry_blocks_even_without_findings(self):
        expired = dict(self.exception, expires=datetime.date.today().isoformat())
        with self.assertRaises(SystemExit):
            self.check_audit(code=0, exception=expired)

    def test_scanner_failure_cannot_become_clean_scan(self):
        for code, raw in [(1, ""), (2, "{}"), (1, None)]:
            with self.subTest(code=code, raw=raw), self.assertRaises(SystemExit):
                self.check_audit(code=code, raw=raw)

    def test_clean_scan_passes(self):
        self.check_audit(code=0)

    def test_changed_fixture_is_rejected_before_any_scan(self):
        fixture = self.root / security.FIXTURE
        fixture.parent.mkdir(parents=True)
        fixture.write_text("replacement-key")
        with patch.object(security, "ROOT", self.root), patch.object(security.subprocess, "run") as scan:
            with self.assertRaises(SystemExit):
                security.main()
            scan.assert_not_called()


if __name__ == "__main__":
    unittest.main()
