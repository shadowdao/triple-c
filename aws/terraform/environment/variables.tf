# Every rule here mirrors the app's (`app/src-tauri/src/aws/tofu_modules.rs`),
# so an admin running a bundle by hand gets the same answers as the app.

variable "environment_name" {
  description = "The environment's name: lowercase letters, digits and single hyphens, starting with a letter, at most 20 characters."
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
  description = "The AWS region to deploy into, e.g. us-west-2."
  type        = string

  validation {
    condition = (
      can(regex("^[a-z]{2}(-[a-z]+)+-[0-9]+$", var.region))
      && length([for p in ["cn-", "us-gov-", "us-iso-", "us-isob-", "eu-isoe-", "us-isof-"] : p if startswith(var.region, p)]) == 0
    )
    error_message = "region must be a commercial AWS region id such as us-west-2 (China and GovCloud are not supported)."
  }
}

variable "network_mode" {
  description = "public: a new VPC with two public subnets and no NAT. private-nat: a new VPC with two private subnets behind one NAT gateway (about $33/month). existing: the given vpc_id and subnet_ids."
  type        = string
  default     = "public"

  validation {
    condition     = contains(["public", "private-nat", "existing"], var.network_mode)
    error_message = "network_mode must be public, private-nat or existing."
  }
}

variable "vpc_id" {
  description = "network_mode = existing only: the VPC to run tasks in."
  type        = string
  default     = ""

  validation {
    condition = (
      var.network_mode == "existing"
      ? can(regex("^vpc-([0-9a-f]{8}|[0-9a-f]{17})$", var.vpc_id))
      : var.vpc_id == ""
    )
    error_message = "vpc_id must be a VPC id (vpc-xxxxxxxx) when network_mode = existing, and empty otherwise."
  }
}

variable "subnet_ids" {
  description = "network_mode = existing only: exactly two subnets of vpc_id, in different availability zones."
  type        = list(string)
  default     = []

  validation {
    condition = (
      var.network_mode == "existing"
      ? (
        length(var.subnet_ids) == 2
        && length(distinct(var.subnet_ids)) == 2
        && alltrue([for s in var.subnet_ids : can(regex("^subnet-([0-9a-f]{8}|[0-9a-f]{17})$", s))])
      )
      : length(var.subnet_ids) == 0
    )
    error_message = "subnet_ids must be exactly two distinct subnet ids (subnet-xxxxxxxx) when network_mode = existing, and empty otherwise."
  }
}

variable "assign_public_ip" {
  description = "network_mode = existing only: whether tasks get a public IP (needed in a public subnet without NAT)."
  type        = bool
  default     = false

  validation {
    condition     = var.network_mode == "existing" || !var.assign_public_ip
    error_message = "assign_public_ip applies only to network_mode = existing; the other modes decide it themselves."
  }
}

variable "log_retention_days" {
  description = "How long the environment's log group keeps task logs."
  type        = number
  default     = 30

  validation {
    condition     = contains([1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288, 3653], var.log_retention_days)
    error_message = "log_retention_days must be a CloudWatch Logs retention value (1, 3, 5, 7, 14, 30, 60, 90, ...)."
  }
}

variable "max_workspace_hours" {
  description = "Advisory cap on how long one workspace task may run, in hours (0 = no cap, at most 168). The app reads it from the environment record and enforces it inside the workspace."
  type        = number
  default     = 0

  validation {
    condition     = var.max_workspace_hours == floor(var.max_workspace_hours) && var.max_workspace_hours >= 0 && var.max_workspace_hours <= 168
    error_message = "max_workspace_hours must be a whole number from 0 to 168."
  }
}
