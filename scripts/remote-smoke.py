#!/usr/bin/env python3
"""Fetch only synthetic credentials/fixture; send tokens to the Rust client on stdin."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request

from deploy import aws, require, run


def main():
    os.umask(0o077)
    region = os.environ["AWS_REGION"]
    require(region in ("us-east-1", "us-east-2", "us-west-2"), "smoke_region")
    credentials = json.loads(aws("secretsmanager", "get-secret-value",
                                 SecretId=os.environ["SMOKE_SECRET_ARN"])["SecretString"])
    tokens = {}
    for name in ("clinician_a", "clinician_b", "unprovisioned"):
        user = credentials[name]
        request = urllib.request.Request(f"https://cognito-idp.{region}.amazonaws.com/",
            data=json.dumps({"AuthFlow": "USER_PASSWORD_AUTH", "ClientId": os.environ["COGNITO_CLIENT_ID"],
                             "AuthParameters": {"USERNAME": user["username"], "PASSWORD": user["password"]}}).encode(),
            headers={"Content-Type": "application/x-amz-json-1.1", "X-Amz-Target": "AWSCognitoIdentityProviderService.InitiateAuth"})
        with urllib.request.urlopen(request, timeout=30) as response:
            tokens[name] = json.load(response)["AuthenticationResult"]
    with tempfile.TemporaryDirectory(prefix="ernote-synthetic-") as work:
        audio = Path(work) / "smoke.wav"
        run(["aws", "s3api", "get-object", "--bucket", os.environ["DEPLOY_ARTIFACTS_BUCKET"],
             "--key", "envs/staging/fixtures/smoke.wav", str(audio)], timeout=90)
        require(hashlib.sha256(audio.read_bytes()).hexdigest() == os.environ["SMOKE_WAV_SHA256"], "fixture_checksum")
        require(44 < audio.stat().st_size <= 10 * 1024 * 1024, "fixture_size")
        payload = {"base_url": os.environ["API_BASE_URL"].rstrip("/"),
                   "token_a": tokens["clinician_a"]["AccessToken"], "token_b": tokens["clinician_b"]["AccessToken"],
                   "id_token": tokens["clinician_a"]["IdToken"],
                   "unprovisioned_token": tokens["unprovisioned"]["AccessToken"],
                   "audio_path": str(audio), "expected_terms": json.loads(os.environ["SMOKE_EXPECTED_TERMS"])}
        # The child emits only assertions and synthetic UUIDs; errors remain inside it.
        completed = subprocess.run(["target/debug/examples/remote_smoke"], input=json.dumps(payload).encode(), timeout=930)
        require(completed.returncode == 0, "remote_smoke_failed")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        sys.exit("synthetic_smoke_failed_inspect_assertions_and_restricted_aws_evidence")
