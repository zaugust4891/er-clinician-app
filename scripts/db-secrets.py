#!/usr/bin/env python3
"""Stage DB credentials without files/arguments containing passwords; promote after ECS proof.

Run only from an approved operator session. The record contains ARNs/version IDs,
never passwords. ECS reads AWSPENDING during bootstrap; services read AWSCURRENT.
The script never calls GetSecretValue. See docs/ops/identity-verification.md.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import uuid


LOGIN_NAMES = {
    "migration_db": "er_schema_owner",
    "api_db": "er_api_login",
    "worker_db": "er_worker_login",
    "onboarding_db": "er_onboarding_login",
}


def aws(args: argparse.Namespace, service: str, operation: str, payload: dict) -> dict:
    # CLI input travels through stdin, with no plaintext credential in argv or disk.
    command = ["aws", "--no-cli-pager", "--region", args.region, "--output", "json"]
    if args.profile:
        command += ["--profile", args.profile]
    command += [service, operation, "--cli-input-json", "file:///dev/stdin"]
    env = {**os.environ, "AWS_PAGER": "", "AWS_CLI_AUTO_PROMPT": "off"}
    result = subprocess.run(command, input=json.dumps(payload), text=True,
                            capture_output=True, env=env, timeout=60, check=False)
    if result.returncode:
        # Neither CLI stderr nor the payload is safe to print.
        raise ValueError(f"{service}_{operation}_failed")
    return json.loads(result.stdout)


def save_record(path: Path, record: dict) -> None:
    # An exclusive temporary sibling avoids symlink following and partial records.
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex)
    try:
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as stream:
            json.dump(record, stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def versions(args: argparse.Namespace, arn: str) -> dict:
    return aws(args, "secretsmanager", "describe-secret", {"SecretId": arn}).get("VersionIdsToStages", {})


def verify_record(record: dict, args: argparse.Namespace, arns: dict) -> None:
    if record.get("account_id") != args.account_id or record.get("region") != args.region:
        raise ValueError("record_account_or_region_mismatch")
    entries = record.get("logins", {})
    if set(entries) != set(LOGIN_NAMES):
        raise ValueError("record_login_set_mismatch")
    for name, username in LOGIN_NAMES.items():
        entry = entries[name]
        if entry.get("arn") != arns.get(name) or entry.get("username") != username:
            raise ValueError("record_secret_or_username_mismatch")
        if not re.fullmatch(r"[0-9a-f]{32}", entry.get("version_id", "")):
            raise ValueError("record_version_invalid")


def stage(args: argparse.Namespace, arns: dict) -> None:
    if args.record.exists():
        record = json.loads(args.record.read_text())
        verify_record(record, args, arns)
    else:
        # Refuse to displace a previous unfinished rotation. Recover its record.
        for name in LOGIN_NAMES:
            if any("AWSPENDING" in stages for stages in versions(args, arns[name]).values()):
                raise ValueError("existing_pending_rotation_requires_its_record")
        record = {"account_id": args.account_id, "region": args.region, "logins": {
            name: {"arn": arns[name], "username": username, "version_id": uuid.uuid4().hex}
            for name, username in LOGIN_NAMES.items()
        }}
        # Persist IDs before the first write, so a lost response is recoverable.
        save_record(args.record, record)

    for entry in record["logins"].values():
        known = versions(args, entry["arn"])
        own = known.get(entry["version_id"], [])
        if "AWSCURRENT" in own:
            raise ValueError("record_already_promoted")
        if "AWSPENDING" in own:
            continue
        if entry["version_id"] in known or any("AWSPENDING" in labels for labels in known.values()):
            raise ValueError("pending_version_changed")
        aws(args, "secretsmanager", "put-secret-value", {
            "SecretId": entry["arn"], "ClientRequestToken": entry["version_id"],
            "VersionStages": ["AWSPENDING"],
            "SecretString": json.dumps({"username": entry["username"], "password": secrets.token_urlsafe(48)}),
        })
    print("db_secrets status=staged")


def bootstrap_secrets(identity: dict, record: dict) -> list[dict]:
    injected = {name: identity["ecs_secrets"]["db-bootstrap"][name] for name in ("DB_USERNAME", "DB_PASSWORD")}
    for key, prefix in (("migration_db", "OWNER"), ("api_db", "API"), ("worker_db", "WORKER"), ("onboarding_db", "ONBOARDING")):
        entry = record["logins"][key]
        for field in ("username", "password"):
            injected[f"{prefix}_DB_{field.upper()}"] = f"{entry['arn']}:{field}::{entry['version_id']}"
    return [{"name": name, "valueFrom": value} for name, value in sorted(injected.items())]


def verify_definition(definition: dict, identity: dict, record: dict) -> None:
    containers = definition.get("containerDefinitions", [])
    if (len(containers) != 1 or containers[0].get("name") != "db-bootstrap"
            or containers[0].get("entryPoint") != ["/usr/local/bin/er-db-bootstrap"]
            or not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", containers[0].get("image", ""))
            or containers[0].get("command")
            or definition.get("taskRoleArn")
            or definition.get("executionRoleArn") != identity["execution_role_arns"]["db-bootstrap"]
            or definition.get("networkMode") != "awsvpc"
            or "FARGATE" not in definition.get("requiresCompatibilities", [])):
        raise ValueError("bootstrap_definition_boundary_mismatch")
    expected = bootstrap_secrets(identity, record)
    if sorted(containers[0].get("secrets", []), key=lambda x: x["name"]) != expected:
        raise ValueError("bootstrap_definition_does_not_pin_this_rotation")
    names = {entry["name"] for entry in expected}
    if any(item.get("name") in names for item in containers[0].get("environment", [])):
        raise ValueError("bootstrap_environment_overrides_secret")


def render(args: argparse.Namespace, identity: dict, record: dict) -> None:
    if args.task_definition_template is None:
        raise ValueError("bootstrap_template_required")
    definition = json.loads(args.task_definition_template.read_text())
    containers = definition.get("containerDefinitions", [])
    if len(containers) != 1:
        raise ValueError("bootstrap_requires_one_container")
    containers[0]["secrets"] = bootstrap_secrets(identity, record)
    verify_definition(definition, identity, record)
    print(json.dumps(definition, indent=2))


def verify_bootstrap(args: argparse.Namespace, identity: dict, record: dict) -> None:
    prefix = f"arn:aws:ecs:{args.region}:{args.account_id}:"
    if not args.task or not args.cluster or not args.digest:
        raise ValueError("promotion_requires_bootstrap_task_cluster_and_image_digest")
    if not args.task.startswith(prefix + "task/") or not args.cluster.startswith(prefix + "cluster/"):
        raise ValueError("bootstrap_task_account_or_region_mismatch")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", args.digest):
        raise ValueError("image_digest_invalid")
    result = aws(args, "ecs", "describe-tasks", {"cluster": args.cluster, "tasks": [args.task]})
    tasks = result.get("tasks", [])
    if result.get("failures") or len(tasks) != 1:
        raise ValueError("bootstrap_task_missing")
    task = tasks[0]
    containers = task.get("containers", [])
    if (task.get("lastStatus") != "STOPPED" or task.get("stopCode") != "EssentialContainerExited"
            or len(containers) != 1 or containers[0].get("name") != "db-bootstrap"
            or containers[0].get("exitCode") != 0 or containers[0].get("imageDigest") != args.digest):
        raise ValueError("bootstrap_task_not_successful_at_expected_digest")
    overrides = task.get("overrides", {})
    if (overrides.get("executionRoleArn") or overrides.get("taskRoleArn")
            or any(item.get("command") or item.get("environment") or item.get("environmentFiles")
                   for item in overrides.get("containerOverrides", []))):
        raise ValueError("bootstrap_task_used_overrides")
    definition = aws(args, "ecs", "describe-task-definition", {"taskDefinition": task["taskDefinitionArn"]})
    verify_definition(definition["taskDefinition"], identity, record)


def promote(args: argparse.Namespace, arns: dict, identity: dict) -> None:
    record = json.loads(args.record.read_text())
    verify_record(record, args, arns)
    verify_bootstrap(args, identity, record)
    # Check the complete set before changing any stage labels.
    for entry in record["logins"].values():
        own = versions(args, entry["arn"]).get(entry["version_id"], [])
        if "AWSPENDING" not in own and "AWSCURRENT" not in own:
            raise ValueError("record_no_longer_matches_staged_versions")
    for entry in record["logins"].values():
        known = versions(args, entry["arn"])
        own = known.get(entry["version_id"], [])
        if "AWSCURRENT" not in own:
            if "AWSPENDING" not in own:
                raise ValueError("pending_version_changed")
            current = [version for version, labels in known.items() if "AWSCURRENT" in labels]
            payload = {"SecretId": entry["arn"], "VersionStage": "AWSCURRENT", "MoveToVersionId": entry["version_id"]}
            if current:
                payload["RemoveFromVersionId"] = current[0]
            aws(args, "secretsmanager", "update-secret-version-stage", payload)
        # Removing AWSPENDING lets the next maintenance rotation start.
        if "AWSPENDING" in own:
            aws(args, "secretsmanager", "update-secret-version-stage", {
                "SecretId": entry["arn"], "VersionStage": "AWSPENDING", "RemoveFromVersionId": entry["version_id"],
            })
    print("db_secrets status=promoted")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("stage", "render", "promote"))
    parser.add_argument("--identity-file", type=Path, required=True, help="terraform output -json identity, pointers only")
    parser.add_argument("--record", type=Path, required=True, help="durable rotation record; retain through recovery")
    parser.add_argument("--account-id", required=True)
    parser.add_argument("--region", required=True)
    parser.add_argument("--profile")
    parser.add_argument("--task", help="successful db-bootstrap task ARN for promotion")
    parser.add_argument("--cluster", help="ECS cluster ARN for promotion")
    parser.add_argument("--digest", help="reviewed bootstrap image sha256 digest for promotion")
    parser.add_argument("--task-definition-template", type=Path, help="S8 reviewed, value-free bootstrap template for render")
    args = parser.parse_args()
    try:
        if not re.fullmatch(r"[0-9]{12}", args.account_id) or not re.fullmatch(r"us-[a-z]+-[0-9]", args.region):
            raise ValueError("invalid_account_or_region")
        identity = json.loads(args.identity_file.read_text())
        arns = identity["secret_arns"]
        prefix = f"arn:aws:secretsmanager:{args.region}:{args.account_id}:secret:"
        if not all(isinstance(arns.get(name), str) and arns[name].startswith(prefix) for name in LOGIN_NAMES):
            raise ValueError("identity_secret_account_or_region_mismatch")
        if len({arns[name] for name in LOGIN_NAMES}) != len(LOGIN_NAMES):
            raise ValueError("identity_secrets_must_be_distinct")
        if args.action == "render":
            record = json.loads(args.record.read_text())
            verify_record(record, args, arns)
            render(args, identity, record)
            return 0
        if aws(args, "sts", "get-caller-identity", {}).get("Account") != args.account_id:
            raise ValueError("caller_account_mismatch")
        if args.action == "stage":
            stage(args, arns)
        else:
            promote(args, arns, identity)
        return 0
    except (ValueError, KeyError, TypeError, AttributeError, OSError, subprocess.SubprocessError):
        # Even malformed inputs and CLI diagnostics may contain credentials.
        print("db_secrets status=failed; retain the rotation record and inspect metadata through the approved operator path", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
