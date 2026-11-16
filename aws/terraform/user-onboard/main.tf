# One user's place in an environment: their task and execution roles and
# their own EFS file system (D2, strict tenancy), and optionally their own
# EC2 capacity (ec2.tf, phase 7). The environment itself is read only from
# its record, never from its state.

locals {
  # ONBOARD_MODULE_VERSION in app/src-tauri/src/aws/tofu_modules.rs.
  template_version = 2
  name             = "triple-c-${var.environment_name}-${var.owner_key}"

  partition = data.aws_partition.current.partition
  account   = data.aws_caller_identity.current.account_id

  # The environment record (EnvironmentInfo schema 1), checked below.
  env = jsondecode(data.aws_ssm_parameter.environment.insecure_value)

  boundary_arn  = "arn:${local.partition}:iam::${local.account}:policy/triple-c-permissions-boundary"
  log_group_arn = "arn:${local.partition}:logs:${var.region}:${local.account}:log-group:${local.env.log_group}:*"
}

provider "aws" {
  region = var.region

  # The only place the component tag is set: the admin policy's
  # destruction rights are scoped to it, so no resource may override it.
  default_tags {
    tags = {
      "triple-c:component"        = "user-onboarding"
      "triple-c:environment"      = var.environment_name
      "triple-c:owner"            = var.owner_id
      "triple-c:template-version" = tostring(local.template_version)
    }
  }
}

data "aws_caller_identity" "current" {}

data "aws_partition" "current" {}

# The record is a String parameter (not secret), so its non-sensitive value
# is read: subnet ids drive `count`, which a sensitive value cannot.
data "aws_ssm_parameter" "environment" {
  name            = "/triple-c/environments/${var.environment_name}"
  with_decryption = false

  # The record is untrusted input that feeds IAM policies and the network:
  # every field this module uses is format-checked before anything is built.
  lifecycle {
    postcondition {
      condition = try(
        jsondecode(self.insecure_value).schema == 1
        && jsondecode(self.insecure_value).name == var.environment_name
        && jsondecode(self.insecure_value).tenancy == "strict"
        && length(jsondecode(self.insecure_value).subnet_ids) == 2,
        false
      )
      error_message = "The environment record is missing or not a schema-1 strict-tenancy record with two subnets; deploy (or update) the environment first."
    }

    postcondition {
      condition = try(
        can(regex(
          "^arn:aws[a-z-]*:kms:${var.region}:${data.aws_caller_identity.current.account_id}:key/[0-9a-f-]+$",
          jsondecode(self.insecure_value).kms_key_arn
        ))
        && jsondecode(self.insecure_value).log_group == "/triple-c/${var.environment_name}"
        && can(regex("^sg-[0-9a-f]{8,17}$", jsondecode(self.insecure_value).security_group_id))
        && alltrue([for s in jsondecode(self.insecure_value).subnet_ids : can(regex("^subnet-[0-9a-f]{8,17}$", s))])
        && (jsondecode(self.insecure_value).assign_public_ip == true || jsondecode(self.insecure_value).assign_public_ip == false),
        false
      )
      error_message = "The environment record has a malformed kms_key_arn (not a key in this region and account), log_group (not /triple-c/<environment>), security_group_id, subnet id or assign_public_ip."
    }
  }
}
