resource "aws_security_group" "alb" {
  name_prefix = "${local.project}-${local.env}-alb-"
  description = "ALB ingress for Wavis backend"
  vpc_id      = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-alb-sg"
  })

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_vpc_security_group_ingress_rule" "alb_https" {
  count = var.backend_enable_https ? 1 : 0

  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 443
  to_port           = 443
  ip_protocol       = "tcp"
  description       = "HTTPS from the internet"
}

resource "aws_vpc_security_group_ingress_rule" "alb_http" {
  count = var.backend_enable_https ? 0 : 1

  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 80
  to_port           = 80
  ip_protocol       = "tcp"
  description       = "HTTP from the internet"
}

resource "aws_vpc_security_group_egress_rule" "alb_all_out" {
  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}

resource "aws_security_group" "backend" {
  name_prefix = "${local.project}-${local.env}-backend-"
  description = "Backend ECS tasks / application runtime"
  vpc_id      = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-backend-sg"
  })

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_vpc_security_group_ingress_rule" "backend_from_alb" {
  security_group_id            = aws_security_group.backend.id
  referenced_security_group_id = aws_security_group.alb.id
  from_port                    = 3000
  to_port                      = 3000
  ip_protocol                  = "tcp"
  description                  = "Backend traffic from ALB"
}

resource "aws_vpc_security_group_egress_rule" "backend_all_out" {
  security_group_id = aws_security_group.backend.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}

resource "aws_security_group" "rds" {
  name_prefix = "${local.project}-${local.env}-rds-"
  description = "RDS PostgreSQL access from backend only"
  vpc_id      = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-rds-sg"
  })

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_vpc_security_group_ingress_rule" "rds_postgres" {
  security_group_id            = aws_security_group.rds.id
  referenced_security_group_id = aws_security_group.backend.id
  from_port                    = 5432
  to_port                      = 5432
  ip_protocol                  = "tcp"
  description                  = "Postgres from backend security group"
}

resource "aws_vpc_security_group_egress_rule" "rds_all_out" {
  security_group_id = aws_security_group.rds.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}

resource "aws_security_group" "livekit" {
  count = var.create_livekit_instance ? 1 : 0

  name_prefix = "${local.project}-${local.env}-livekit-"
  description = "Dedicated LiveKit instance security group"
  vpc_id      = aws_vpc.main.id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-livekit-sg"
  })

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_vpc_security_group_ingress_rule" "livekit_http" {
  count = var.create_livekit_instance ? 1 : 0

  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 7880
  to_port           = 7880
  ip_protocol       = "tcp"
  description       = "LiveKit HTTP API + WebSocket"
}

resource "aws_vpc_security_group_ingress_rule" "livekit_ice_tcp" {
  count = var.create_livekit_instance ? 1 : 0

  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 7881
  to_port           = 7881
  ip_protocol       = "tcp"
  description       = "LiveKit ICE over TCP"
}

resource "aws_vpc_security_group_ingress_rule" "livekit_udp" {
  count = var.create_livekit_instance ? 1 : 0

  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  from_port         = 51000
  to_port           = 51100
  ip_protocol       = "udp"
  description       = "LiveKit media UDP"
}

resource "aws_vpc_security_group_ingress_rule" "livekit_from_backend" {
  count = var.create_livekit_instance ? 1 : 0

  security_group_id            = aws_security_group.livekit[0].id
  referenced_security_group_id = aws_security_group.backend.id
  from_port                    = 7880
  to_port                      = 7880
  ip_protocol                  = "tcp"
  description                  = "LiveKit API access from backend"
}

resource "aws_vpc_security_group_egress_rule" "livekit_all_out" {
  count = var.create_livekit_instance ? 1 : 0

  security_group_id = aws_security_group.livekit[0].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}
