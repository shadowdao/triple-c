terraform {
  # OpenTofu 1.10+ or Terraform 1.10+: S3-native state locking (`use_lockfile`).
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "6.73.0"
    }
  }

  # Filled in by the app (or an admin bundle's backend.hcl) with
  # `-backend-config`: bucket, key, region, use_lockfile = true.
  backend "s3" {}
}
