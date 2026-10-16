# ---------------------------------------------------------------------------
# S7 authentication
# ---------------------------------------------------------------------------
# A Cognito pool proves who a caller is; it never says which org they belong to.
# The database `clinicians` row is the only authority for membership, so the pool
# has no custom attributes at all (no `custom:org_id`) and a successful login
# provisions nothing. Users are created by the onboarding procedure, not here:
# Terraform state must hold no user records or passwords.
#
# Replacing this pool changes every user's `sub` and orphans their clinician rows,
# so deletion protection is on and the pool is never replaced implicitly.

locals {
  hosted_login = length(var.callback_urls) > 0

  # SRP and refresh for every client. Plain password auth only under the
  # synthetic-staging smoke exception.
  auth_flows = concat(
    ["ALLOW_REFRESH_TOKEN_AUTH", "ALLOW_USER_SRP_AUTH"],
    var.synthetic_smoke_exception ? ["ALLOW_USER_PASSWORD_AUTH"] : [],
  )

  # The pool's only schema source (name => data type). Deliberately empty, and the
  # tests pin it so; org membership must never become a token claim.
  custom_attributes = {}
}

resource "aws_cognito_user_pool" "this" {
  name                = var.name
  deletion_protection = "ACTIVE"

  # Mandatory TOTP everywhere except the labeled synthetic smoke exception.
  mfa_configuration = var.synthetic_smoke_exception ? "OPTIONAL" : "ON"
  software_token_mfa_configuration {
    enabled = true
  }

  # No public self-signup; administrators create every user.
  admin_create_user_config {
    allow_admin_create_user_only = true
  }

  # No email/SMS recovery channel to take over an account through.
  account_recovery_setting {
    recovery_mechanism {
      name     = "admin_only"
      priority = 1
    }
  }

  username_configuration {
    case_sensitive = false
  }

  password_policy {
    minimum_length                   = 16
    require_lowercase                = true
    require_uppercase                = true
    require_numbers                  = true
    require_symbols                  = false
    temporary_password_validity_days = 1
  }

  dynamic "schema" {
    for_each = local.custom_attributes
    content {
      name                = schema.key
      attribute_data_type = schema.value
      mutable             = false
    }
  }

  # A plan that would replace the pool (and so every subject) fails instead.
  lifecycle {
    prevent_destroy = true

    # Checked against the real pool after apply, whatever the configuration says.
    postcondition {
      condition     = !anytrue([for attribute in self.schema : strcontains(lower(attribute.name), "org")])
      error_message = "The user pool must not carry an org attribute; the clinicians table is the only membership authority."
    }
  }
}

resource "aws_cognito_user_pool_client" "native" {
  name         = "${var.name}-native"
  user_pool_id = aws_cognito_user_pool.this.id

  # A CLI/native client cannot keep a secret, so it has none.
  generate_secret     = false
  explicit_auth_flows = local.auth_flows

  prevent_user_existence_errors = "ENABLED"
  enable_token_revocation       = true
  auth_session_validity         = 3

  access_token_validity  = var.access_token_minutes
  id_token_validity      = var.access_token_minutes
  refresh_token_validity = var.refresh_token_hours
  token_validity_units {
    access_token  = "minutes"
    id_token      = "minutes"
    refresh_token = "hours"
  }

  # Hosted login is off until exact redirects are reviewed. When on, it is the
  # authorization-code flow only: never implicit, never a wildcard redirect.
  supported_identity_providers         = ["COGNITO"]
  allowed_oauth_flows_user_pool_client = local.hosted_login
  allowed_oauth_flows                  = local.hosted_login ? ["code"] : []
  allowed_oauth_scopes                 = local.hosted_login ? ["openid"] : []
  callback_urls                        = var.callback_urls
  logout_urls                          = var.logout_urls
}

# Synthetic smoke credentials: one secret holding the synthetic users' usernames and
# passwords, under its own key, so the smoke role's decrypt grant reaches no
# application data. Terraform creates only the empty container; operators write the
# value out of band and it never appears in state, workflow logs or fixtures.
resource "aws_kms_key" "synthetic_credentials" {
  count = var.synthetic_smoke_exception ? 1 : 0

  description             = "${var.name} synthetic smoke login credentials only"
  enable_key_rotation     = true
  deletion_window_in_days = var.kms_deletion_window_days
}

resource "aws_kms_alias" "synthetic_credentials" {
  count = var.synthetic_smoke_exception ? 1 : 0

  name          = "alias/${var.name}-synthetic-credentials"
  target_key_id = aws_kms_key.synthetic_credentials[0].key_id
}

resource "aws_secretsmanager_secret" "synthetic_smoke" {
  count = var.synthetic_smoke_exception ? 1 : 0

  name                    = "${var.name}/synthetic-smoke-credentials"
  description             = "Synthetic staging smoke users only. Never a real clinician; never reused for patient data."
  kms_key_id              = aws_kms_key.synthetic_credentials[0].arn
  recovery_window_in_days = var.secret_recovery_window_days
}
