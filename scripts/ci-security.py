#!/usr/bin/env python3
"""Scan tracked source only; never print a secret value or copy local ignored data."""
import hashlib
import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = "dev/keys/dev_private.pem"
FIXTURE_SHA = "3a0d85b9c5614b015b54c6ed1895cbc15f5b141d4ae6e5e86cc45146faee4b09"


def main():
    if hashlib.sha256((ROOT / FIXTURE).read_bytes()).hexdigest() != FIXTURE_SHA:
        raise SystemExit("Known public dev-key fixture changed; secret-scan exception no longer applies")
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
    with tempfile.TemporaryDirectory(prefix="ernote-secret-scan-") as temporary:
        snapshot = Path(temporary)
        for name in filter(None, paths):
            source = ROOT / name
            if not source.exists():  # Working-tree deletion is not part of candidate source.
                continue
            if source.is_symlink():
                raise SystemExit("Tracked symlinks require a source-boundary review")
            if (source.suffix in {".db", ".sqlite", ".sqlite3", ".wav", ".mp3", ".m4a", ".rtf", ".odt", ".tfstate", ".tfplan"}
                    or re.search(r"\.(db|sqlite3?)([-.].*)?$", source.name)
                    or source.name in {".env", "shift.json"}):
                raise SystemExit("Runtime/data artifact found in tracked source; review before publication")
            destination = snapshot / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
        report = snapshot / "findings.json"
        result = subprocess.run(["gitleaks", "dir", "--redact=100", "--no-banner", "--exit-code=1",
                                 "--ignore-gitleaks-allow", "--gitleaks-ignore-path", "/dev/null",
                                 "--report-format=json", "--report-path", str(report),
                                 "--config", str(ROOT / "ci/gitleaks.toml"), "."],
                                cwd=snapshot, capture_output=True, text=True)
        if result.returncode:
            if report.exists():
                for finding in json.loads(report.read_text()):
                    print(f"BLOCK: {finding['RuleID']} {finding['File']}:{finding['StartLine']}")
            raise SystemExit("Secret scan failed; no matched values are printed")
    print("Tracked-source data boundary and redacted secret scan passed")


if __name__ == "__main__":
    main()
