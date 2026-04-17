resource "aws_db_subnet_group" "postgres" {
  name       = "${local.project}-${local.env}-${var.rds_identifier_suffix}-rds-subnet-group"
  subnet_ids = [for subnet in aws_subnet.private_data : subnet.id]

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-${var.rds_identifier_suffix}-rds-subnet-group"
  })
}

resource "aws_db_instance" "postgres" {
  identifier                = "${local.project}-${local.env}-${var.rds_identifier_suffix}-postgres"
  engine                    = "postgres"
  engine_version            = "16"
  instance_class            = var.rds_instance_class
  allocated_storage         = var.rds_allocated_storage
  max_allocated_storage     = var.rds_max_allocated_storage
  storage_type              = "gp3"
  storage_encrypted         = true
  db_name                   = var.rds_db_name
  username                  = var.rds_master_username
  password                  = data.aws_ssm_parameter.secrets["RDS_MASTER_PASSWORD"].value
  port                      = 5432
  multi_az                  = var.rds_multi_az
  publicly_accessible       = false
  backup_retention_period   = var.rds_backup_retention
  skip_final_snapshot       = false
  final_snapshot_identifier = "${local.project}-${local.env}-${var.rds_identifier_suffix}-postgres-final"
  db_subnet_group_name      = aws_db_subnet_group.postgres.name
  vpc_security_group_ids    = [aws_security_group.rds.id]
  copy_tags_to_snapshot     = true
  deletion_protection       = true

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-${var.rds_identifier_suffix}-postgres"
  })
}
