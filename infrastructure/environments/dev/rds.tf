# ---------- RDS Scaling ----------
#
# Dedicated PostgreSQL instance and supporting resources, all gated behind
# var.enable_rds so the current Docker Postgres topology remains the default.

data "aws_availability_zones" "available" {
  count = var.enable_rds ? 1 : 0
  state = "available"
}

data "aws_subnets" "rds_secondary" {
  count = var.enable_rds ? 1 : 0

  filter {
    name   = "vpc-id"
    values = [data.aws_subnet.existing.vpc_id]
  }

  filter {
    name = "availability-zone"
    values = [
      for az in data.aws_availability_zones.available[0].names : az
      if az != data.aws_subnet.existing.availability_zone
    ]
  }
}

data "aws_subnet" "rds_secondary" {
  count = var.enable_rds ? 1 : 0
  id    = data.aws_subnets.rds_secondary[0].ids[0]
}

resource "aws_db_subnet_group" "wavis" {
  count = var.enable_rds ? 1 : 0

  name = "${local.project}-${local.env}-rds-subnet-group"
  subnet_ids = [
    try(aws_subnet.private[0].id, data.aws_subnet.existing.id),
    data.aws_subnet.rds_secondary[0].id,
  ]

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-rds-subnet-group"
  })
}

resource "aws_security_group" "rds" {
  count = var.enable_rds ? 1 : 0

  name_prefix = "${local.project}-${local.env}-rds-"
  description = "RDS PostgreSQL access from the Wavis backend only"
  vpc_id      = data.aws_subnet.existing.vpc_id

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-rds-sg"
  })

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_vpc_security_group_ingress_rule" "rds_postgres" {
  count = var.enable_rds ? 1 : 0

  security_group_id            = aws_security_group.rds[0].id
  referenced_security_group_id = aws_security_group.wavis.id
  from_port                    = 5432
  to_port                      = 5432
  ip_protocol                  = "tcp"
  description                  = "PostgreSQL from Wavis backend security group"
}

resource "aws_vpc_security_group_egress_rule" "rds_all_out" {
  count = var.enable_rds ? 1 : 0

  security_group_id = aws_security_group.rds[0].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "All outbound"
}

resource "aws_ssm_parameter" "rds_master_password" {
  count = var.enable_rds ? 1 : 0

  name  = "${local.ssm_prefix}/RDS_MASTER_PASSWORD"
  type  = "SecureString"
  value = "CHANGE-ME-rds-master-password"
  tags  = local.tags

  lifecycle {
    ignore_changes = [value]
  }
}

resource "aws_ssm_parameter" "enable_rds" {
  count = var.enable_rds ? 1 : 0

  name  = "${local.ssm_prefix}/ENABLE_RDS"
  type  = "String"
  value = "true"
  tags  = local.tags
}

resource "aws_db_instance" "wavis" {
  count = var.enable_rds ? 1 : 0

  identifier                = "${local.project}-${local.env}-postgres"
  engine                    = "postgres"
  engine_version            = "16"
  instance_class            = var.rds_instance_class
  allocated_storage         = var.rds_allocated_storage
  max_allocated_storage     = var.rds_max_allocated_storage
  storage_type              = "gp3"
  storage_encrypted         = true
  db_name                   = var.rds_db_name
  username                  = var.rds_master_username
  password                  = aws_ssm_parameter.rds_master_password[0].value
  port                      = 5432
  availability_zone         = data.aws_subnet.existing.availability_zone
  multi_az                  = false
  publicly_accessible       = false
  backup_retention_period   = var.rds_backup_retention
  skip_final_snapshot       = false
  final_snapshot_identifier = "${local.project}-${local.env}-postgres-final"
  db_subnet_group_name      = aws_db_subnet_group.wavis[0].name
  vpc_security_group_ids    = [aws_security_group.rds[0].id]
  copy_tags_to_snapshot     = true

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-postgres"
  })
}
