# ---------- Deploy Pipeline IAM (GitHub Actions OIDC) ----------
#
# Allows the GitHub Actions deploy workflow to assume an IAM role via
# OIDC and run ssm:SendCommand / ssm:GetCommandInvocation against the
# backend (and LiveKit, when separate) EC2 instances.
#
# Requirements: 6.3

# ── Data sources ────────────────────────────────────────────────────

data "aws_caller_identity" "current" {}
data "aws_region" "current" {}

# ── GitHub OIDC Provider ───────────────────────────────────────────

resource "aws_iam_openid_connect_provider" "github" {
  url = "https://token.actions.githubusercontent.com"

  client_id_list = ["sts.amazonaws.com"]

  # GitHub Actions OIDC thumbprint — required by AWS even though it
  # doesn't actually validate it for GitHub's provider.
  thumbprint_list = ["6938fd4d98bab03faadb97b34396831e3780aea1"]

  tags = merge(local.tags, {
    Name = "${local.project}-github-oidc-${local.env}"
  })
}

# ── Deploy Role ────────────────────────────────────────────────────

resource "aws_iam_role" "github_deploy" {
  name = "${local.project}-${local.env}-github-deploy"
  tags = local.tags

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Principal = {
        Federated = aws_iam_openid_connect_provider.github.arn
      }
      Action = "sts:AssumeRoleWithWebIdentity"
      Condition = {
        StringEquals = {
          "token.actions.githubusercontent.com:aud" = "sts.amazonaws.com"
        }
        StringLike = {
          "token.actions.githubusercontent.com:sub" = "repo:Davalf99/wavis:*"
        }
      }
    }]
  })
}

# ── SSM Policy (SendCommand + GetCommandInvocation) ────────────────

locals {
  # Instance ARNs that the deploy role can target with SSM commands.
  # Always includes the backend instance; adds the LiveKit instance
  # when livekit_deployment_mode = "separate".
  deploy_target_instance_arns = concat(
    [
      "arn:aws:ec2:${data.aws_region.current.name}:${data.aws_caller_identity.current.account_id}:instance/${aws_instance.wavis.id}",
    ],
    local.livekit_enabled ? [
      "arn:aws:ec2:${data.aws_region.current.name}:${data.aws_caller_identity.current.account_id}:instance/${aws_instance.livekit[0].id}",
    ] : []
  )
}

resource "aws_iam_role_policy" "github_deploy_ssm" {
  name = "ssm-deploy"
  role = aws_iam_role.github_deploy.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "SSMSendCommand"
        Effect = "Allow"
        Action = "ssm:SendCommand"
        Resource = concat(
          local.deploy_target_instance_arns,
          # SendCommand also requires access to the SSM document
          ["arn:aws:ssm:${data.aws_region.current.name}::document/AWS-RunShellScript"]
        )
      },
      {
        Sid      = "SSMGetCommandInvocation"
        Effect   = "Allow"
        Action   = "ssm:GetCommandInvocation"
        Resource = "*"
      }
    ]
  })
}
