# Terraform and provider versions are pinned here for every root under infra/.
#
# required_version keeps the floor at 1.10, the first release whose S3 backend can
# take its own lock with a `.tflock` object instead of a DynamoDB table. The exact
# release this repository was tested with lives in `.terraform-version` at the repo
# root; CI installs that one. Terraform 1.11 promoted S3 native locking from
# experimental to generally available, so upgrading past 1.10 is encouraged.
terraform {
  required_version = ">= 1.10.0, < 2.0.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }
}
