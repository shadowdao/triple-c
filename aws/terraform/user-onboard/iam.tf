# The user's two roles. Both carry the permissions boundary and trust only
# ECS tasks in this account; the owner tag comes from default_tags.

# The role the user's container runs as.
resource "aws_iam_role" "task" {
  name                 = "${local.name}-task"
  description          = "Triple-C ${var.environment_name}: tasks of ${var.owner_id}"
  permissions_boundary = local.boundary_arn
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ecs-tasks.amazonaws.com" }
      Action    = "sts:AssumeRole"
      Condition = {
        StringEquals = { "aws:SourceAccount" = data.aws_caller_identity.current.account_id }
      }
    }]
  })
}

resource "aws_iam_role_policy" "task" {
  name = "triple-c-task"
  role = aws_iam_role.task.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "EcsExecChannels"
        Effect = "Allow"
        Action = [
          "ssmmessages:CreateControlChannel",
          "ssmmessages:CreateDataChannel",
          "ssmmessages:OpenControlChannel",
          "ssmmessages:OpenDataChannel",
        ]
        Resource = "*"
      },
      {
        # CreateLogStream is authorized on the log group; writing only into
        # the user's own streams (other users' are in the same group).
        Sid      = "CreateEnvironmentLogStreams"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream"]
        Resource = local.log_group_arn
      },
      {
        Sid      = "WriteOwnLogStreams"
        Effect   = "Allow"
        Action   = ["logs:PutLogEvents"]
        Resource = local.own_log_streams_arn
      },
      {
        Sid      = "MountOwnFileSystem"
        Effect   = "Allow"
        Action   = ["elasticfilesystem:ClientMount", "elasticfilesystem:ClientWrite"]
        Resource = aws_efs_file_system.this.arn
      },
    ]
  })
}

resource "aws_iam_role_policy" "task_bedrock" {
  count = var.bedrock_on_task_role ? 1 : 0
  name  = "triple-c-bedrock"
  role  = aws_iam_role.task.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid    = "InvokeBedrockModels"
      Effect = "Allow"
      Action = ["bedrock:InvokeModel", "bedrock:InvokeModelWithResponseStream"]
      # Cross-region inference profiles invoke models in other regions.
      Resource = [
        "arn:${local.partition}:bedrock:*::foundation-model/*",
        "arn:${local.partition}:bedrock:*:${local.account}:inference-profile/*",
        "arn:${local.partition}:bedrock:*:${local.account}:application-inference-profile/*",
      ]
    }]
  })
}

# The role ECS uses to start the user's tasks: pull their secrets, write logs.
resource "aws_iam_role" "exec" {
  name                 = "${local.name}-exec"
  description          = "Triple-C ${var.environment_name}: task execution for ${var.owner_id}"
  permissions_boundary = local.boundary_arn
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ecs-tasks.amazonaws.com" }
      Action    = "sts:AssumeRole"
      Condition = {
        StringEquals = { "aws:SourceAccount" = data.aws_caller_identity.current.account_id }
      }
    }]
  })
}

resource "aws_iam_role_policy" "exec" {
  name = "triple-c-exec"
  role = aws_iam_role.exec.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "ReadOwnSecrets"
        Effect   = "Allow"
        Action   = "secretsmanager:GetSecretValue"
        Resource = "arn:${local.partition}:secretsmanager:${var.region}:${local.account}:secret:triple-c/*"
        Condition = {
          StringEquals = { "aws:ResourceTag/triple-c:owner" = var.owner_id }
        }
      },
      {
        Sid      = "DecryptSecretsThroughSecretsManager"
        Effect   = "Allow"
        Action   = "kms:Decrypt"
        Resource = local.env.kms_key_arn
        Condition = {
          StringEquals = { "kms:ViaService" = "secretsmanager.${var.region}.amazonaws.com" }
        }
      },
      {
        # CreateLogStream is authorized on the log group; writing only into
        # the user's own streams (other users' are in the same group).
        Sid      = "CreateEnvironmentLogStreams"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream"]
        Resource = local.log_group_arn
      },
      {
        Sid      = "WriteOwnLogStreams"
        Effect   = "Allow"
        Action   = ["logs:PutLogEvents"]
        Resource = local.own_log_streams_arn
      },
    ]
  })
}
