# ---------- SSM Parameter Store ----------
#
# Secrets and config parameters under /wavis/dev/.
# These were originally created by the legacy dev/ (EC2) Terraform environment
# and read here as data sources. They are now owned by dev-ecs as resources
# so the legacy environment can be safely destroyed.
#
# IMPORTANT: All secret resources use `ignore_changes = [value]` so Terraform
# will not overwrite the real values in AWS with the placeholder defaults below.
# The placeholder values are only used if the parameter doesn't exist yet.
#
# Migration: after applying this change, run the import commands in
# infrastructure/environments/dev-ecs/migrate-ssm-ownership.sh

resource "aws_ssm_parameter" "secrets" {
  for_each = local.secrets

  name  = "${local.ssm_prefix}/${each.key}"
  type  = "SecureString"
  value = each.value
  tags  = local.tags

  lifecycle {
    ignore_changes = [value]
  }
}

# Config parameters that were previously read-only data sources.
# Now managed as resources so dev-ecs fully owns the SSM namespace.
resource "aws_ssm_parameter" "existing_config" {
  for_each = local.existing_config

  name  = "${local.ssm_prefix}/${each.key}"
  type  = "String"
  value = each.value
  tags  = local.tags

  lifecycle {
    ignore_changes = [value]
  }
}

resource "aws_ssm_parameter" "config" {
  for_each = local.managed_config

  name  = "${local.ssm_prefix}/${each.key}"
  type  = "String"
  value = each.value
  tags  = local.tags
}
