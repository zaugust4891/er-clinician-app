#!/usr/bin/env python3
"""cargo audit policy: exact, expiring exceptions, with no suppressed new advisories."""
import datetime
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main():
    exceptions = json.loads((ROOT / "ci/security-exceptions.json").read_text())
    today = datetime.datetime.now(datetime.timezone.utc).date()
    for exception in exceptions:
        for key in ("id", "package", "owner", "expires", "impact", "mitigation", "evidence"):
            if not exception.get(key):
                raise SystemExit(f"Incomplete security exception: {key}")
        if datetime.date.fromisoformat(exception["expires"]) <= today:
            raise SystemExit(f"Expired security exception: {exception['id']}")
    result = subprocess.run(["cargo", "audit", "--json"], cwd=ROOT, capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise SystemExit("cargo audit could not complete")
    try:
        report = json.loads(result.stdout)
        vulnerabilities = report["vulnerabilities"]["list"]
    except (ValueError, KeyError):
        raise SystemExit("cargo audit returned no valid report (database/network/tool failure)") from None
    allowed = {(entry["id"], entry["package"]) for entry in exceptions}
    blocked = []
    for finding in vulnerabilities:
        identity = (finding["advisory"]["id"], finding["package"]["name"])
        print(f"{'EXCEPTION' if identity in allowed else 'BLOCK'}: {identity[0]} {identity[1]}")
        if identity not in allowed:
            blocked.append(identity)
    for category, warnings in report.get("warnings", {}).items():
        for warning in warnings:
            print(f"WARNING: {category}: {warning['package']['name']} {warning['package']['version']}")
    if blocked or (result.returncode != 0 and not vulnerabilities):
        raise SystemExit("Dependency audit failed")
    print("Dependency audit policy passed")


if __name__ == "__main__":
    main()
