# ---------- EC2 ----------

output "instance_id" {
  value = aws_instance.wavis.id
}

output "instance_type" {
  value = aws_instance.wavis.instance_type
}

output "public_ip" {
  value = aws_instance.wavis.public_ip
}

output "private_ip" {
  value = aws_instance.wavis.private_ip
}

output "vpc_id" {
  value = data.aws_subnet.existing.vpc_id
}

output "subnet_id" {
  value = aws_instance.wavis.subnet_id
}

output "availability_zone" {
  value = aws_instance.wavis.availability_zone
}

output "security_group_id" {
  description = "Hardened SG managed by Terraform"
  value       = aws_security_group.wavis.id
}

output "ssh_command" {
  value = "ssh -i infrastructure/wavis-backend-dev-jey.pem ec2-user@${aws_instance.wavis.public_ip}"
}

output "backend_url" {
  value = "http://${aws_instance.wavis.public_ip}:3000"
}

output "health_check" {
  value = "curl http://${aws_instance.wavis.public_ip}:3000/health"
}

# ---------- CloudFront ----------

output "cloudfront_distribution_id" {
  value = aws_cloudfront_distribution.wavis.id
}

output "cloudfront_domain" {
  value = aws_cloudfront_distribution.wavis.domain_name
}

output "cloudfront_url" {
  value = "wss://${aws_cloudfront_distribution.wavis.domain_name}/ws"
}

# ---------- SSM ----------

output "ssm_prefix" {
  value = local.ssm_prefix
}

# ---------- Networking (Private Subnet Migration) ----------

output "nat_gateway_public_ip" {
  description = "NAT gateway public IP address"
  value       = var.enable_private_subnet ? aws_eip.nat[0].public_ip : null
}

output "private_subnet_id" {
  description = "Private subnet ID for the backend instance"
  value       = var.enable_private_subnet ? aws_subnet.private[0].id : null
}

output "public_nat_subnet_id" {
  description = "Public NAT subnet ID (NAT gateway + LiveKit)"
  value       = var.enable_private_subnet ? aws_subnet.public_nat[0].id : null
}

# ---------- Monitoring (NAT Gateway Alarms) ----------

output "sns_alerts_topic_arn" {
  description = "SNS topic ARN for CloudWatch alarm notifications"
  value       = (var.enable_private_subnet || var.enable_rds) ? aws_sns_topic.alerts[0].arn : null
}

output "nat_error_port_alarm_arn" {
  description = "CloudWatch alarm ARN for NAT ErrorPortAllocation"
  value       = var.enable_private_subnet ? aws_cloudwatch_metric_alarm.nat_error_port_alloc[0].arn : null
}

output "nat_packets_drop_alarm_arn" {
  description = "CloudWatch alarm ARN for NAT PacketsDropCount"
  value       = var.enable_private_subnet ? aws_cloudwatch_metric_alarm.nat_packets_drop[0].arn : null
}

output "ec2_cpu_credit_alarm_arn" {
  description = "CloudWatch alarm ARN for EC2 CPUCreditBalance"
  value       = (var.enable_private_subnet || var.enable_rds) ? aws_cloudwatch_metric_alarm.ec2_cpu_credit[0].arn : null
}

# ---------- RDS Scaling ----------

output "rds_endpoint" {
  description = "RDS PostgreSQL endpoint hostname"
  value       = var.enable_rds ? aws_db_instance.wavis[0].endpoint : null
}

output "rds_port" {
  description = "RDS PostgreSQL port"
  value       = var.enable_rds ? aws_db_instance.wavis[0].port : null
}

output "rds_db_name" {
  description = "RDS PostgreSQL database name"
  value       = var.enable_rds ? aws_db_instance.wavis[0].db_name : null
}

output "rds_security_group_id" {
  description = "Security group ID attached to the RDS instance"
  value       = var.enable_rds ? aws_security_group.rds[0].id : null
}

output "rds_cpu_alarm_arn" {
  description = "CloudWatch alarm ARN for RDS CPUUtilization"
  value       = var.enable_rds ? aws_cloudwatch_metric_alarm.rds_cpu[0].arn : null
}

output "rds_memory_alarm_arn" {
  description = "CloudWatch alarm ARN for RDS FreeableMemory"
  value       = var.enable_rds ? aws_cloudwatch_metric_alarm.rds_memory[0].arn : null
}

output "rds_connections_alarm_arn" {
  description = "CloudWatch alarm ARN for RDS DatabaseConnections"
  value       = var.enable_rds ? aws_cloudwatch_metric_alarm.rds_connections[0].arn : null
}

# ---------- LiveKit (Separate Instance) ----------

output "livekit_instance_id" {
  description = "LiveKit dedicated instance ID"
  value       = local.livekit_enabled ? aws_instance.livekit[0].id : null
}

output "livekit_public_ip" {
  description = "LiveKit instance public IP"
  value       = local.livekit_enabled ? aws_instance.livekit[0].public_ip : null
}

output "livekit_private_ip" {
  description = "LiveKit instance private IP"
  value       = local.livekit_enabled ? aws_instance.livekit[0].private_ip : null
}
