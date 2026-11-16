output "task_role_arn" {
  value = aws_iam_role.task.arn
}

output "execution_role_arn" {
  value = aws_iam_role.exec.arn
}

output "file_system_id" {
  value = aws_efs_file_system.this.id
}

output "file_system_arn" {
  value = aws_efs_file_system.this.arn
}

output "efs_security_group_id" {
  value = aws_security_group.efs.id
}

# The user's EC2 capacity, null without it. The app reads these from the
# user record (/triple-c/users/<env>/<owner key>), not from the state.
output "ec2_cluster_arn" {
  value = one(aws_ecs_cluster.ec2[*].arn)
}

output "ec2_capacity_provider" {
  value = one(aws_ecs_capacity_provider.ec2[*].name)
}

output "ec2_instance_type" {
  value = local.ec2 ? var.ec2_instance_type : null
}

# Written only when every resource above was applied: a run that fails
# part-way never evaluates it, so the app reads its absence from the state
# as "the last onboarding didn't finish" (Incomplete, Join re-applies)
# rather than Joined. Keep depends_on listing every resource in the module.
output "onboarding_complete" {
  value = true
  depends_on = [
    aws_efs_file_system.this,
    aws_security_group.efs,
    aws_efs_mount_target.this,
    aws_efs_file_system_policy.this,
    aws_iam_role.task,
    aws_iam_role_policy.task,
    aws_iam_role_policy.task_bedrock,
    aws_iam_role.exec,
    aws_iam_role_policy.exec,
    aws_ecs_cluster.ec2,
    aws_iam_role.instance,
    aws_iam_role_policy.instance,
    aws_iam_instance_profile.instance,
    aws_launch_template.ec2,
    aws_autoscaling_group.ec2,
    aws_ecs_capacity_provider.ec2,
    aws_ecs_cluster_capacity_providers.ec2,
    aws_ssm_parameter.user,
  ]
}
