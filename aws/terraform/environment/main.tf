# A Triple-C environment: the pieces every user's tasks share. Each user's
# own roles, EFS file system and secrets come from the onboarding module.

locals {
  # ENVIRONMENT_MODULE_VERSION in app/src-tauri/src/aws/tofu_modules.rs.
  template_version = 1
  name             = "triple-c-${var.environment_name}"
}

provider "aws" {
  region = var.region

  # The only place the component tag is set: the admin policy's
  # destruction rights are scoped to it, so no resource may override it.
  default_tags {
    tags = {
      "triple-c:component"        = "environment"
      "triple-c:environment"      = var.environment_name
      "triple-c:template-version" = tostring(local.template_version)
    }
  }
}

data "aws_caller_identity" "current" {}

data "aws_partition" "current" {}

resource "aws_ecs_cluster" "this" {
  name = local.name

  setting {
    name  = "containerInsights"
    value = "disabled"
  }
}

resource "aws_ecs_cluster_capacity_providers" "this" {
  cluster_name       = aws_ecs_cluster.this.name
  capacity_providers = ["FARGATE", "FARGATE_SPOT"]

  default_capacity_provider_strategy {
    capacity_provider = "FARGATE"
    weight            = 1
  }
}

# Tasks are reached only through SSM, so nothing may connect in.
resource "aws_security_group" "tasks" {
  name        = "triple-c-${var.environment_name}-tasks"
  description = "Triple-C ${var.environment_name} tasks: outbound only"
  vpc_id      = local.vpc_id

  # Empty, not absent: OpenTofu then removes any rule added out of band.
  ingress = []

  egress {
    description      = "All outbound"
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }
}

resource "aws_kms_key" "this" {
  description             = "Triple-C environment ${var.environment_name}"
  enable_key_rotation     = true
  deletion_window_in_days = 7

  # Delegate to IAM: who may use the key is decided by IAM policies (and the
  # permissions boundary), not by the key policy.
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "EnableIAMPolicies"
      Effect    = "Allow"
      Principal = { AWS = "arn:${data.aws_partition.current.partition}:iam::${data.aws_caller_identity.current.account_id}:root" }
      Action    = "kms:*"
      Resource  = "*"
    }]
  })
}

resource "aws_kms_alias" "this" {
  name          = "alias/${local.name}"
  target_key_id = aws_kms_key.this.key_id
}

resource "aws_cloudwatch_log_group" "this" {
  name              = "/triple-c/${var.environment_name}"
  retention_in_days = var.log_retention_days
}

# The environment record (EnvironmentInfo schema 1). Written last, so its
# presence means the environment is complete. The app treats it as untrusted.
resource "aws_ssm_parameter" "environment" {
  name = "/triple-c/environments/${var.environment_name}"
  type = "String"
  tier = "Standard"
  value = jsonencode({
    schema            = 1
    name              = var.environment_name
    template_version  = local.template_version
    cluster_arn       = aws_ecs_cluster.this.arn
    security_group_id = aws_security_group.tasks.id
    subnet_ids        = local.task_subnet_ids
    assign_public_ip  = local.assign_public_ip
    kms_key_arn       = aws_kms_key.this.arn
    log_group         = aws_cloudwatch_log_group.this.name
    network_mode      = var.network_mode
    tenancy           = "strict"
  })

  depends_on = [
    aws_ecs_cluster_capacity_providers.this,
    aws_kms_alias.this,
    aws_route.public_internet,
    aws_route.private_nat,
    aws_route_table_association.public,
    aws_route_table_association.private,
  ]
}
