# ---------- Phase 3 — Dedicated LiveKit EC2 Instance ----------
#
# Provisions a separate public EC2 instance for LiveKit when
# livekit_deployment_mode = "separate". Launches in the same public
# subnet as the backend (no private subnet / NAT required).
#
# Resources:
#   aws_instance.livekit
#   aws_security_group.livekit
#   aws_iam_role.livekit
#   aws_iam_instance_profile.livekit
#   aws_iam_role_policy.livekit_ssm_session_manager
#   aws_iam_role_policy.livekit_ssm_read
#
# Requirements: 8.1, 8.2, 8.5, 8.7

locals {
  livekit_enabled = var.livekit_deployment_mode == "separate"
}

# ── Security Group ──────────────────────────────────────────────────
# Requirements: 8.2

resource "aws_security_group" "livekit" {
  count = local.livekit_enabled ? 1 : 0

  name_prefix = "${local.project}-livekit-${local.env}-"
  description = "LiveKit dedicated instance - media ports open to internet"
  vpc_id      = data.aws_subnet.existing.vpc_id
  tags        = merge(local.tags, { Name = "${local.project}-livekit-${local.env}-sg" })

  lifecycle {
    create_before_destroy = true
  }
}

# Ingress: LiveKit HTTP API + WebSocket (TCP 7880) from anywhere
resource "aws_vpc_security_group_ingress_rule" "livekit_http" {
  count             = local.livekit_enabled ? 1 : 0
  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 7880
  to_port           = 7880
  ip_protocol       = "tcp"
  description       = "LiveKit HTTP API + WebSocket (token-protected)"
}

# Ingress: LiveKit ICE TCP (7881) from anywhere
resource "aws_vpc_security_group_ingress_rule" "livekit_ice_tcp" {
  count             = local.livekit_enabled ? 1 : 0
  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 7881
  to_port           = 7881
  ip_protocol       = "tcp"
  description       = "LiveKit ICE over TCP (direct, token-protected)"
}

# Ingress: LiveKit ICE media UDP (50000-50100) from anywhere
resource "aws_vpc_security_group_ingress_rule" "livekit_ice_media_udp" {
  count             = local.livekit_enabled ? 1 : 0
  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 50000
  to_port           = 50100
  ip_protocol       = "udp"
  description       = "LiveKit ICE media UDP (direct, token-protected)"
}

# Ingress: TCP 7880 from backend SG (private, for token validation)
resource "aws_vpc_security_group_ingress_rule" "livekit_from_backend" {
  count                        = local.livekit_enabled ? 1 : 0
  security_group_id            = aws_security_group.livekit[0].id
  referenced_security_group_id = aws_security_group.wavis.id
  from_port                    = 7880
  to_port                      = 7880
  ip_protocol                  = "tcp"
  description                  = "LiveKit API from backend (token validation)"
}

# Egress: all outbound
resource "aws_vpc_security_group_egress_rule" "livekit_all_out" {
  count             = local.livekit_enabled ? 1 : 0
  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}

# ── IAM Role + Instance Profile ─────────────────────────────────────
# Requirements: 8.5, 8.7

resource "aws_iam_role" "livekit" {
  count = local.livekit_enabled ? 1 : 0
  name  = "${local.project}-${local.env}-livekit"
  tags  = local.tags

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action    = "sts:AssumeRole"
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
    }]
  })
}

# SSM Session Manager — same 11 actions as the backend (iam.tf)
resource "aws_iam_role_policy" "livekit_ssm_session_manager" {
  count = local.livekit_enabled ? 1 : 0
  name  = "ssm-session-manager"
  role  = aws_iam_role.livekit[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "SSMCore"
        Effect = "Allow"
        Action = [
          "ssmmessages:CreateControlChannel",
          "ssmmessages:CreateDataChannel",
          "ssmmessages:OpenControlChannel",
          "ssmmessages:OpenDataChannel",
          "ssm:UpdateInstanceInformation",
          "ec2messages:AcknowledgeMessage",
          "ec2messages:DeleteMessage",
          "ec2messages:FailMessage",
          "ec2messages:GetEndpoint",
          "ec2messages:GetMessages",
          "ec2messages:SendReply"
        ]
        Resource = "*"
      }
    ]
  })
}

# SSM parameter read — scoped to the project's SSM prefix
resource "aws_iam_role_policy" "livekit_ssm_read" {
  count = local.livekit_enabled ? 1 : 0
  name  = "ssm-read"
  role  = aws_iam_role.livekit[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "SSMRead"
        Effect   = "Allow"
        Action   = ["ssm:GetParameter", "ssm:GetParameters", "ssm:GetParametersByPath"]
        Resource = "arn:aws:ssm:${var.region}:*:parameter/${local.project}/${local.env}/*"
      },
      {
        Sid      = "KMSDecryptSSM"
        Effect   = "Allow"
        Action   = "kms:Decrypt"
        Resource = "*"
        Condition = {
          StringEquals = {
            "kms:ViaService" = "ssm.${var.region}.amazonaws.com"
          }
        }
      }
    ]
  })
}

resource "aws_iam_instance_profile" "livekit" {
  count = local.livekit_enabled ? 1 : 0
  name  = "${local.project}-${local.env}-livekit"
  role  = aws_iam_role.livekit[0].name
}

# ── EC2 Instance ────────────────────────────────────────────────────
# Requirements: 8.1, 8.7

resource "aws_instance" "livekit" {
  count = local.livekit_enabled ? 1 : 0

  ami                         = var.ami_id
  instance_type               = "t3.small"
  subnet_id                   = var.subnet_id
  iam_instance_profile        = aws_iam_instance_profile.livekit[0].name
  vpc_security_group_ids      = [aws_security_group.livekit[0].id]
  associate_public_ip_address = true

  root_block_device {
    volume_size           = 20
    volume_type           = "gp3"
    delete_on_termination = true
    encrypted             = true
  }

  metadata_options {
    http_tokens   = "required" # IMDSv2 only
    http_endpoint = "enabled"
  }

  tags = merge(local.tags, {
    Name = "${local.project}-livekit-${local.env}"
  })

  lifecycle {
    prevent_destroy = true

    ignore_changes = [
      ami,
      user_data,
      user_data_base64,
    ]
  }
}
