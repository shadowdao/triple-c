# The user's own EC2 capacity (phase 7, E1–E4, E11), made only when
# `ec2_instance_type` is set: every resource and data source here has
# `count = local.ec2_count`, so an onboarding without it plans exactly as
# before. One cluster per user (`triple-c-<env>--u-<user tag>`, the name the
# workspace policy lets only that `triple-c-user` principal tag run tasks on)
# with exactly one capacity provider: an Auto Scaling group of 0 to
# `ec2_max_instances` instances that ECS's managed scaling grows when a task
# needs one and shrinks back to zero when none does.
#
# Removing it (re-applying with an empty type, or destroying) goes in
# dependency order: the user record first, then the cluster's provider list
# (ECS refuses while a task still runs on the provider, so a running
# workspace stops the removal before anything is torn down), the capacity
# provider, the group (`force_delete` off: the provider first drains it to
# zero, clearing scale-in protection, and waits for the instances to
# terminate), the launch template, the instance profile and role, and last
# the cluster, which the launch template's user data references so that it
# outlives every instance registered into it.

locals {
  ec2       = var.ec2_instance_type != ""
  ec2_count = local.ec2 ? 1 : 0

  # `user_ec2_stem` and `user_instance_role_name` in tofu_modules.rs.
  ec2_name           = "triple-c-${var.environment_name}--u-${var.ec2_user_tag}"
  instance_role_name = "${local.name}-instance"

  # Graviton families (m7g, c7g, r7g, t4g: a `g` after the generation) run
  # the arm64 image.
  ec2_arm64 = can(regex("^[a-z]+[0-9]+g[a-z]*\\.", var.ec2_instance_type))
  ecs_ami_parameter = (
    local.ec2_arm64
    ? "/aws/service/ecs/optimized-ami/amazon-linux-2023/arm64/recommended/image_id"
    : "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id"
  )
}

# The provider's default tags (component, environment, owner, version), for
# the places they don't reach by themselves: the launch template's tag
# specifications and the group's tags.
data "aws_default_tags" "current" {
  count = local.ec2_count
}

# The current ECS-optimized Amazon Linux 2023 image, resolved at apply time.
data "aws_ssm_parameter" "ecs_ami" {
  count = local.ec2_count
  name  = local.ecs_ami_parameter
}

resource "aws_ecs_cluster" "ec2" {
  count = local.ec2_count
  name  = local.ec2_name

  setting {
    name  = "containerInsights"
    value = "disabled"
  }
}

# The instance's role: the ECS agent on this user's cluster only (the
# boundary allows the agent's calls only on the cluster this role's
# `triple-c:cluster` tag names), image pulls and the environment's logs.
# ECS Exec needs nothing here: its channels run on the task role.
resource "aws_iam_role" "instance" {
  count                = local.ec2_count
  name                 = local.instance_role_name
  description          = "Triple-C ${var.environment_name}: EC2 instances of ${var.owner_id}"
  permissions_boundary = local.boundary_arn
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })

  tags = { "triple-c:cluster" = local.ec2_name }
}

resource "aws_iam_role_policy" "instance" {
  count = local.ec2_count
  name  = "triple-c-instance"
  role  = aws_iam_role.instance[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "EcsAgentOnOwnCluster"
        Effect = "Allow"
        Action = [
          "ecs:RegisterContainerInstance",
          "ecs:DeregisterContainerInstance",
          "ecs:Poll",
          "ecs:StartTelemetrySession",
          "ecs:Submit*",
          "ecs:UpdateContainerInstancesState",
        ]
        Resource = [
          "arn:${local.partition}:ecs:${var.region}:${local.account}:cluster/${local.ec2_name}",
          "arn:${local.partition}:ecs:${var.region}:${local.account}:container-instance/${local.ec2_name}/*",
        ]
      },
      {
        Sid      = "EcsAgentDiscovery"
        Effect   = "Allow"
        Action   = "ecs:DiscoverPollEndpoint"
        Resource = "*"
      },
      {
        Sid      = "EcrAuthorization"
        Effect   = "Allow"
        Action   = "ecr:GetAuthorizationToken"
        Resource = "*"
      },
      {
        Sid    = "PullTripleCImages"
        Effect = "Allow"
        Action = [
          "ecr:BatchGetImage",
          "ecr:GetDownloadUrlForLayer",
          "ecr:BatchCheckLayerAvailability",
        ]
        Resource = [
          "arn:${local.partition}:ecr:${var.region}:${local.account}:repository/triple-c-*",
          "arn:${local.partition}:ecr:${var.region}:${local.account}:repository/triple-c/*",
        ]
      },
      {
        # The awslogs driver runs in the instance's Docker daemon.
        Sid      = "WriteEnvironmentLogs"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
        Resource = local.log_group_arn
      },
    ]
  })
}

resource "aws_iam_instance_profile" "instance" {
  count = local.ec2_count
  name  = aws_iam_role.instance[0].name
  role  = aws_iam_role.instance[0].name
}

resource "aws_launch_template" "ec2" {
  count       = local.ec2_count
  name        = local.ec2_name
  description = "Triple-C ${var.environment_name}: EC2 capacity of ${var.owner_id}"

  image_id      = data.aws_ssm_parameter.ecs_ami[0].insecure_value
  instance_type = var.ec2_instance_type

  iam_instance_profile {
    name = aws_iam_instance_profile.instance[0].name
  }

  # The environment's tasks group (outbound only), named in the template so
  # the admin policy sees it as a launch-template resource. A public IP only
  # where the environment's tasks get one: in a public-subnet environment
  # the agent reaches ECS through the internet gateway.
  network_interfaces {
    device_index                = 0
    security_groups             = [local.env.security_group_id]
    associate_public_ip_address = local.env.assign_public_ip
    delete_on_termination       = true
  }

  # IMDSv2 only, one hop: the agent (host network) reaches it, bridge
  # containers don't, and awsvpc tasks are blocked by the agent below.
  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
    instance_metadata_tags      = "disabled"
  }

  block_device_mappings {
    device_name = "/dev/xvda"

    ebs {
      volume_size           = 50
      volume_type           = "gp3"
      encrypted             = true
      delete_on_termination = true
    }
  }

  user_data = base64encode(<<-EOT
    #!/bin/bash
    cat >>/etc/ecs/ecs.config <<'EOF'
    ECS_CLUSTER=${aws_ecs_cluster.ec2[0].name}
    ECS_AWSVPC_BLOCK_IMDS=true
    EOF
  EOT
  )

  # Default tags don't reach what a launch creates: the instance, its
  # volume and its interface are tagged here (the admin policy lets a launch
  # create only component-tagged instances and volumes).
  tag_specifications {
    resource_type = "instance"
    tags          = data.aws_default_tags.current[0].tags
  }

  tag_specifications {
    resource_type = "volume"
    tags          = data.aws_default_tags.current[0].tags
  }

  tag_specifications {
    resource_type = "network-interface"
    tags          = data.aws_default_tags.current[0].tags
  }
}

resource "aws_autoscaling_group" "ec2" {
  count               = local.ec2_count
  name                = local.ec2_name
  vpc_zone_identifier = local.env.subnet_ids

  # Zero when idle; ECS's managed scaling sets the desired count.
  min_size              = 0
  max_size              = var.ec2_max_instances
  protect_from_scale_in = true
  force_delete          = false

  # A fixed version, never $Latest/$Default (the admin policy refuses a
  # group that would follow later versions). Running instances keep theirs
  # until they scale in.
  launch_template {
    id      = aws_launch_template.ec2[0].id
    version = aws_launch_template.ec2[0].latest_version
  }

  # Provider default tags don't apply to groups.
  dynamic "tag" {
    for_each = data.aws_default_tags.current[0].tags
    content {
      key                 = tag.key
      value               = tag.value
      propagate_at_launch = false
    }
  }

  # ECS adds this when the capacity provider takes the group over; declared
  # so a re-apply doesn't remove it.
  tag {
    key                 = "AmazonECSManaged"
    value               = "true"
    propagate_at_launch = true
  }

  # The drain before deletion waits for the instances to terminate.
  timeouts {
    delete = "20m"
  }

  lifecycle {
    ignore_changes = [desired_capacity]
  }
}

resource "aws_ecs_capacity_provider" "ec2" {
  count = local.ec2_count
  name  = local.ec2_name

  auto_scaling_group_provider {
    auto_scaling_group_arn = aws_autoscaling_group.ec2[0].arn

    # ECS protects an instance from scale-in while a task runs on it.
    managed_termination_protection = "ENABLED"

    managed_scaling {
      status                    = "ENABLED"
      target_capacity           = 100
      minimum_scaling_step_size = 1
      maximum_scaling_step_size = 1
      instance_warmup_period    = 60
    }
  }
}

resource "aws_ecs_cluster_capacity_providers" "ec2" {
  count              = local.ec2_count
  cluster_name       = aws_ecs_cluster.ec2[0].name
  capacity_providers = [aws_ecs_capacity_provider.ec2[0].name]

  default_capacity_provider_strategy {
    capacity_provider = aws_ecs_capacity_provider.ec2[0].name
    weight            = 1
  }
}

# The user's record (E11): what the app reads to run EC2 projects, since
# workspace users can't read the state bucket. Outside
# /triple-c/environments/, so environment detection never sees it. Written
# last, removed first.
resource "aws_ssm_parameter" "user" {
  count       = local.ec2_count
  name        = "/triple-c/users/${var.environment_name}/${var.owner_key}"
  description = "Triple-C ${var.environment_name}: EC2 capacity of ${var.owner_id}"
  type        = "String"
  value = jsonencode({
    schema            = 1
    instance_type     = var.ec2_instance_type
    cluster_arn       = aws_ecs_cluster.ec2[0].arn
    capacity_provider = aws_ecs_capacity_provider.ec2[0].name
    user_tag          = var.ec2_user_tag
  })

  depends_on = [aws_ecs_cluster_capacity_providers.ec2]
}
