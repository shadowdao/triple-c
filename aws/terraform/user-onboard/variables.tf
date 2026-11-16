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

# Optional per-user EC2 capacity (phase 7). Empty = none: the module then
# makes nothing for EC2 and the user's projects run on Fargate only.
variable "ec2_instance_type" {
  description = "The instance type of the user's own EC2 capacity (e.g. m7i.large or m7g.large), or empty for none. Only the types the app can size are accepted."
  type        = string
  default     = ""

  # The app's table (`ec2_instance_spec`) and the admin policy's lists.
  validation {
    condition = var.ec2_instance_type == "" || contains([
      "m7i.large", "m7i.xlarge", "m7i.2xlarge", "m7i.4xlarge",
      "m7g.large", "m7g.xlarge", "m7g.2xlarge", "m7g.4xlarge",
      "c7i.large", "c7i.xlarge", "c7i.2xlarge", "c7i.4xlarge",
      "c7g.large", "c7g.xlarge", "c7g.2xlarge", "c7g.4xlarge",
      "r7i.large", "r7i.xlarge", "r7i.2xlarge", "r7i.4xlarge",
      "r7g.large", "r7g.xlarge", "r7g.2xlarge", "r7g.4xlarge",
      "t3.large", "t3.xlarge", "t3.2xlarge",
      "t4g.large", "t4g.xlarge", "t4g.2xlarge",
    ], var.ec2_instance_type)
    error_message = "ec2_instance_type must be empty or one of the m7i, m7g, c7i, c7g, r7i, r7g (large to 4xlarge), t3 or t4g (large to 2xlarge) types."
  }
}

variable "ec2_max_instances" {
  description = "How many of the user's instances may run at once (1 to 4); each holds one workspace as expected use."
  type        = number
  default     = 1

  validation {
    condition     = var.ec2_max_instances >= 1 && var.ec2_max_instances <= 4 && floor(var.ec2_max_instances) == var.ec2_max_instances
    error_message = "ec2_max_instances must be a whole number from 1 to 4."
  }
}

variable "ec2_user_tag" {
  description = "The value of the triple-c-user principal tag the user signs in with; names their EC2 cluster (triple-c-<environment>--u-<tag>). Required with ec2_instance_type."
  type        = string
  default     = ""

  validation {
    condition = var.ec2_user_tag == "" || (
      can(regex("^[A-Za-z0-9-]{1,32}$", var.ec2_user_tag))
      && !startswith(var.ec2_user_tag, "-")
      && !endswith(var.ec2_user_tag, "-")
      && !strcontains(var.ec2_user_tag, "--")
    )
    error_message = "ec2_user_tag must be 1 to 32 letters, digits and single hyphens, not starting or ending with a hyphen."
  }

  validation {
    condition     = var.ec2_instance_type == "" || var.ec2_user_tag != ""
    error_message = "ec2_user_tag (the user's triple-c-user principal tag) is required when ec2_instance_type is set."
  }
}
