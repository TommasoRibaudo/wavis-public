variable "region" {
  description = "AWS region"
  type        = string
  default     = "us-east-2"
}

# ---------- EC2 ----------

variable "ami_id" {
  description = "AMI ID for the EC2 instance. Get from: aws ec2 describe-instances --instance-ids i-0123456789abcdef0 --query 'Reservations[0].Instances[0].ImageId' --output text --region us-east-2"
  type        = string
  # No default — must be set in terraform.tfvars after reading from the console
}

variable "instance_type" {
  description = "EC2 instance type"
  type        = string
  default     = "t3.medium"
}

variable "subnet_id" {
  description = "Subnet ID where the EC2 instance lives. Get from: aws ec2 describe-instances --instance-ids i-0123456789abcdef0 --query 'Reservations[0].Instances[0].SubnetId' --output text --region us-east-2"
  type        = string
  # No default — must be set in terraform.tfvars
}

variable "key_pair_name" {
  description = "Name of the EC2 key pair (as registered in AWS, not the file path)"
  type        = string
  default     = "wavis-backend-dev-jey"
}

# ---------- Repo (for reference / deploy scripts) ----------

variable "repo_url" {
  description = "Git repo URL"
  type        = string
  default     = "https://github.com/example/wavis.git"
}

variable "repo_branch" {
  description = "Git branch to checkout"
  type        = string
  default     = "dev"
}

# ---------- Network access ----------

variable "allowed_ssh_cidrs" {
  description = "CIDR blocks allowed to SSH into the instance. MUST be set to your IP/32."
  type        = list(string)
  # No default — forces explicit setting in terraform.tfvars
}

variable "allow_direct_backend" {
  description = "Allow direct access to port 3000 from allowed_ssh_cidrs (for dev testing). Disable in prod."
  type        = bool
  default     = false
}

variable "enable_ssm_access" {
  description = "Attach SSM Session Manager policy to the EC2 role (enables SSH-less access)."
  type        = bool
  default     = true
}

variable "use_cf_prefix_list" {
  description = "Use the AWS-managed CloudFront prefix list to restrict port 3000. Requires ec2:DescribeManagedPrefixLists + ec2:GetManagedPrefixListEntries on the deploying IAM user. Set false to fall back to allowed_ssh_cidrs only."
  type        = bool
  default     = false
}

# ---------- CloudFront ----------

variable "cf_origin_secret" {
  description = "Shared secret for CloudFront origin verification (X-Origin-Verify header). Must match CF_ORIGIN_SECRET in the backend .env."
  type        = string
  sensitive   = true
  default     = ""
}

variable "waf_web_acl_arn" {
  description = "ARN of the WAF WebACL attached to the CloudFront distribution. Get from: aws cloudfront get-distribution --id E1234567890EXAMPLE --query 'Distribution.DistributionConfig.WebACLId' --output text"
  type        = string
  default     = ""
}

# ---------- Private Subnet Migration ----------

variable "enable_private_subnet" {
  description = "Create private subnet, NAT gateway, and associated networking. Defaults to false to preserve current public-subnet behavior."
  type        = bool
  default     = false
}

variable "livekit_deployment_mode" {
  description = "How LiveKit is deployed: 'colocated' (same instance), 'separate' (dedicated public EC2). Defaults to colocated."
  type        = string
  default     = "colocated"
  validation {
    condition     = contains(["colocated", "separate"], var.livekit_deployment_mode)
    error_message = "Must be 'colocated' or 'separate'."
  }
}

variable "alert_email" {
  description = "Email address for CloudWatch alarm notifications. Leave empty to skip SNS subscription."
  type        = string
  default     = ""
}

variable "private_subnet_cidr" {
  description = "CIDR block for the private subnet. Must not overlap existing subnets. Auto-calculated from VPC CIDR if empty."
  type        = string
  default     = ""
}

variable "public_nat_subnet_cidr" {
  description = "CIDR block for the public NAT subnet. Must not overlap existing subnets. Auto-calculated from VPC CIDR if empty."
  type        = string
  default     = ""
}

# ---------- RDS Scaling ----------

variable "enable_rds" {
  description = "Provision a dedicated RDS PostgreSQL instance and related resources. Defaults to false to preserve the current Docker Postgres topology."
  type        = bool
  default     = false
}

variable "rds_instance_class" {
  description = "RDS instance class for PostgreSQL."
  type        = string
  default     = "db.t3.micro"
}

variable "rds_allocated_storage" {
  description = "Initial RDS storage allocation in GB."
  type        = number
  default     = 20
}

variable "rds_max_allocated_storage" {
  description = "Maximum RDS storage autoscaling limit in GB."
  type        = number
  default     = 100
}

variable "rds_backup_retention" {
  description = "Automated backup retention period for RDS in days."
  type        = number
  default     = 7
}

variable "rds_master_username" {
  description = "Master username for the RDS PostgreSQL instance."
  type        = string
  default     = "wavis"
}

variable "rds_db_name" {
  description = "Database name to create in the RDS PostgreSQL instance."
  type        = string
  default     = "wavis"
}

variable "cpu_credit_alarm_threshold" {
  description = "Threshold for the EC2 CPUCreditBalance CloudWatch alarm."
  type        = number
  default     = 30
}

variable "rds_max_connections_threshold" {
  description = "Threshold for the RDS DatabaseConnections CloudWatch alarm."
  type        = number
  default     = 90
}
