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
  ]
}
