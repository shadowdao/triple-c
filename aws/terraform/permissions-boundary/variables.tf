variable "region" {
  description = "The region the provider talks to (IAM itself is global), e.g. us-west-2."
  type        = string

  validation {
    condition = (
      can(regex("^[a-z]{2}(-[a-z]+)+-[0-9]+$", var.region))
      && length([for p in ["cn-", "us-gov-", "us-iso-", "us-isob-", "eu-isoe-", "us-isof-"] : p if startswith(var.region, p)]) == 0
    )
    error_message = "region must be a commercial AWS region id such as us-west-2 (China and GovCloud are not supported)."
  }
}

variable "policy_json" {
  description = "The boundary document: the app passes aws/policies/permissions-boundary.json verbatim, so this module holds no copy."
  type        = string

  validation {
    condition     = can(jsondecode(var.policy_json).Statement)
    error_message = "policy_json must be an IAM policy document (JSON with a Statement)."
  }
}
