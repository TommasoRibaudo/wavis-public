data "aws_instance" "livekit" {
  count = var.create_livekit_instance ? 0 : 1

  instance_id = var.existing_livekit_instance_id
}

resource "aws_iam_role" "livekit" {
  count = var.create_livekit_instance ? 1 : 0

  name = "${local.project}-${local.env}-livekit"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action    = "sts:AssumeRole"
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
    }]
  })

  tags = local.tags
}

resource "aws_iam_role_policy" "livekit_ssm_session_manager" {
  count = var.create_livekit_instance ? 1 : 0

  name = "ssm-session-manager"
  role = aws_iam_role.livekit[0].id

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

resource "aws_iam_role_policy" "livekit_ssm_read" {
  count = var.create_livekit_instance ? 1 : 0

  name = "ssm-read"
  role = aws_iam_role.livekit[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "SSMRead"
        Effect   = "Allow"
        Action   = ["ssm:GetParameter", "ssm:GetParameters", "ssm:GetParametersByPath"]
        Resource = "arn:aws:ssm:${var.region}:${data.aws_caller_identity.current.account_id}:parameter${local.ssm_prefix}/*"
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

resource "aws_iam_role_policy" "livekit_cloudwatch_agent" {
  count = var.create_livekit_instance ? 1 : 0

  name = "cloudwatch-agent"
  role = aws_iam_role.livekit[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "CloudWatchLogs"
        Effect = "Allow"
        Action = [
          "cloudwatch:PutMetricData",
          "logs:CreateLogGroup",
          "logs:CreateLogStream",
          "logs:PutLogEvents",
          "logs:DescribeLogStreams",
          "logs:DescribeLogGroups"
        ]
        Resource = "*"
      }
    ]
  })
}

resource "aws_iam_instance_profile" "livekit" {
  count = var.create_livekit_instance ? 1 : 0

  name = "${local.project}-${local.env}-livekit"
  role = aws_iam_role.livekit[0].name
}

resource "aws_eip" "livekit" {
  domain = "vpc"

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-livekit-eip"
  })
}

resource "aws_eip_association" "livekit" {
  instance_id   = local.livekit_instance_id
  allocation_id = aws_eip.livekit.id
}

resource "aws_instance" "livekit" {
  count = var.create_livekit_instance ? 1 : 0

  ami                         = var.livekit_ami_id
  instance_type               = var.livekit_instance_type
  subnet_id                   = values(aws_subnet.public)[0].id
  iam_instance_profile        = aws_iam_instance_profile.livekit[0].name
  vpc_security_group_ids      = [aws_security_group.livekit[0].id]
  associate_public_ip_address = true

  root_block_device {
    volume_size           = var.livekit_root_volume_size
    volume_type           = "gp3"
    delete_on_termination = true
    encrypted             = true
  }

  metadata_options {
    http_tokens   = "required"
    http_endpoint = "enabled"
  }

  user_data = <<-EOF
    #!/usr/bin/env bash
    set -euxo pipefail

    if command -v dnf >/dev/null 2>&1; then
      dnf update -y
      dnf install -y docker git curl jq
      systemctl enable --now docker
      usermod -aG docker ec2-user || true
    elif command -v apt-get >/dev/null 2>&1; then
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -y
      apt-get install -y docker.io git curl jq
      systemctl enable --now docker
      usermod -aG docker ubuntu || true
    fi

    mkdir -p /home/ec2-user/wavis
    chown -R ec2-user:ec2-user /home/ec2-user/wavis || true
  EOF

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
