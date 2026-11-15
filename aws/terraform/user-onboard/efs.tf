# The user's own file system (D2): only their task role may mount it, and
# only through an access point, over TLS.

# The environment's subnets, for the mount-target group's VPC and to check
# the two subnets can each hold one mount target.
data "aws_subnet" "task" {
  count = 2
  id    = local.env.subnet_ids[count.index]
}

resource "aws_efs_file_system" "this" {
  creation_token   = local.name
  encrypted        = true
  performance_mode = "generalPurpose"
  throughput_mode  = "elastic"

  tags = { Name = local.name }
}

# Admits NFS from the environment's task group, which itself gains nothing.
resource "aws_security_group" "efs" {
  name        = "${local.name}-efs"
  description = "Triple-C ${var.environment_name}: NFS to one user's file system"
  vpc_id      = data.aws_subnet.task[0].vpc_id

  ingress {
    description     = "NFS from the environment's tasks"
    from_port       = 2049
    to_port         = 2049
    protocol        = "tcp"
    security_groups = [local.env.security_group_id]
  }

  # Mount targets never open connections and groups are stateful, so NFS
  # replies need no rule. Empty, not absent: OpenTofu then removes any rule
  # added out of band (an absent egress would keep AWS's allow-all default).
  egress = []

  lifecycle {
    precondition {
      condition = (
        data.aws_subnet.task[0].vpc_id == data.aws_subnet.task[1].vpc_id
        && data.aws_subnet.task[0].availability_zone != data.aws_subnet.task[1].availability_zone
      )
      error_message = "The environment's two subnets must be in one VPC and in different availability zones."
    }
  }
}

resource "aws_efs_mount_target" "this" {
  count           = 2
  file_system_id  = aws_efs_file_system.this.id
  subnet_id       = local.env.subnet_ids[count.index]
  security_groups = [aws_security_group.efs.id]
}

resource "aws_efs_file_system_policy" "this" {
  file_system_id = aws_efs_file_system.this.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid       = "DenyInsecureTransport"
        Effect    = "Deny"
        Principal = { AWS = "*" }
        Action    = "*"
        Resource  = aws_efs_file_system.this.arn
        Condition = { Bool = { "aws:SecureTransport" = "false" } }
      },
      {
        Sid       = "DenyWithoutAccessPoint"
        Effect    = "Deny"
        Principal = { AWS = "*" }
        Action = [
          "elasticfilesystem:ClientMount",
          "elasticfilesystem:ClientWrite",
          "elasticfilesystem:ClientRootAccess",
        ]
        Resource  = aws_efs_file_system.this.arn
        Condition = { Null = { "elasticfilesystem:AccessPointArn" = "true" } }
      },
      {
        Sid       = "OwnersTaskRoleOnly"
        Effect    = "Allow"
        Principal = { AWS = aws_iam_role.task.arn }
        Action    = ["elasticfilesystem:ClientMount", "elasticfilesystem:ClientWrite"]
        Resource  = aws_efs_file_system.this.arn
      },
    ]
  })
}
