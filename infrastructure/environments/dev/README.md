# Wavis Dev Environment — Infrastructure

This environment documents the legacy EC2-based dev stack.

It is no longer the primary dev backend path. The current dev backend lives in:

- `infrastructure/environments/dev-ecs`

The old backend EC2 instance, old CloudFront distribution, and old dev RDS were
retired after the ECS migration was validated. Keep this environment only for
historical reference and any remaining cleanup work.

## Prerequisites

- AWS CLI configured (`aws sts get-caller-identity` works)
- Python 3 installed (`python --version`)
- Terraform >= 1.5 installed (`terraform --version`)
- IAM user with permissions for EC2, SSM, IAM PassRole, CloudFront

## What Terraform Manages

- **EC2 instance** — imported from console, `prevent_destroy` lifecycle
- **CloudFront distribution** — imported from console, `prevent_destroy` lifecycle
- **Security group** — hardened: HTTPS/443 from anywhere, SSH disabled (SSM), backend 3000 restricted to CF prefix list, LiveKit ports
- **IAM role + instance profile** — SSM parameter read, Session Manager, CloudWatch Agent
- **SSM Parameter Store** — all secrets and config

## What Terraform Does NOT Manage

- VPC / subnet (read-only data sources)
- GitHub Actions self-hosted runner registration
- Docker-compose services (managed by deploy workflow)

---

## First-Time Setup (New Environment)

### 1. Bootstrap State Backend

```powershell
cd infrastructure/bootstrap
terraform init
terraform apply
```

### 2. Gather Existing Resource Info

Before importing, get the values you need for `terraform.tfvars`:

```powershell
# AMI ID, Subnet ID, Instance Type
aws ec2 describe-instances --instance-ids i-0c62cb9c5229b086d --region us-east-2 `
  --query "Reservations[0].Instances[0].[ImageId,SubnetId,InstanceType]" --output text
```

Update `terraform.tfvars` with the real `ami_id` and `subnet_id` values.

### 3. Init + Import

```powershell
cd infrastructure/environments/dev
terraform init

# Import the EC2 instance
terraform import aws_instance.wavis i-0c62cb9c5229b086d

# Import the CloudFront distribution
terraform import aws_cloudfront_distribution.wavis dt2nm86rf5ksq
```

### 4. Verify — No Destructive Changes

```powershell
$env:TF_VAR_cf_origin_secret = "your-cf-origin-secret-here"
terraform plan
```

Review the plan carefully. You should see:
- EC2 instance: in-place updates only (tags, metadata_options, etc.) — never replace/destroy
- CloudFront: in-place updates only — never destroy
- SSM parameters: created (new ones like CF_ORIGIN_SECRET)
- No resources destroyed

If the plan wants to replace the EC2 instance (e.g., due to `root_block_device` mismatch), adjust `ec2.tf` to match the actual instance config, then re-plan.

### 5. Apply

```powershell
terraform apply
```

### 6. Rotate Secrets in SSM

Replace CHANGE-ME placeholders with real cryptographic values:

```powershell
$PG_PASSWORD = python -c "import secrets, string; print(''.join(secrets.choice(string.ascii_letters + string.digits) for _ in range(32)))"

aws ssm put-parameter --region us-east-2 `
  --name "/wavis/dev/POSTGRES_PASSWORD" `
  --value $PG_PASSWORD `
  --type SecureString --overwrite

aws ssm put-parameter --region us-east-2 `
  --name "/wavis/dev/DATABASE_URL" `
  --value "postgres://wavis:${PG_PASSWORD}@postgres:5432/wavis" `
  --type SecureString --overwrite

# Generate and set CF_ORIGIN_SECRET (must also be set in CloudFront custom header)
$CF_SECRET = python -c "import os, base64; print(base64.b64encode(os.urandom(32)).decode())"
aws ssm put-parameter --region us-east-2 `
  --name "/wavis/dev/CF_ORIGIN_SECRET" `
  --value $CF_SECRET `
  --type SecureString --overwrite

# Repeat for other secrets: AUTH_JWT_SECRET, AUTH_REFRESH_PEPPER,
# PHRASE_ENCRYPTION_KEY, PAIRING_CODE_PEPPER, SFU_JWT_SECRET,
# LIVEKIT_API_KEY, LIVEKIT_API_SECRET
```

### 7. Verify No Placeholders Remain

```powershell
aws ssm get-parameters-by-path --region us-east-2 `
  --path "/wavis/dev/" `
  --with-decryption `
  --query "Parameters[*].[Name,Value]" `
  --output table
```

---

## Day-to-Day Operations

### Applying Changes

```powershell
cd infrastructure/environments/dev
$env:TF_VAR_cf_origin_secret = "your-secret"
terraform plan
terraform apply
```

The `cf_origin_secret` variable is marked `sensitive` and should never be committed to `terraform.tfvars`. Always pass it via environment variable or `-var` flag.

### CloudFront Changes

CloudFront config changes (cache behaviors, origin settings, etc.) are now made in `cloudfront.tf` and applied via `terraform apply`. No more console edits.

If you need to update the origin verification secret:
1. Generate a new secret
2. Update SSM: `aws ssm put-parameter --name "/wavis/dev/CF_ORIGIN_SECRET" --value "new-secret" --type SecureString --overwrite`
3. Apply with the new secret: `$env:TF_VAR_cf_origin_secret = "new-secret"; terraform apply`
4. Redeploy the backend (so it picks up the new CF_ORIGIN_SECRET from SSM)

### Deploy Flow

Push to `dev` branch → GitHub Actions → self-hosted runner:
1. `git pull`
2. `deploy/fetch-ssm-env.sh` pulls SSM params into `.env`
3. `docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d`
4. Health check: `curl --fail http://localhost:3000/health`

## Notes

- `ssm.tf` contains CHANGE-ME placeholders — safe to commit. Real secrets live only in AWS SSM.
- `lifecycle { ignore_changes = [value] }` on secrets means Terraform creates them on first apply but never overwrites rotated values.
- `lifecycle { prevent_destroy = true }` on EC2 and CloudFront prevents accidental deletion.
- `lifecycle { ignore_changes = [ami, user_data] }` on EC2 prevents instance replacement from AMI drift.
- `fetch-ssm-env.sh` rejects any CHANGE-ME values and fails the deploy.
- Local dev is unaffected — `docker compose up` still uses defaults from the base `docker-compose.yml`.
