variable "region" {
  description = "AWS region"
  type        = string
  default     = "us-east-2"
}

variable "state_bucket_name" {
  description = "S3 bucket name for Terraform state"
  type        = string
  default     = "example-terraform-state-bucket"
}

variable "lock_table_name" {
  description = "DynamoDB table name for Terraform state locking"
  type        = string
  default     = "wavis-terraform-locks"
}
