terraform {
  required_version = ">= 1.5"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

provider "aws" {
  region = var.region
}

locals {
  project = "wavis"
  env     = "dev"
  tags = {
    Project     = local.project
    Environment = local.env
    ManagedBy   = "terraform"
  }
}

# ---------- VPC / Subnet (read-only, not managed by us) ----------

data "aws_vpc" "existing" {
  id = data.aws_subnet.existing.vpc_id
}

data "aws_subnet" "existing" {
  id = var.subnet_id
}

# CloudFront origin-facing prefix list.
# Requires ec2:DescribeManagedPrefixLists + ec2:GetManagedPrefixListEntries on the
# deploying IAM principal. If your user lacks those permissions, either:
#   1. Add them (see README), or
#   2. Set use_cf_prefix_list = false to fall back to your-IP-only access.
data "aws_ec2_managed_prefix_list" "cloudfront" {
  count = var.use_cf_prefix_list ? 1 : 0
  name  = "com.amazonaws.global.cloudfront.origin-facing"
}
