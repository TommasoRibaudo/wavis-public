output "vpc_id" {
  value = aws_vpc.main.id
}

output "public_subnet_ids" {
  value = [for subnet in aws_subnet.public : subnet.id]
}

output "private_app_subnet_ids" {
  value = [for subnet in aws_subnet.private_app : subnet.id]
}

output "private_data_subnet_ids" {
  value = [for subnet in aws_subnet.private_data : subnet.id]
}

output "alb_security_group_id" {
  value = aws_security_group.alb.id
}

output "backend_security_group_id" {
  value = aws_security_group.backend.id
}

output "livekit_security_group_id" {
  value = var.create_livekit_instance ? aws_security_group.livekit[0].id : null
}

output "rds_security_group_id" {
  value = aws_security_group.rds.id
}

output "rds_endpoint" {
  value = aws_db_instance.postgres.address
}

output "rds_port" {
  value = aws_db_instance.postgres.port
}

output "ssm_prefix" {
  value = local.ssm_prefix
}

output "ecs_task_execution_role_arn" {
  value = aws_iam_role.ecs_task_execution.arn
}

output "backend_app_role_arn" {
  value = aws_iam_role.backend_app.arn
}

output "backend_ecr_repository_url" {
  value = aws_ecr_repository.backend.repository_url
}

output "backend_ecs_cluster_name" {
  value = aws_ecs_cluster.backend.name
}

output "backend_ecs_service_name" {
  value = aws_ecs_service.backend.name
}

output "backend_task_definition_arn" {
  value = aws_ecs_task_definition.backend.arn
}

output "backend_alb_dns_name" {
  value = aws_lb.backend.dns_name
}

output "backend_alb_zone_id" {
  value = aws_lb.backend.zone_id
}

output "backend_public_hostname" {
  value = var.backend_public_hostname
}

output "backend_cloudfront_domain" {
  value = aws_cloudfront_distribution.backend.domain_name
}

output "backend_cloudfront_url" {
  value = "https://${aws_cloudfront_distribution.backend.domain_name}"
}

output "backend_effective_certificate_arn" {
  value = local.backend_certificate_arn
}

output "livekit_instance_id" {
  value = local.livekit_instance_id
}

output "livekit_public_ip" {
  value = local.livekit_public_ip
}

output "livekit_private_ip" {
  value = local.livekit_private_ip
}

output "livekit_instance_profile_name" {
  value = local.livekit_instance_profile_name
}

output "alerts_topic_arn" {
  value = aws_sns_topic.alerts.arn
}

output "ops_dashboard_name" {
  value = aws_cloudwatch_dashboard.dev_ops.dashboard_name
}

output "github_livekit_deploy_role_arn" {
  value = aws_iam_role.github_livekit_deploy.arn
}

output "github_backend_deploy_role_arn" {
  value = aws_iam_role.github_backend_deploy.arn
}
