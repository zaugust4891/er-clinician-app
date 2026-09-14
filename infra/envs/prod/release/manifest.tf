# ---------------------------------------------------------------------------
# Read the foundation's published contract
# ---------------------------------------------------------------------------
# The release root is deliberately small: ECS services and task definitions, and
# nothing else. It learns resource names from this one JSON object rather than from a
# `terraform_remote_state` read of the whole foundation, so the release identity's IAM
# policy can be scoped to exactly one object key.
#
# The provider returns `body` for JSON content types, which is why the manifest module
# sets `content_type = "application/json"`.

data "aws_s3_object" "manifest" {
  count = local.manifest_required ? 1 : 0

  bucket     = var.deploy_artifacts_bucket
  key        = local.manifest_key
  version_id = var.manifest_version_id
}

# A manifest is data fetched at plan time, so nothing else in this root should touch it
# before these checks have run. Preconditions fail the plan, not the apply.
resource "terraform_data" "manifest_guard" {
  count = local.manifest_required ? 1 : 0

  input = {
    environment     = local.manifest.environment
    schema_version  = local.manifest.schema_version
    generated_by    = local.manifest.generated_by
    version_id      = data.aws_s3_object.manifest[0].version_id
    resource_keys   = sort(keys(local.manifest.resources))
    secret_ref_keys = sort(keys(local.manifest.secret_refs))
  }

  lifecycle {
    precondition {
      condition     = local.manifest.schema_version == "1.0"
      error_message = "Unsupported manifest schema ${local.manifest.schema_version}. This release root understands 1.0."
    }

    precondition {
      condition     = local.manifest.environment == local.environment
      error_message = "Manifest is for ${local.manifest.environment} but this root deploys ${local.environment}."
    }

    precondition {
      condition     = local.manifest.account_id == var.aws_account_id
      error_message = "Manifest was published for account ${local.manifest.account_id}, not ${var.aws_account_id}."
    }

    precondition {
      condition     = local.manifest.region == var.aws_region
      error_message = "Manifest was published for region ${local.manifest.region}, not ${var.aws_region}."
    }

    precondition {
      condition     = local.manifest.project == var.project
      error_message = "Manifest was published for project ${local.manifest.project}, not ${var.project}."
    }

    # A foundation that has not finished applying publishes a manifest missing keys the
    # services need. Catching that here is the difference between a failed plan and a
    # task definition pointing at nothing.
    precondition {
      condition = length(setsubtract(local.required_resource_keys, keys(local.manifest.resources))) == 0
      error_message = format(
        "Foundation manifest is missing required resources: %s. Apply the %s foundation root first.",
        join(", ", sort(setsubtract(local.required_resource_keys, keys(local.manifest.resources)))),
        local.environment,
      )
    }

    # Defence in depth against the one mistake that would matter: the manifest module
    # validates this on the way out, and this validates it on the way in.
    precondition {
      condition = alltrue([
        for value in values(local.manifest.secret_refs) :
        can(regex("^arn:aws[a-z-]*:secretsmanager:", value))
      ])
      error_message = "Manifest secret_refs must contain Secrets Manager ARNs only, never secret values."
    }
  }
}
