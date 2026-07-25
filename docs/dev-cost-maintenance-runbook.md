# Dev AWS Cost Maintenance Runbook

An operator checklist for the ~$31/mo dev cost reduction: retiring a
CloudFront distribution that was disabled but never deleted, detaching the dev
WAF, and downsizing RDS and the backend Fargate task to match measured load.

The Terraform-side changes ship in the repo. This runbook covers everything
that *cannot* — `terraform.tfvars` and both `terraform.tfstate` files are
gitignored, and the CloudFront/WAF work is deliberately done outside any
`terraform apply`. Run the steps in order; two of them are hard gates.

## Why the ordering matters

A previous maintenance pass aborted mid-apply: a CloudFront API error killed a
`terraform apply` *after* it had already destroyed a NAT Gateway. The lesson is
that a single apply is not transactional across resources, and an AWS-side
rejection on one resource can leave the rest half-changed.

So the CloudFront and WAF work here is done via CLI, **before** and **outside**
the apply that touches RDS and ECS. The corresponding code changes
(dropping `web_acl_id` from `ignore_changes` in `dev-ecs/cloudfront.tf`, and
deleting `dev/cloudfront.tf`) are no-op cleanups that converge state with
reality — they must follow the AWS-side action, never drive it.

## Baseline

Corrected against Cost Explorer over a 30-day window (`us-east-2` + global) —
not on-demand list prices, which materially overstate savings for a workload
running entirely on Fargate Spot.

| Line item | $/30d | Action |
|---|---|---|
| `DataTransfer-Out-Bytes` (LiveKit media) | 31.9 | Leave — real WebRTC egress |
| AWS WAF (all) | 24.4 | ~16.6 is ours, in two `CreatedByCloudFront-*` ACLs |
| `InstanceUsage:db.t4g.small` | 23.0 | **Halve** — 3.5% CPU, 8 peak connections |
| `LoadBalancerUsage` | 16.2 | Leave — intrinsic to the ALB |
| `PublicIPv4:InUseAddress` | 12.6 | Leave — `ServiceManaged: alb` |
| `BoxUsage:t3.small` (LiveKit) | 7.4 | Leave — see "Out of scope" |
| Fargate **Spot** vCPU + GB | 6.4 | **Halve the vCPU** — peak is 7.7% of 0.5 vCPU |

Target: ~$132/mo -> ~$101/mo, with measured headroom preserved on every
resource touched.

## 1. Delete the legacy CloudFront distribution + its orphan WAF (~$8.3/mo)

`E20JDZ30DRXUY6` (`dt2nm86rf5ksq.cloudfront.net`) is `Enabled: False`,
`Status: Deployed`, with origins pointing at a terminated backend
(`ec2-18-116-34-126`) and a stale LiveKit IP (`ec2-3-20-222-26`). Nothing
routes through it. It still carries `CreatedByCloudFront-aad5a11b`, which bills
`Global-WebACLV2` + `Global-RuleV2` regardless of the distribution being
disabled.

This is the only step with genuinely zero blast radius.

```bash
aws cloudfront get-distribution-config --id E20JDZ30DRXUY6 --query ETag --output text
aws cloudfront delete-distribution --id E20JDZ30DRXUY6 --if-match <ETag>

aws wafv2 get-web-acl --scope CLOUDFRONT --region us-east-1 \
  --name CreatedByCloudFront-aad5a11b --id 77ea3262-8c3f-4609-8b73-b311598552e0 \
  --query LockToken --output text
aws wafv2 delete-web-acl --scope CLOUDFRONT --region us-east-1 \
  --name CreatedByCloudFront-aad5a11b --id 77ea3262-8c3f-4609-8b73-b311598552e0 \
  --lock-token <token>
```

Then reconcile the legacy Terraform state. `infrastructure/environments/dev/cloudfront.tf`
has been deleted from the repo, so:

```bash
cd infrastructure/environments/dev
terraform plan
```

Refresh should drop the deleted distribution from state on its own. If the plan
still shows a pending destroy for `aws_cloudfront_distribution.wavis`, run
`terraform state rm aws_cloudfront_distribution.wavis`.

> **Do not run `terraform apply` in `environments/dev`.** That state predates the
> ECS migration and a full apply could try to recreate retired resources. The
> LiveKit instance there still carries `prevent_destroy = true`, which is why the
> config had to be cleaned rather than left to rot.

## 2. Detach and delete the active dev WAF (~$8.3/mo)

Do this **via CLI, not `terraform apply`.** The pricing plan can reject the
update, and an aborted apply is the failure mode this runbook exists to avoid.

```bash
DIST=<dev-ecs-distribution-id>
aws cloudfront get-distribution-config --id "$DIST" > cf.json
# ETag is at .ETag; the config body is at .DistributionConfig
# Set .DistributionConfig.WebACLId to "" and write the body alone to cfg.json
aws cloudfront update-distribution --id "$DIST" --if-match <ETag> \
  --distribution-config file://cfg.json
```

> **Gate.** If this fails with `InvalidArgument: ... must have a web ACL resource`,
> the CloudFront security pricing plan is still active. **Stop this step**, revert
> the `dev-ecs/cloudfront.tf` hunk so `web_acl_id` stays in `ignore_changes`, and
> continue to step 3. The other ~$14/mo is unaffected.

On success:

```bash
aws wafv2 delete-web-acl --scope CLOUDFRONT --region us-east-1 \
  --name CreatedByCloudFront-89e82227 --id 39a3f385-5eae-44e9-a54e-c93e18e102d6 \
  --lock-token <token>
```

Then clear the stale `cloudfront_web_acl_id` in the local `terraform.tfvars` so it
stops misleading readers, and confirm `terraform plan` in `dev-ecs/` reports no
change on the distribution now that the `ignore_changes` entry is gone.

Whether prod should have a WAF stays an open question. `waf.tf` keeps the
re-enable path in code behind `enable_waf = true`, and the `precondition` in
`cloudfront.tf` still blocks a prod apply with WAF off — nothing is lost by
detaching in dev.

## 3. Update the local `terraform.tfvars`

`infrastructure/environments/dev-ecs/terraform.tfvars` is gitignored, so these
three values must be set by hand. `terraform.tfvars.example` carries the same
values with the full rationale.

```hcl
rds_instance_class                        = "db.t4g.micro"
backend_task_cpu                          = 256
rds_freeable_memory_alarm_threshold_bytes = 134217728  # 128 MB
```

Leave `backend_task_memory = 1024`. Halving it would save ~$0.50/mo and cut the
Argon2id concurrent-hash headroom from ~15 to ~7 — see the README note in
`environments/dev-ecs/`.

## 4. Plan, with a hard gate

```bash
cd infrastructure/environments/dev-ecs
terraform plan -out=cost.tfplan
```

> **Gate.** `aws_db_instance.postgres` must show `~ update in-place`. If it shows
> `-/+ destroy and then create`, **abort and apply nothing.** With
> `deletion_protection = true` and `skip_final_snapshot = false` (`rds.tf`), a
> forced replacement would attempt a final snapshot and drop the dev database.

The plan should contain these and nothing else:

- `aws_db_instance.postgres` — instance class, in place
- `aws_ecs_task_definition.backend` — replaced (a new revision; expected)
- `aws_cloudwatch_metric_alarm.rds_memory` — threshold + description
- `aws_cloudwatch_metric_alarm.rds_cpu_credit_balance_low` — created
- `aws_cloudwatch_dashboard.dev_ops` — updated

## 5. Apply

```bash
terraform apply cost.tfplan
```

Causes ~5–10 min of dev backend downtime while RDS modifies. Do not run it while
a deploy is in flight. Merging the infrastructure PR will not itself trigger one —
the `deploy-dev-ec2.yml` `paths:` filter does not include `infrastructure/**`.

## 6. Repoint the ECS service

`aws_ecs_service.backend` has `ignore_changes = [task_definition]`, so the new
revision is registered but idle. Terraform will not roll it out.

```bash
aws ecs update-service --cluster wavis-dev-backend --service wavis-dev-backend \
  --task-definition <new-arn> --force-new-deployment --region us-east-2
```

The next CI deploy would also pick it up — the workflow runs
`describe-task-definition --task-definition "$ECS_SERVICE"`, and the family name
equals the service name, so it resolves to the latest ACTIVE revision and carries
`cpu`/`memory` forward. Don't rely on that for a change you want verified now.

## 7. Verify

```powershell
Invoke-WebRequest -Uri "https://d1d06fp0adg6ml.cloudfront.net/health" -UseBasicParsing | Select-Object StatusCode

aws rds describe-db-instances --db-instance-identifier wavis-dev-ecs-postgres --region us-east-2 `
  --query "DBInstances[0].{Class:DBInstanceClass,Status:DBInstanceStatus,Storage:AllocatedStorage}"

aws ecs describe-services --cluster wavis-dev-backend --services wavis-dev-backend --region us-east-2 `
  --query "services[0].taskDefinition"
aws ecs describe-task-definition --task-definition <arn-from-above> --region us-east-2 `
  --query "taskDefinition.{cpu:cpu,memory:memory}"   # must be 256 / 1024

aws cloudfront list-distributions --query "DistributionList.Items[].[Id,Enabled,WebACLId]" --output text
aws wafv2 list-web-acls --scope CLOUDFRONT --region us-east-1 --query "WebACLs[].[Name,Id]" --output text

aws cloudwatch describe-alarms --state-value ALARM --region us-east-2 --query "MetricAlarms[].AlarmName" --output text
```

Then run `tools/smoke/smoke.py` against dev, and separately **register a new
account through the desktop app and join a voice room**. That manual pass is the
only check exercising Argon2id hashing and RDS writes together on the downsized
resources — the smoke test's two registrations are sequential, so they do not
cover concurrent hashing. Re-check `FreeableMemory` and the `rds-memory` alarm
afterward.

Leave it ~48h, then:

- On the `wavis-dev-ops` CloudWatch dashboard, confirm `CPUCreditBalance` on the
  RDS Health widget is flat or rising, not draining.
- On Cost Explorer, confirm `InstanceUsage:db.t4g.micro`, `Global-WebACLV2`, and
  `SpotUsage-Fargate-vCPU-Hours` have all dropped.

## Rollback

| Risk | Mitigation | Rollback |
|---|---|---|
| RDS plan shows replace, not in-place | Gate in step 4 | Abort before applying |
| `rds-memory` alarm flaps on a 1 GB box | Threshold -> 128 MB, description interpolated | Raise `rds_freeable_memory_alarm_threshold_bytes` |
| RDS CPU credits deplete (baseline 20% -> 10%) | `rds-cpu-credits-low` alarm + dashboard metric ship with the change | Revert class to `db.t4g.small`, in place |
| Argon2 OOM at lower memory | Avoided by design — memory stays 1024 MiB | n/a |
| WAF detach rejected by pricing plan | Step 2 is a CLI call outside any apply | No change made; drop the `cloudfront.tf` hunk |
| Apply aborts mid-flight | All CloudFront/WAF work precedes the apply | `aws ecs update-service` manual recovery |
| Legacy distribution recreated by a stray apply | Removed from config, not just from AWS | n/a |

Every item is reversible: the RDS class change is in-place in both directions,
task CPU is a tfvars value, and `enable_waf = true` recreates a WebACL from
`waf.tf`.

## Out of scope

- **LiveKit `t3.small` -> `t4g.small` (~$1.5/mo).** The objection is the migration,
  not the size: an x86->ARM change on a host with a pinned kernel and a prior
  crash-reboot incident, requiring a new AMI, re-pinning, EIP re-association, and a
  LiveKit redeploy. Poor risk-adjusted value. `environments/dev-ecs/README.md` also
  records that `t3.small` already saturates under the 5-room target profile — the
  open question there is arguably undersizing, not cost.
- **Making Argon2 memory configurable** (`PhraseConfig::default()` in
  `wavis-backend/src/main.rs` has no env override) and wrapping hashing in
  `spawn_blocking`. Both are real robustness items; neither belongs in a cost
  change, and the ~$0.50/mo they would unlock is not the reason to do them.
- **RDS Reserved Instance.** Downsizing first is strictly better — same order of
  saving, zero commitment, and it does not lock in a size the team may drop further.
- **Data transfer (~$32/mo).** Real WebRTC media egress. CloudFront cannot front
  UDP/RTP and LiveKit is already an SFU.
- **`kalawala-staging-booking-api-waf`** (~$7.8/mo, regional, `us-east-1`) — a
  different project sharing this AWS account. Flagged, not touched; worth asking
  whoever owns it.
- **`PublicIPv4:IdleAddress` (~$1.8/mo).** All three EIPs are currently associated;
  the charge appears transient.
