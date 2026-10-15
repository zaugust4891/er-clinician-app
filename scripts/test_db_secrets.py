"""Offline regressions for the privileged credential staging/promotion boundary."""
import argparse
from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("db_secrets", Path(__file__).with_name("db-secrets.py"))
db_secrets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(db_secrets)


class RotationTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.args = argparse.Namespace(
            record=Path(self.work.name) / "rotation.json", region="us-east-2", account_id="000000000000",
            task="arn:aws:ecs:us-east-2:000000000000:task/ernote-staging/bootstrap",
            cluster="arn:aws:ecs:us-east-2:000000000000:cluster/ernote-staging", digest="sha256:" + "a" * 64,
        )
        prefix = "arn:aws:secretsmanager:us-east-2:000000000000:secret:"
        self.arns = {name: prefix + name + "-AbCdEf" for name in db_secrets.LOGIN_NAMES}
        self.identity = {
            "secret_arns": self.arns,
            "execution_role_arns": {"db-bootstrap": "arn:aws:iam::000000000000:role/ernote-staging-db-bootstrap-execution"},
            "ecs_secrets": {"db-bootstrap": {"DB_USERNAME": prefix + "master:username::", "DB_PASSWORD": prefix + "master:password::"}},
        }
        self.metadata = {arn: {} for arn in self.arns.values()}
        self.puts = []
        self.promotions = []
        self.definition = None
        self.task_changes = {}

    def aws(self, args, service, operation, payload):
        if operation == "describe-secret":
            return {"VersionIdsToStages": self.metadata[payload["SecretId"]]}
        if operation == "put-secret-value":
            self.puts.append(payload)
            self.metadata[payload["SecretId"]][payload["ClientRequestToken"]] = ["AWSPENDING"]
            return {}
        if operation == "update-secret-version-stage":
            self.promotions.append(payload)
            known = self.metadata[payload["SecretId"]]
            if payload.get("RemoveFromVersionId"):
                known[payload["RemoveFromVersionId"]].remove(payload["VersionStage"])
            if payload.get("MoveToVersionId"):
                known[payload["MoveToVersionId"]].append(payload["VersionStage"])
            return {}
        if operation == "describe-tasks":
            task = {"lastStatus": "STOPPED", "stopCode": "EssentialContainerExited",
                "taskDefinitionArn": "synthetic-bootstrap:1", "containers": [{"name": "db-bootstrap",
                "exitCode": 0, "imageDigest": args.digest}]}
            task.update(self.task_changes)
            return {"tasks": [task]}
        if operation == "describe-task-definition":
            return {"taskDefinition": self.definition}
        raise AssertionError(operation)

    def stage(self):
        with patch.object(db_secrets, "aws", self.aws), redirect_stdout(io.StringIO()):
            db_secrets.stage(self.args, self.arns)
        return json.loads(self.args.record.read_text())

    def definition_for(self, record):
        return {"executionRoleArn": self.identity["execution_role_arns"]["db-bootstrap"],
            "networkMode": "awsvpc", "requiresCompatibilities": ["FARGATE"], "containerDefinitions": [{
                "name": "db-bootstrap", "entryPoint": ["/usr/local/bin/er-db-bootstrap"],
                "image": "synthetic.example/ernote@" + self.args.digest,
                "secrets": db_secrets.bootstrap_secrets(self.identity, record),
            }]}

    def test_stage_is_resumable_and_record_never_contains_passwords(self):
        real = self.aws
        lost = False

        def lose_response(args, service, operation, payload):
            nonlocal lost
            result = real(args, service, operation, payload)
            if operation == "put-secret-value" and not lost:
                lost = True
                raise ValueError("lost_response")
            return result

        with patch.object(db_secrets, "aws", lose_response):
            with self.assertRaises(ValueError):
                db_secrets.stage(self.args, self.arns)
        record = self.stage()
        self.assertEqual(len(self.puts), 4)
        self.assertEqual(self.args.record.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("password", self.args.record.read_text())
        for put in self.puts:
            self.assertEqual(put["VersionStages"], ["AWSPENDING"])
            self.assertNotIn(json.loads(put["SecretString"])["password"], self.args.record.read_text())
        db_secrets.verify_record(record, self.args, self.arns)

    def test_an_unrelated_pending_rotation_is_never_displaced(self):
        self.metadata[next(iter(self.arns.values()))]["b" * 32] = ["AWSPENDING"]
        with self.assertRaises(ValueError):
            self.stage()
        self.assertEqual(self.puts, [])
        self.assertFalse(self.args.record.exists())

    def test_promote_requires_exact_versions_and_successful_task_digest(self):
        record = self.stage()
        self.definition = self.definition_for(record)
        self.definition["containerDefinitions"][0]["secrets"][0]["valueFrom"] = "different-rotation"
        with patch.object(db_secrets, "aws", self.aws):
            with self.assertRaises(ValueError):
                db_secrets.promote(self.args, self.arns, self.identity)
        self.assertEqual(self.promotions, [])
        self.definition = self.definition_for(record)
        with patch.object(db_secrets, "aws", self.aws), redirect_stdout(io.StringIO()):
            db_secrets.promote(self.args, self.arns, self.identity)
            db_secrets.promote(self.args, self.arns, self.identity)
        self.assertEqual(len(self.promotions), 8)
        for entry in record["logins"].values():
            self.assertEqual(self.metadata[entry["arn"]][entry["version_id"]], ["AWSCURRENT"])

    def test_bootstrap_definition_refuses_privileged_role_or_plaintext_overrides(self):
        record = self.stage()
        for change in ("task_role", "entry_point", "environment"):
            definition = self.definition_for(record)
            if change == "task_role":
                definition["taskRoleArn"] = "administrator"
            elif change == "entry_point":
                definition["containerDefinitions"][0]["entryPoint"] = ["sh"]
            else:
                definition["containerDefinitions"][0]["environment"] = [{"name": "DB_PASSWORD", "value": "unsafe"}]
            with self.assertRaises(ValueError):
                db_secrets.verify_definition(definition, self.identity, record)

    def test_failed_running_wrong_digest_and_overridden_tasks_cannot_promote(self):
        record = self.stage()
        self.definition = self.definition_for(record)
        for changes in (
            {"lastStatus": "RUNNING"},
            {"containers": [{"name": "db-bootstrap", "exitCode": 1, "imageDigest": self.args.digest}]},
            {"containers": [{"name": "db-bootstrap", "exitCode": 0, "imageDigest": "sha256:" + "b" * 64}]},
            {"overrides": {"containerOverrides": [{"name": "db-bootstrap", "command": ["sh"]}]}},
        ):
            self.task_changes = changes
            with patch.object(db_secrets, "aws", self.aws), self.assertRaises(ValueError):
                db_secrets.promote(self.args, self.arns, self.identity)
            self.assertEqual(self.promotions, [])


if __name__ == "__main__":
    unittest.main()
