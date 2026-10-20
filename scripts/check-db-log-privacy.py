#!/usr/bin/env python3
"""S9: prove the configured PostgreSQL logging floor with fabricated error canaries.

Uses only a disposable postgres:16 container on tmpfs, no host ports, no cloud.
Requires Docker and the locally cached image. Prints category evidence, not errors.
"""
import re
import subprocess
import time
import uuid
from pathlib import Path


def run(*args, check=True):
    return subprocess.run(args, capture_output=True, text=True, check=check)


def main():
    # Read the actual Terraform values, so this checks the deployed contract.
    source = (Path(__file__).resolve().parents[1] / "infra/modules/data/database.tf").read_text()
    names = ["log_min_messages", "log_min_error_statement", "log_parameter_max_length",
             "log_parameter_max_length_on_error", "log_duration", "log_min_duration_sample",
             "log_statement_sample_rate", "log_transaction_sample_rate"]
    settings = {name: re.search(rf'\b{name}\s*=\s*"([^\"]+)"', source).group(1) for name in names}
    settings.update(log_statement="none", log_min_duration_statement="-1")
    container = f"er-s9-log-canary-{uuid.uuid4().hex[:10]}"
    try:
        command = ["docker", "run", "--detach", "--rm", "--name", container,
                   "--pull=never", "--network=none", "--tmpfs", "/var/lib/postgresql/data:rw,size=256m",
                   "--env", "POSTGRES_PASSWORD=synthetic-only", "postgres:16"]
        command += [item for name, value in settings.items() for item in ("-c", f"{name}={value}")]
        run(*command)
        for _ in range(60):
            if run("docker", "exec", container, "pg_isready", "-U", "postgres", check=False).returncode == 0:
                # The init entrypoint briefly starts a temporary server; wait until
                # the final server has our settings before injecting any canary.
                current = run("docker", "exec", container, "psql", "-U", "postgres", "-Atc", "SHOW log_min_messages", check=False)
                if current.stdout.strip() == settings["log_min_messages"]:
                    break
            time.sleep(0.5)
        else:
            raise RuntimeError("postgres_not_ready")
        for name, expected in settings.items():
            actual = run("docker", "exec", container, "psql", "-U", "postgres", "-Atc", f"SHOW {name}").stdout.strip()
            if actual != ({"0": "off"}.get(expected, expected) if name == "log_duration" else expected):
                raise RuntimeError("parameter_mismatch")
        # Literal parse error plus extended bind value echoed by a real DB error.
        queries = ["SELECT 'S9CANARY_LITERAL'::integer", "PREPARE canary(text) AS SELECT $1::integer; EXECUTE canary('S9CANARY_BIND');"]
        for query in queries:
            result = run("docker", "exec", container, "psql", "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-c", query, check=False)
            if result.returncode == 0 or "S9CANARY_" not in result.stderr:
                raise RuntimeError("canary_not_exercised")
        time.sleep(0.25)
        if "S9CANARY_" in run("docker", "logs", container).stdout + run("docker", "logs", container).stderr:
            raise RuntimeError("database_log_canary_leaked")
        print("PASS: effective PostgreSQL parameters; literal/bind errors exercised; engine logs omit both canaries")
    finally:
        run("docker", "rm", "--force", container, check=False)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.SubprocessError, AttributeError) as error:
        # Docker/psql output may contain the fabricated values; retain only a code.
        print(f"FAIL: {str(error) if isinstance(error, RuntimeError) else type(error).__name__}")
        raise SystemExit(1) from None
