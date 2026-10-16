# Mocked plans: no AWS credentials and nothing created. Synthetic identifiers only.
# Plan only, because the pool's prevent_destroy forbids test teardown. Assertions
# read configured values; the computed schema is pinned through its only source,
# local.custom_attributes, and the pool's postcondition checks the real schema.
mock_provider "aws" {
}

variables {
  name        = "ernote-synthetic-test"
  environment = "prod"
}

run "default_pool_is_admin_only_mfa_and_srp" {
  command = plan

  assert {
    condition = (
      aws_cognito_user_pool.this.admin_create_user_config[0].allow_admin_create_user_only &&
      aws_cognito_user_pool.this.mfa_configuration == "ON" &&
      aws_cognito_user_pool.this.deletion_protection == "ACTIVE" &&
      length(local.custom_attributes) == 0
    )
    error_message = "The pool must refuse self-signup, require MFA, resist deletion and define no custom attributes such as custom:org_id."
  }
  assert {
    condition = (
      !aws_cognito_user_pool_client.native.generate_secret &&
      sort(tolist(aws_cognito_user_pool_client.native.explicit_auth_flows)) == sort(["ALLOW_REFRESH_TOKEN_AUTH", "ALLOW_USER_SRP_AUTH"]) &&
      aws_cognito_user_pool_client.native.enable_token_revocation &&
      aws_cognito_user_pool_client.native.prevent_user_existence_errors == "ENABLED"
    )
    error_message = "The native client has no secret, uses SRP and refresh only, and supports revocation."
  }
  assert {
    condition = (
      aws_cognito_user_pool_client.native.access_token_validity == 60 &&
      aws_cognito_user_pool_client.native.refresh_token_validity == 12 &&
      aws_cognito_user_pool_client.native.token_validity_units[0].access_token == "minutes" &&
      aws_cognito_user_pool_client.native.token_validity_units[0].refresh_token == "hours"
    )
    error_message = "Token durations must be explicit."
  }
  assert {
    condition = (
      !aws_cognito_user_pool_client.native.allowed_oauth_flows_user_pool_client &&
      !local.hosted_login
    )
    error_message = "Hosted login stays off until exact redirects are supplied."
  }
  assert {
    condition = (
      length(aws_secretsmanager_secret.synthetic_smoke) == 0 &&
      length(aws_kms_key.synthetic_credentials) == 0
    )
    error_message = "Without the staging exception there is no synthetic credential secret."
  }
}

run "hosted_login_is_code_flow_with_exact_redirects" {
  command = plan
  variables {
    callback_urls = ["https://staging.example.test/callback", "com.example.ernote:/oauth2redirect"]
    logout_urls   = ["https://staging.example.test/signed-out"]
  }

  assert {
    condition = (
      aws_cognito_user_pool_client.native.allowed_oauth_flows_user_pool_client &&
      length(aws_cognito_user_pool_client.native.allowed_oauth_flows) == 1 &&
      contains(aws_cognito_user_pool_client.native.allowed_oauth_flows, "code") &&
      length(aws_cognito_user_pool_client.native.callback_urls) == 2
    )
    error_message = "Hosted login uses only the authorization-code flow with the reviewed URLs."
  }
}

run "staging_smoke_exception_is_labeled_and_scoped" {
  command = plan
  variables {
    environment               = "staging"
    synthetic_smoke_exception = true
  }

  assert {
    condition = (
      aws_cognito_user_pool.this.mfa_configuration == "OPTIONAL" &&
      contains(tolist(aws_cognito_user_pool_client.native.explicit_auth_flows), "ALLOW_USER_PASSWORD_AUTH") &&
      !aws_cognito_user_pool_client.native.generate_secret
    )
    error_message = "The exception allows password sign-in without MFA, still without a client secret."
  }
  assert {
    condition = (
      aws_secretsmanager_secret.synthetic_smoke[0].name == "ernote-synthetic-test/synthetic-smoke-credentials" &&
      aws_kms_key.synthetic_credentials[0].enable_key_rotation &&
      aws_secretsmanager_secret.synthetic_smoke[0].recovery_window_in_days == 7
    )
    error_message = "Synthetic credentials live in one recoverable secret under a dedicated rotating key."
  }
}

run "reject_smoke_exception_in_production" {
  command = plan
  variables { synthetic_smoke_exception = true }
  expect_failures = [var.synthetic_smoke_exception]
}

run "reject_wildcard_callback" {
  command = plan
  variables { callback_urls = ["https://*.example.test/callback"] }
  expect_failures = [var.callback_urls]
}

run "reject_plain_http_callback" {
  command = plan
  variables { callback_urls = ["http://staging.example.test/callback"] }
  expect_failures = [var.callback_urls]
}

run "reject_logout_without_hosted_login" {
  command = plan
  variables { logout_urls = ["https://staging.example.test/signed-out"] }
  expect_failures = [var.logout_urls]
}

run "reject_long_access_tokens" {
  command = plan
  variables { access_token_minutes = 1440 }
  expect_failures = [var.access_token_minutes]
}
