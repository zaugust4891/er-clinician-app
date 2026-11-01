#!/usr/bin/env python3
"""Exchange GitHub's environment OIDC token without printing credentials or JWTs."""
import json
import os
import subprocess
import sys
import urllib.request


def main():
    request = urllib.request.Request(
        os.environ["ACTIONS_ID_TOKEN_REQUEST_URL"] + "&audience=sts.amazonaws.com",
        headers={"Authorization": "bearer " + os.environ["ACTIONS_ID_TOKEN_REQUEST_TOKEN"]},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        token = json.load(response)["value"]
    payload = {
        "RoleArn": os.environ["DEPLOY_ROLE_ARN"],
        "RoleSessionName": "github-" + os.environ["GITHUB_RUN_ID"],
        "WebIdentityToken": token,
        "DurationSeconds": 7200,
    }
    result = subprocess.run(
        ["aws", "sts", "assume-role-with-web-identity", "--cli-input-json", "file:///dev/stdin", "--output", "json"],
        input=json.dumps(payload), capture_output=True, text=True, timeout=60, check=True,
    )
    credentials = json.loads(result.stdout)["Credentials"]
    with open(os.environ["GITHUB_ENV"], "a") as env:
        for name, key in [("AWS_ACCESS_KEY_ID", "AccessKeyId"),
                          ("AWS_SECRET_ACCESS_KEY", "SecretAccessKey"),
                          ("AWS_SESSION_TOKEN", "SessionToken")]:
            value = credentials[key]
            print("::add-mask::" + value)
            env.write(name + "=" + value + "\n")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        sys.exit("OIDC credential exchange failed; inspect the environment trust and AWS audit record")
