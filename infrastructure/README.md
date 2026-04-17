# Wavis Infrastructure (Terraform)

The current primary dev backend environment is:

- `infrastructure/environments/dev-ecs`

That environment provisions the new dev backend stack on `ECS Fargate` and
reuses the existing dev LiveKit EC2 instance.

Current dev status:

- `dev-ecs` is live and serving the active dev backend traffic
- the deploy pipeline for `dev` now targets `ECR` + `ECS` instead of the legacy
  EC2 host
- post-deploy smoke coverage exists for health, room creation, second-peer
  join, and signaling sanity
- the legacy EC2 backend path under `infrastructure/environments/dev` has been
  retired from active use

The older EC2-based dev environment under `infrastructure/environments/dev` is
now legacy and should only be used for historical reference or controlled
cleanup work.

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/install) >= 1.5
- AWS CLI configured with credentials (`aws configure`) for us-east-2
- Existing dev LiveKit EC2 instance still available in AWS

## First-Time Setup (State Backend)

```bash
cd infrastructure/bootstrap
terraform init
terraform apply
```

Creates the S3 bucket + DynamoDB table for Terraform state. Run once.

## Deploy Current Dev Environment

```bash
cd infrastructure/environments/dev-ecs
terraform init
terraform apply
```

## Legacy Dev Environment

```bash
cd infrastructure/environments/dev
terraform init
terraform plan
```

## What Terraform Manages

- `dev-ecs` manages:
  - VPC, subnets, route tables, NAT
  - `ECR`, `ECS`, `ALB`, CloudFront
  - `RDS`
  - IAM roles and SSM parameters
- `environments/dev` manages only the legacy EC2-era dev resources

## What Terraform Does NOT Manage

- The reused dev LiveKit EC2 instance lifecycle in the current `dev-ecs` setup
- GitHub Actions self-hosted runner
- Application containers beyond the deploy workflows and ECS service definitions

## Notes

- Commit `terraform.tfvars.example`, not local `terraform.tfvars`.
- The current dev public backend URL is documented in
  `infrastructure/environments/dev-ecs/README.md`.
- The deploy workflow currently named `.github/workflows/deploy-dev-ec2.yml`
  now deploys the ECS-backed dev environment and is kept for continuity rather
  than because the backend still runs on EC2.
