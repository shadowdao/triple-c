output "parameter_name" {
  description = "The SSM parameter holding the environment record."
  value       = aws_ssm_parameter.environment.name
}

output "cluster_arn" {
  value = aws_ecs_cluster.this.arn
}

output "security_group_id" {
  value = aws_security_group.tasks.id
}

output "vpc_id" {
  value = local.vpc_id
}

output "subnet_ids" {
  value = local.task_subnet_ids
}

output "assign_public_ip" {
  value = local.assign_public_ip
}

output "kms_key_arn" {
  value = aws_kms_key.this.arn
}

output "log_group" {
  value = aws_cloudwatch_log_group.this.name
}
