# The permissions boundary every role a Triple-C module creates must carry:
# the most any such role may ever hold. One per account.

locals {
  # BOUNDARY_MODULE_VERSION in app/src-tauri/src/aws/tofu_modules.rs.
  # 2: the instance role's ECS agent and Session Manager actions (phase 7).
  template_version = 2
}

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      "triple-c:component"        = "boundary"
      "triple-c:template-version" = tostring(local.template_version)
    }
  }
}

resource "aws_iam_policy" "boundary" {
  # BOUNDARY_POLICY_NAME in app/src-tauri/src/aws/tofu_modules.rs; no path,
  # so the ARN the other modules build is exact.
  name        = "triple-c-permissions-boundary"
  description = "Triple-C: the most any Triple-C-created role may hold"
  policy      = var.policy_json
}
