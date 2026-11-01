#!/usr/bin/env python3
"""Persist/review/apply one encrypted foundation plan; never publish its raw JSON."""
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile

from deploy import ROOT, GateError, aws, require, run, write_json


def main():
    os.umask(0o077)
    config = json.loads(os.environ["STAGING_RELEASE_CONFIG"])
    foundation = json.loads(os.environ["STAGING_FOUNDATION_TFVARS"])
    commit = run(["git", "rev-parse", "HEAD"], cwd=ROOT).decode().strip()
    bucket = config["deploy_artifacts_bucket"]
    require(aws("sts", "get-caller-identity")["Account"] == config["aws_account_id"], "wrong_account")
    for key in ("aws_account_id", "aws_region", "project", "deploy_artifacts_bucket"):
        require(foundation[key] == config[key], "foundation_release_contract_mismatch")
    foundation["generated_by"] = commit
    root = ROOT / "infra/envs/staging/foundation"

    def tf(*args):
        return run(["terraform", f"-chdir={root}", *args], timeout=3600)

    tf("init", "-input=false", "-lockfile=readonly", "-reconfigure",
       "-backend-config=bucket=" + config["state_bucket"], "-backend-config=key=staging/foundation/terraform.tfstate",
       "-backend-config=region=" + config["aws_region"], "-backend-config=kms_key_id=" + config["state_kms_key_arn"])
    with tempfile.TemporaryDirectory(prefix="ernote-foundation-") as work:
        work = Path(work)
        plan = work / "foundation.tfplan"
        if os.environ["INFRA_ACTION"] == "plan":
            variables = work / "foundation.tfvars.json"
            write_json(variables, foundation)
            tf("plan", "-input=false", "-lock-timeout=5m", "-var-file=" + str(variables), "-out=" + str(plan))
            plan_id = os.environ["GITHUB_RUN_ID"] + "-" + os.environ["GITHUB_RUN_ATTEMPT"]
            key = "envs/staging/foundation-plans/" + plan_id + ".tfplan"
            metadata = {"source-commit": commit, "sha256": hashlib.sha256(plan.read_bytes()).hexdigest()}
            result = json.loads(run(["aws", "s3api", "put-object", "--bucket", bucket, "--key", key,
                                    "--body", str(plan), "--metadata", json.dumps(metadata)]))
            # Reviewers retrieve this exact version using their restricted operator role.
            print(json.dumps({"plan_id": plan_id, "version_id": result["VersionId"], **metadata}))
            document = json.loads(tf("show", "-json", str(plan)))
            counts = {}
            for item in document.get("resource_changes", []):
                action = "/".join(item["change"]["actions"])
                counts[action] = counts.get(action, 0) + 1
            print(json.dumps({"resource_action_counts": counts}))
        else:
            require(os.environ["INFRA_ACTION"] == "apply", "infra_action")
            plan_id = os.environ["PLAN_ID"]
            require(re.fullmatch(r"[0-9]+-[0-9]+", plan_id), "plan_id")
            version = os.environ["PLAN_VERSION_ID"]
            require(bool(version), "plan_version")
            key = "envs/staging/foundation-plans/" + plan_id + ".tfplan"
            response = json.loads(run(["aws", "s3api", "get-object", "--bucket", bucket, "--key", key,
                                       "--version-id", version, str(plan)]))
            metadata = response["Metadata"]
            require(metadata["source-commit"] == commit, "plan_source_changed")
            require(metadata["sha256"] == hashlib.sha256(plan.read_bytes()).hexdigest()
                    == os.environ["PLAN_SHA256"], "reviewed_plan_checksum")
            # Terraform itself rejects a stale state serial. Never silently replan/apply.
            tf("apply", "-input=false", "-lock-timeout=5m", str(plan))
            print("reviewed_foundation_plan_applied")


if __name__ == "__main__":
    try:
        main()
    except GateError as error:
        sys.exit(str(error))
    except Exception:
        sys.exit("foundation_failed_inspect_restricted_plan_and_operator_evidence")
