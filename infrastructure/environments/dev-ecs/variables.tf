variable "region" {
  description = "AWS region"
  type        = string
  default     = "us-east-2"
}

variable "environment_name" {
  description = "Environment name used in resource names and SSM prefixes"
  type        = string
  default     = "dev"
}

variable "vpc_cidr" {
  description = "CIDR block for the production VPC"
  type        = string
  default     = "10.40.0.0/16"
}

variable "public_subnet_cidrs" {
  description = "Two public subnet CIDRs, one per AZ"
  type        = list(string)
  default     = ["10.40.0.0/24", "10.40.1.0/24"]
}

variable "private_app_subnet_cidrs" {
  description = "Two private application subnet CIDRs, one per AZ"
  type        = list(string)
  default     = ["10.40.10.0/24", "10.40.11.0/24"]
}

variable "private_data_subnet_cidrs" {
  description = "Two private data subnet CIDRs, one per AZ"
  type        = list(string)
  default     = ["10.40.20.0/24", "10.40.21.0/24"]
}

variable "rds_instance_class" {
  description = "RDS instance class for PostgreSQL"
  type        = string
  default     = "db.t4g.small"
}

variable "rds_allocated_storage" {
  description = "Initial RDS storage allocation in GB"
  type        = number
  default     = 20
}

variable "rds_max_allocated_storage" {
  description = "Maximum RDS storage autoscaling limit in GB"
  type        = number
  default     = 100
}

variable "rds_backup_retention" {
  description = "Automated backup retention in days"
  type        = number
  default     = 14
}

variable "rds_master_username" {
  description = "Master username for RDS PostgreSQL"
  type        = string
  default     = "wavis"
}

variable "rds_db_name" {
  description = "Application database name"
  type        = string
  default     = "wavis"
}

variable "rds_identifier_suffix" {
  description = "Suffix used to keep the dev-ecs RDS resources distinct from the legacy dev database"
  type        = string
  default     = "ecs"
}

variable "rds_multi_az" {
  description = "Enable Multi-AZ for the production database"
  type        = bool
  default     = false
}

variable "alert_email" {
  description = "Optional email address for alarm notifications"
  type        = string
  default     = ""
}

variable "backend_public_hostname" {
  description = "Public DNS hostname for the production backend ALB"
  type        = string
  default     = ""
}

variable "backend_certificate_arn" {
  description = "Optional ACM certificate ARN for the production backend ALB HTTPS listener. Leave empty to let Terraform request one."
  type        = string
  default     = ""
}

variable "route53_hosted_zone_id" {
  description = "Optional Route53 hosted zone ID used to create backend and LiveKit DNS records and validate ACM certificates"
  type        = string
  default     = ""
}

variable "backend_enable_https" {
  description = "Enable HTTPS on the backend ALB. Disable for dev-style environments that use the ALB DNS name over HTTP."
  type        = bool
  default     = true
}

variable "backend_container_port" {
  description = "Container port exposed by wavis-backend"
  type        = number
  default     = 3000
}

variable "backend_health_check_path" {
  description = "Health check path for the backend target group"
  type        = string
  default     = "/health"
}

variable "backend_alb_idle_timeout_seconds" {
  description = "ALB idle timeout in seconds to support longer-lived WebSocket connections"
  type        = number
  default     = 3600
}

variable "backend_task_cpu" {
  description = "Fargate CPU units for the backend task"
  type        = number
  default     = 512
}

variable "backend_task_memory" {
  description = "Fargate memory in MiB for the backend task"
  type        = number
  default     = 1024
}

variable "backend_desired_count" {
  description = "Desired number of backend tasks"
  type        = number
  default     = 1
}

variable "backend_health_check_grace_period_seconds" {
  description = "Grace period before ECS enforces target group health checks"
  type        = number
  default     = 60
}

variable "backend_log_retention_days" {
  description = "CloudWatch log retention for backend ECS task logs"
  type        = number
  default     = 30
}

variable "backend_image_tag" {
  description = "Container image tag deployed to the backend ECS service"
  type        = string
  default     = "latest"
}

variable "backend_autoscaling_enabled" {
  description = "Enable ECS service autoscaling for the backend"
  type        = bool
  default     = false
}

variable "backend_autoscaling_min_capacity" {
  description = "Minimum backend task count when autoscaling is enabled"
  type        = number
  default     = 1
}

variable "backend_autoscaling_max_capacity" {
  description = "Maximum backend task count when autoscaling is enabled"
  type        = number
  default     = 2
}

variable "backend_autoscaling_cpu_target" {
  description = "Target ECS service CPU utilization percentage"
  type        = number
  default     = 60
}

variable "backend_autoscaling_memory_target" {
  description = "Target ECS service memory utilization percentage"
  type        = number
  default     = 70
}

variable "backend_cpu_alarm_threshold" {
  description = "CPU utilization threshold for backend ECS alarm"
  type        = number
  default     = 80
}

variable "backend_memory_alarm_threshold" {
  description = "Memory utilization threshold for backend ECS alarm"
  type        = number
  default     = 85
}

variable "backend_target_5xx_alarm_threshold" {
  description = "Threshold for backend ALB target 5XX responses over 5 minutes"
  type        = number
  default     = 5
}

variable "livekit_public_hostname" {
  description = "Public DNS hostname reserved for the LiveKit service"
  type        = string
}

variable "create_livekit_instance" {
  description = "Whether Terraform should create and manage a dedicated LiveKit instance"
  type        = bool
  default     = true
}

variable "existing_livekit_instance_id" {
  description = "Existing LiveKit EC2 instance ID to reuse when create_livekit_instance is false"
  type        = string
  default     = ""
}

variable "livekit_ami_id" {
  description = "AMI ID for the dedicated LiveKit EC2 instance"
  type        = string
}

variable "livekit_instance_type" {
  description = "EC2 instance type for the dedicated LiveKit host"
  type        = string
  default     = "c7a.large"
}

variable "livekit_root_volume_size" {
  description = "Root volume size in GB for the LiveKit instance"
  type        = number
  default     = 20
}

variable "livekit_cpu_alarm_threshold" {
  description = "CPU utilization threshold for the LiveKit EC2 alarm"
  type        = number
  default     = 80
}

variable "livekit_network_out_alarm_threshold_bytes" {
  description = "NetworkOut threshold in bytes over 5 minutes for the LiveKit host alarm"
  type        = number
  default     = 2000000000
}

variable "cloudfront_5xx_error_rate_alarm_threshold" {
  description = "CloudFront 5XX error rate threshold percentage"
  type        = number
  default     = 1
}

variable "rds_connections_alarm_threshold" {
  description = "Database connections threshold for the dev RDS instance"
  type        = number
  default     = 40
}
