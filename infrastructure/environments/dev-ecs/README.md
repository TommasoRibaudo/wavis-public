# Wavis Dev ECS Environment

This is the current dev backend stack.

It replaces the old single-EC2 dev backend path with:

- a dedicated VPC and subnets
- `ECR`
- `ECS Fargate` for `wavis-backend`
- `ALB`
- CloudFront as the public dev entrypoint
- managed PostgreSQL on `RDS`
- IAM, alarms, and SSM-backed configuration

It intentionally reuses the existing dev LiveKit EC2 instance instead of
creating a second LiveKit node.

Current public dev backend URL:

- `https://<dev-cloudfront-url>`

Current runtime split:

- backend/signaling: `ECS Fargate`
- media/LiveKit: existing dev EC2 instance

## Current Status

- `dev-ecs` is the active dev backend path.
- The legacy EC2 backend, old CloudFront distribution, and old dev RDS were
  retired after the ECS migration was validated.
- The backend deploy pipeline now builds the image, pushes to `ECR`, updates
  `ECS`, waits for service stability, and runs a post-deploy smoke test.
- CI owns mutable backend task-definition/image rollouts in dev. Terraform
  ignores `task_definition` drift on the ECS service so unrelated infra applies
  do not rewind the running backend revision.
- The smoke test validates:
  - backend health
  - SFU room creation
  - invite-based second-peer join
  - `media_token` issuance
  - `participant_joined` signaling
- Baseline observability is live through the shared CloudWatch dashboard:
  - `wavis-dev-ops`

## Deploy Verification

Current automated dev deploy verification lives in:

- `.github/workflows/deploy-dev-ec2.yml`
- `scripts/dev-smoke-test.sh`
- `scripts/ws-sfu-test`
- `tools/smoke/smoke.py`
- `doc/deploy/dev-ecs-runbook.md`

The smoke test runs against the current public dev entrypoint and fails the
workflow if signaling or room join regresses after deployment.

The CloudWatch dashboard name is also exported from Terraform as:

- `ops_dashboard_name`

## Files

- `main.tf` — provider, locals, shared environment shape and backend env config
- `networking.tf` — VPC, public/private subnets, IGW, NAT, route tables
- `security_groups.tf` — ALB/backend/RDS/LiveKit security groups
- `iam.tf` — ECS execution role and backend app role
- `ecs_backend.tf` — ECR, ECS cluster/task/service, ALB, listener, autoscaling, backend alarms
- `dns.tf` — optional ACM certificate and DNS automation for later non-dev use
- `cloudfront.tf` — CloudFront distribution used as the dev HTTPS entrypoint
- `livekit.tf` — optional LiveKit EC2 host support; currently disabled in dev
- `monitoring.tf` — SNS topic, dev ops dashboard, and backend/RDS/LiveKit alarms
- `deploy-pipeline-iam.tf` — GitHub OIDC deploy role for LiveKit SSM deploys
- `rds.tf` — managed PostgreSQL
- `ssm.tf` — config and secret parameters
- `outputs.tf` — IDs, endpoints, and the CloudWatch dashboard name

## Usage

```bash
cd infrastructure/environments/dev-ecs
cp terraform.tfvars.example terraform.tfvars
terraform init
terraform plan
terraform apply
```

## Notes

- Secret parameters are created with placeholders and `ignore_changes = [value]`.
  This matches the existing dev pattern: Terraform creates them once, operators rotate them out-of-band without Terraform trying to overwrite them later.
- Local `terraform.tfvars` is intentionally ignored and should not be committed.
- The ECS task builds `DATABASE_URL` at runtime from the RDS endpoint plus the
  `RDS_MASTER_PASSWORD` secret so the database password is not duplicated into a
  plain SSM config parameter.
- If `backend_enable_https = false`, the backend ALB exposes plain HTTP on port
  `80` and can be used without a domain or ACM certificate. This is suitable
  for dev-only use with the ALB DNS name.
- In the current dev deployment, users do not hit the ALB directly. They use
  the CloudFront URL above.
- Set `environment_name = "dev"` to make resource names and SSM prefixes use the
  dev namespace.
- Set `create_livekit_instance = false` and `existing_livekit_instance_id` to
  reuse the current dev LiveKit EC2 instance instead of creating a new one.
- The backend defaults to `desired_count = 1` and autoscaling disabled. That is
  intentional until the backend state model is made safe for multi-task
  horizontal scaling.
- `RDS` has `deletion_protection = true` and `skip_final_snapshot = false`.
- The current baseline uses a single NAT gateway for simplicity and cost control. If cross-AZ egress resilience becomes important, split to one NAT per AZ later.
- `livekit_public_hostname` is stored as config so the backend can reference the
  existing dev LiveKit endpoint while LiveKit remains on EC2.
- The default LiveKit prod host size is `c7a.large`, reflecting the benchmark result that `t3.small` saturates CPU under the target 5-room profile.
- `ops_dashboard_name` exposes the shared CloudWatch dashboard for day-to-day
  ECS, edge, RDS, and LiveKit checks.
- `aws_ecs_service.backend` ignores `task_definition` drift so CI can own
  backend revision rollouts without Terraform rewinding the service on later
  infra applies.
- The public dev entrypoint is HTTPS via CloudFront, but the backend currently
  runs with `REQUIRE_TLS=false` because the CloudFront/WebSocket path does not
  present a trusted proto signal the backend can enforce reliably yet.
- The unresolved transport hardening gap above is currently tracked as a dev
  quality item. Deploy confidence comes from the smoke test rather than strict
  backend TLS enforcement on websocket upgrades.
