# Every rule here mirrors the app's (`app/src-tauri/src/aws/tofu_modules.rs`),
# so an admin running a bundle by hand gets the same answers as the app.

variable "environment_name" {
  description = "The environment to join: lowercase letters, digits and single hyphens, starting with a letter, at most 20 characters."
  type        = string

  validation {
    condition = (
      can(regex("^[a-z][a-z0-9-]{0,19}$", var.environment_name))
      && !endswith(var.environment_name, "-")
      && !strcontains(var.environment_name, "--")
    )
    error_message = "environment_name must match ^[a-z][a-z0-9-]{0,19}$, not end in '-' and not contain '--'."
  }
}

variable "region" {
  description = "The environment's AWS region, e.g. us-west-2."
  type        = string

  validation {
    condition = (
      can(regex("^[a-z]{2}(-[a-z]+)+-[0-9]+$", var.region))
      && length([for p in ["cn-", "us-gov-", "us-iso-", "us-isob-", "eu-isoe-", "us-isof-"] : p if startswith(var.region, p)]) == 0
    )
    error_message = "region must be a commercial AWS region id such as us-west-2 (China and GovCloud are not supported)."
  }
}

variable "owner_id" {
  description = "The user's aws:userid (e.g. AROA...:alice@example.com). Every resource is tagged triple-c:owner with it."
  type        = string

  # Also a tag value, so only characters both allow.
  validation {
    condition     = can(regex("^[A-Za-z0-9_.:/=+@-]{1,128}$", var.owner_id))
    error_message = "owner_id must be an aws:userid: 1 to 128 letters, digits and _.:/=+@- characters."
  }
}

variable "owner_key" {
  description = "The first 12 hex characters of sha256(\"<region>:<owner_id>\"); names the user's roles, which reject the ':' in owner_id. The region is included because role names are account-wide and environments are regional."
  type        = string

  validation {
    condition     = can(regex("^[0-9a-f]{12}$", var.owner_key)) && var.owner_key == substr(sha256("${var.region}:${var.owner_id}"), 0, 12)
    error_message = "owner_key must be the first 12 hex characters of sha256(\"<region>:<owner_id>\")."
  }
}

variable "bedrock_on_task_role" {
  description = "Whether the task role may invoke Bedrock models (otherwise the workspace user's own credentials are used)."
  type        = bool
  default     = false
}
