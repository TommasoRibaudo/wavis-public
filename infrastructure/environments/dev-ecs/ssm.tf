data "aws_ssm_parameter" "secrets" {
  for_each = local.secrets

  name            = "${local.ssm_prefix}/${each.key}"
  with_decryption = true
}

data "aws_ssm_parameter" "existing_config" {
  for_each = local.existing_config

  name = "${local.ssm_prefix}/${each.key}"
}

resource "aws_ssm_parameter" "config" {
  for_each = local.managed_config

  name  = "${local.ssm_prefix}/${each.key}"
  type  = "String"
  value = each.value
  tags  = local.tags
}
