#!/usr/bin/env python3
"""Synthetic staging release state machine. Stdout contains assertion names only.

AWS/Terraform diagnostics are captured, never copied into hosted logs. Inspect failures
through restricted AWS/operator access. Release records contain references, not secrets.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


class GateError(Exception):
    pass


def require(condition, label):
    if not condition:
        raise GateError(label)


def run(args, *, data=None, cwd=None, timeout=1800, env=None):
    try:
        result = subprocess.run(args, input=data, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, cwd=cwd, timeout=timeout, env=env)
    except subprocess.TimeoutExpired as error:
        raise GateError("command_deadline") from error
    require(result.returncode == 0, "command_failed_" + Path(str(args[0])).name)
    return result.stdout


def aws(service, operation, **payload):
    return json.loads(run(["aws", service, operation, "--cli-input-json", "file:///dev/stdin",
                           "--output", "json", "--no-cli-pager"],
                          data=json.dumps(payload).encode(), timeout=90) or b"{}")


def write_json(path, value):
    path.write_text(json.dumps(value, sort_keys=True))
    path.chmod(0o600)


def output(**values):
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
            for key, value in values.items():
                require("\n" not in str(value), "invalid_output")
                stream.write(f"{key}={value}\n")


def migration_set(root):
    return {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted((root / "crates/er-server/migrations").glob("*.sql"))}


def migration_success(task, definition):
    containers = task.get("containers", [])
    return (task.get("lastStatus") == "STOPPED" and task.get("taskDefinitionArn") == definition
            and task.get("stopCode") == "EssentialContainerExited"
            and len(containers) == 1 and containers[0].get("name") == "migration"
            and containers[0].get("exitCode") == 0)


def exact_service(service, tasks, definition, digest, count):
    deployments = service.get("deployments", [])
    return (service.get("desiredCount") == count and service.get("runningCount") == count
            and service.get("pendingCount") == 0 and service.get("taskDefinition") == definition
            and len(deployments) == 1 and deployments[0].get("rolloutState") == "COMPLETED"
            and deployments[0].get("taskDefinition") == definition and len(tasks) == count
            and all(t.get("lastStatus") == "RUNNING" and t.get("taskDefinitionArn") == definition
                    and len(t.get("containers", [])) == 1
                    and t["containers"][0].get("imageDigest") == digest for t in tasks))


class Deployment:
    def __init__(self, work):
        self.work = Path(work)
        self.config = json.loads(os.environ["STAGING_RELEASE_CONFIG"])
        require(set(self.config) == {"aws_account_id", "aws_region", "project",
                                     "deploy_artifacts_bucket", "state_bucket", "state_kms_key_arn"},
                "release_config_keys")
        self.account = self.config["aws_account_id"]
        self.region = self.config["aws_region"]
        self.name = self.config["project"] + "-staging"
        self.bucket = self.config["deploy_artifacts_bucket"]
        require(re.fullmatch(r"[0-9]{12}", self.account), "account_format")
        require(re.fullmatch(r"[a-z][a-z0-9-]{2,11}", self.config["project"]), "project_format")
        require(self.region in ("us-east-1", "us-east-2", "us-west-2"), "region_format")
        os.environ["AWS_DEFAULT_REGION"] = self.region
        os.environ["AWS_REGION"] = self.region
        require(aws("sts", "get-caller-identity")["Account"] == self.account, "wrong_account")
        self.commit = run(["git", "rev-parse", "HEAD"], cwd=ROOT).decode().strip()
        self.release_id = os.environ["RELEASE_ID"]
        require(re.fullmatch(r"[0-9]+-[0-9]+", self.release_id), "release_id_format")
        self.key = "envs/staging/releases/" + self.release_id + ".json"
        self.accepted_key = "envs/staging/accepted-release.json"

    def get(self, key, *, optional=False, version=None):
        # List distinguishes an absent first release from AccessDenied/network failure.
        if optional:
            objects = aws("s3api", "list-objects-v2", Bucket=self.bucket, Prefix=key).get("Contents", [])
            if not any(item["Key"] == key for item in objects):
                return None
        target = self.work / "object.json"
        args = ["aws", "s3api", "get-object", "--bucket", self.bucket, "--key", key]
        if version:
            args += ["--version-id", version]
        run(args + [str(target)], timeout=90)
        return json.loads(target.read_bytes())

    def put(self, key, value):
        path = self.work / "record.json"
        write_json(path, value)
        run(["aws", "s3api", "put-object", "--bucket", self.bucket, "--key", key,
             "--body", str(path), "--content-type", "application/json"], timeout=90)

    def tf(self, root, *args, timeout=1800):
        return run(["terraform", f"-chdir={root}", *args], timeout=timeout)

    def init(self, root):
        self.tf(root, "init", "-input=false", "-lockfile=readonly", "-reconfigure",
                "-backend-config=bucket=" + self.config["state_bucket"],
                "-backend-config=key=staging/release/terraform.tfstate",
                "-backend-config=region=" + self.region,
                "-backend-config=kms_key_id=" + self.config["state_kms_key_arn"])

    def plan(self, root, inputs, name):
        variables = self.work / (name + ".tfvars.json")
        write_json(variables, inputs)
        plan = self.work / (name + ".tfplan")
        self.tf(root, "plan", "-input=false", "-lock-timeout=5m", "-out=" + str(plan),
                "-var-file=" + str(variables))
        return plan, json.loads(self.tf(root, "show", "-json", str(plan)))

    def publish_image(self):
        repository = f"{self.account}.dkr.ecr.{self.region}.amazonaws.com/{self.name}"
        images = aws("ecr", "list-images", repositoryName=self.name, filter={"tagStatus": "TAGGED"})["imageIds"]
        existing = next((x["imageDigest"] for x in images if x.get("imageTag") == self.commit), None)
        login = aws("ecr", "get-authorization-token")["authorizationData"][0]
        import base64
        password = base64.b64decode(login["authorizationToken"]).split(b":", 1)[1]
        run(["docker", "login", "--username", "AWS", "--password-stdin", repository.split("/")[0]], data=password)
        try:
            if existing:
                # A retry reuses the previously published immutable image. No rebuild.
                image = repository + "@" + existing
                run(["docker", "pull", image], timeout=900)
            else:
                image_file = self.work / "image.id"
                env = dict(os.environ, CI_IMAGE_ID_FILE=str(image_file))
                run([str(ROOT / "scripts/ci-image.sh")], env=env, timeout=3600)
                image = image_file.read_text().strip()
                run(["docker", "tag", image, repository + ":" + self.commit])
                run(["docker", "push", repository + ":" + self.commit], timeout=900)
            digest = aws("ecr", "describe-images", repositoryName=self.name,
                         imageIds=[{"imageTag": self.commit}])["imageDetails"][0]["imageDigest"]
            require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), "registry_digest")
            image = repository + "@" + digest
            run(["docker", "pull", image], timeout=900)
            arch = run(["docker", "image", "inspect", "--format", "{{.Architecture}}", image]).decode().strip()
            require(arch == "amd64", "image_architecture")
            run(["trivy", "image", "--config", "/dev/null", "--ignorefile", "/dev/null", "--scanners", "vuln",
                 "--severity", "HIGH,CRITICAL", "--exit-code", "1", "--timeout", "15m", image], timeout=1000)
            return digest
        finally:
            run(["docker", "logout", repository.split("/")[0]])

    def migrate(self, template, network, cluster):
        family = template["family"]
        # Include PENDING tasks: desiredStatus RUNNING includes tasks not started yet.
        active = aws("ecs", "list-tasks", cluster=cluster, family=family, desiredStatus="RUNNING")["taskArns"]
        require(not active, "migration_already_active_inspect_before_retry")
        definition = aws("ecs", "register-task-definition", **template)["taskDefinition"]["taskDefinitionArn"]
        result = aws("ecs", "run-task", cluster=cluster, taskDefinition=definition,
                     launchType="FARGATE", platformVersion="1.4.0", count=1,
                     networkConfiguration=network, startedBy="release-" + self.release_id,
                     clientToken=hashlib.sha256((self.release_id + definition).encode()).hexdigest())
        require(not result.get("failures") and len(result.get("tasks", [])) == 1, "migration_submission")
        arn = result["tasks"][0]["taskArn"]
        print("migration_submitted " + arn)
        deadline = time.monotonic() + 900
        while time.monotonic() < deadline:
            response = aws("ecs", "describe-tasks", cluster=cluster, tasks=[arn])
            require(not response.get("failures") and len(response.get("tasks", [])) == 1, "migration_inspection")
            task = response["tasks"][0]
            if task.get("lastStatus") == "STOPPED":
                require(migration_success(task, definition), "migration_exit_gate")
                return {"task_arn": arn, "task_definition_arn": definition, "exit_code": 0}
            time.sleep(10)
        aws("ecs", "stop-task", cluster=cluster, task=arn, reason="Release migration deadline")
        raise GateError("migration_timeout_services_unchanged")

    def verify(self, record, root):
        compute = json.loads(self.tf(root, "output", "-json", "compute"))
        resources = record["resources"]
        cluster = resources["ecs_cluster_arn"]
        deadline = time.monotonic() + 600
        while time.monotonic() < deadline:
            healthy = True
            api_ips = set()
            for component, count in [("api", 2), ("worker", 1)]:
                name = compute["services"][component]
                response = aws("ecs", "describe-services", cluster=cluster, services=[name])
                require(not response.get("failures") and len(response.get("services", [])) == 1, "service_inspection")
                arns = aws("ecs", "list-tasks", cluster=cluster, serviceName=name, desiredStatus="RUNNING")["taskArns"]
                described = aws("ecs", "describe-tasks", cluster=cluster, tasks=arns) if arns else {"tasks": []}
                require(not described.get("failures"), "task_inspection")
                tasks = described["tasks"]
                healthy &= exact_service(response["services"][0], tasks,
                                         compute["task_definition_arns"][component], record["digest"], count)
                if component == "api":
                    api_ips = {n["privateIpv4Address"] for t in tasks for c in t.get("containers", [])
                               for n in c.get("networkInterfaces", [])}
            targets = aws("elbv2", "describe-target-health", TargetGroupArn=resources["api_target_group_arn"])["TargetHealthDescriptions"]
            good_ips = {t["Target"]["Id"] for t in targets if t["TargetHealth"]["State"] == "healthy"}
            if healthy and len(api_ips) == 2 and api_ips <= good_ips:
                return compute["task_definition_arns"]
            time.sleep(10)
        raise GateError("exact_release_health_gate")

    def deploy(self):
        require(self.get(self.key, optional=True) is None, "release_attempt_already_exists")
        digest = self.publish_image()
        foundation_key = "envs/staging/foundation-manifest.json"
        version = aws("s3api", "head-object", Bucket=self.bucket, Key=foundation_key)["VersionId"]
        resources = self.get(foundation_key, version=version)["resources"]
        inputs = {k: self.config[k] for k in ("aws_account_id", "aws_region", "project", "deploy_artifacts_bucket")}
        inputs.update(image_digest=digest, manifest_version_id=version, services_enabled=True)
        record = {"schema_version": 1, "environment": "staging", "commit": self.commit,
                  "digest": digest, "architecture": "linux/amd64", "release_id": self.release_id,
                  "inputs": inputs, "resources": resources, "migrations": migration_set(ROOT),
                  "previous": self.get(self.accepted_key, optional=True), "phase": "prepared"}
        # Bound history: retain one prior accepted record, not an ever-growing chain.
        if record["previous"]:
            record["previous"].pop("previous", None)
        self.put(self.key, record)
        output(record_created="true")
        root = ROOT / "infra/envs/staging/release"
        self.init(root)
        plan, document = self.plan(root, inputs, "candidate")
        deployment = document["planned_values"]["outputs"]["deployment"]["value"]
        record["migration"] = self.migrate(deployment["migration"], deployment["network"], resources["ecs_cluster_arn"])
        record["phase"] = "rollout_started"
        self.put(self.key, record)
        # The saved plan pins digest, manifest and state serial; no new plan after migration.
        self.tf(root, "apply", "-input=false", "-lock-timeout=5m", str(plan))
        record["task_definitions"] = self.verify(record, root)
        record["phase"] = "awaiting_smoke"
        self.put(self.key, record)
        self.smoke_outputs(record)
        print("candidate_exact_digest_healthy")

    def smoke_outputs(self, record):
        output(api_base_url=record["resources"]["api_base_url"],
               cognito_client_id=record["resources"]["cognito_client_id"], digest=record["digest"])

    def accept(self):
        record = self.get(self.key)
        require(record["phase"] in ("awaiting_smoke", "accepted"), "accept_phase")
        current = self.get(self.accepted_key, optional=True)
        require((current or {}).get("release_id") in ((record["previous"] or {}).get("release_id"),
                                                     record["release_id"]), "accepted_pointer_changed")
        record["phase"] = "accepted"
        record["accepted_at"] = (current or {}).get("accepted_at") if (current or {}).get("release_id") == record["release_id"] else time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        # A retry can finish either write after a lost response. The accepted pointer
        # is the authority; record bookkeeping failure must not strand a good release.
        self.put(self.accepted_key, record)
        self.put(self.key, record)
        print("release_accepted_after_smoke")

    def recover(self):
        record = self.get(self.key, optional=True)
        if not record or record["phase"] == "prepared":
            print("migration_or_preflight_failed_services_unchanged")
            return
        require(record["phase"] in ("rollout_started", "awaiting_smoke"), "recovery_phase")
        previous = record["previous"]
        policy = json.loads((ROOT / "ci/release-policy.json").read_text())
        compatible = previous and (previous["migrations"] == record["migrations"] or
            {"from": previous["migrations"], "to": record["migrations"]} in policy["rollback_compatible_migration_sets"])
        if compatible:
            require(re.fullmatch(r"[0-9a-f]{40}", previous["commit"]), "rollback_commit")
            # Prior code/config is authoritative, not today's rendering of an old image.
            archive = self.work / "previous.tar"
            archive.write_bytes(run(["git", "archive", previous["commit"]], cwd=ROOT))
            checkout = self.work / "previous"
            checkout.mkdir()
            with tarfile.open(archive) as tar:
                tar.extractall(checkout, filter="data")
            root = checkout / "infra/envs/staging/release"
            self.init(root)
            plan, _ = self.plan(root, previous["inputs"], "rollback")
            self.tf(root, "apply", "-input=false", "-lock-timeout=5m", str(plan))
            self.verify(previous, root)
            record["phase"] = "rollback_awaiting_smoke"
            self.put(self.key, record)
            self.smoke_outputs(previous)
            output(rollback="true")
            print("previous_release_restored_no_down_migration")
        else:
            # First release or an unreviewed schema change: stop through the release owner.
            root = ROOT / "infra/envs/staging/release"
            self.init(root)
            inputs = dict(record["inputs"], services_enabled=False)
            plan, _ = self.plan(root, inputs, "stop")
            self.tf(root, "apply", "-input=false", "-lock-timeout=5m", str(plan))
            record["phase"] = "stopped_forward_repair_required" if previous else "stopped_failed_bootstrap"
            self.put(self.key, record)
            print(record["phase"])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["deploy", "accept", "recover", "recovery-verified"])
    action = parser.parse_args().action
    os.umask(0o077)
    with tempfile.TemporaryDirectory(prefix="ernote-release-") as work:
        deployment = Deployment(work)
        if action == "recovery-verified":
            record = deployment.get(deployment.key)
            require(record["phase"] == "rollback_awaiting_smoke", "rollback_smoke_phase")
            record["phase"] = "rolled_back"
            deployment.put(deployment.key, record)
        else:
            getattr(deployment, action)()


if __name__ == "__main__":
    try:
        main()
    except GateError as error:
        sys.exit(str(error))
    except Exception:
        sys.exit("release_failed_invalid_contract_or_tool_response")
