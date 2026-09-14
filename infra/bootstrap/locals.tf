locals {
  # Bucket names are global across all of AWS, so they carry the account and region to
  # stay unique. They deliberately carry nothing else: no clinician name, no patient
  # identifier, no hospital name.
  state_bucket_name  = "${var.project}-tfstate-${var.aws_account_id}-${var.aws_region}"
  deploy_bucket_name = "${var.project}-deploy-${var.aws_account_id}-${var.aws_region}"

  # Every Terraform root that will ever exist, with the state key it owns. Each scope
  # gets its own IAM policy and its own assumable role, so a credential that can write
  # staging state cannot read or write production state.
  #
  # `environment = null` means the scope publishes and consumes no deployment manifest.
  state_scopes = {
    "bootstrap" = {
      prefix      = "bootstrap"
      environment = null
      manifest    = "none"
      description = "State buckets, encryption key, and the state-access policies themselves."
    }
    "staging-foundation" = {
      prefix      = "staging/foundation"
      environment = "staging"
      manifest    = "write"
      description = "Staging network, data stores, identity, edge, observability, ECR, and cluster."
    }
    "staging-release" = {
      prefix      = "staging/release"
      environment = "staging"
      manifest    = "read"
      description = "Staging ECS services and task definitions only."
    }
    "prod-foundation" = {
      prefix      = "prod/foundation"
      environment = "prod"
      manifest    = "write"
      description = "Production foundation. Authored now, unprovisioned until explicitly enabled."
    }
    "prod-release" = {
      prefix      = "prod/release"
      environment = "prod"
      manifest    = "read"
      description = "Production ECS services and task definitions only."
    }
  }

  # One manifest per environment. The foundation root writes it; the release root reads
  # it. This is how release CI learns resource names without being handed read access to
  # an entire foundation state file.
  manifest_keys = {
    for env in distinct([for s in local.state_scopes : s.environment if s.environment != null]) :
    env => "envs/${env}/foundation-manifest.json"
  }
}
