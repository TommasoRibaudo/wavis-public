# ---------- Private Subnet Migration — Monitoring ----------
#
# CloudWatch alarms for NAT gateway health and SNS notifications.
# All resources gated behind var.enable_private_subnet.
#
# Requirements: 12.1, 12.2, 12.3

# ── SNS Topic for alarm notifications ──────────────────────────────

resource "aws_sns_topic" "alerts" {
  count = (var.enable_private_subnet || var.enable_rds) ? 1 : 0

  name = "${local.project}-${local.env}-alerts"

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-alerts"
  })
}

resource "aws_sns_topic_subscription" "email" {
  count = (var.enable_private_subnet || var.enable_rds) && var.alert_email != "" ? 1 : 0

  topic_arn = aws_sns_topic.alerts[0].arn
  protocol  = "email"
  endpoint  = var.alert_email
}

# ── CloudWatch Alarms — NAT Gateway ───────────────────────────────

resource "aws_cloudwatch_metric_alarm" "nat_error_port_alloc" {
  count = var.enable_private_subnet ? 1 : 0

  alarm_name          = "${local.project}-${local.env}-nat-error-port-alloc"
  alarm_description   = "NAT Gateway ErrorPortAllocation > 0 — port exhaustion detected"
  namespace           = "AWS/NATGateway"
  metric_name         = "ErrorPortAllocation"
  statistic           = "Sum"
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    NatGatewayId = aws_nat_gateway.main[0].id
  }

  alarm_actions = [aws_sns_topic.alerts[0].arn]
  ok_actions    = [aws_sns_topic.alerts[0].arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "nat_packets_drop" {
  count = var.enable_private_subnet ? 1 : 0

  alarm_name          = "${local.project}-${local.env}-nat-packets-drop"
  alarm_description   = "NAT Gateway PacketsDropCount > 100 over 5 min — significant packet loss"
  namespace           = "AWS/NATGateway"
  metric_name         = "PacketsDropCount"
  statistic           = "Sum"
  comparison_operator = "GreaterThanThreshold"
  threshold           = 100
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    NatGatewayId = aws_nat_gateway.main[0].id
  }

  alarm_actions = [aws_sns_topic.alerts[0].arn]
  ok_actions    = [aws_sns_topic.alerts[0].arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "ec2_cpu_credit" {
  count = (var.enable_private_subnet || var.enable_rds) ? 1 : 0

  alarm_name          = "${local.project}-${local.env}-ec2-cpu-credit"
  alarm_description   = "EC2 CPUCreditBalance below threshold for 10 minutes"
  namespace           = "AWS/EC2"
  metric_name         = "CPUCreditBalance"
  statistic           = "Minimum"
  comparison_operator = "LessThanThreshold"
  threshold           = var.cpu_credit_alarm_threshold
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    InstanceId = aws_instance.wavis.id
  }

  alarm_actions = [aws_sns_topic.alerts[0].arn]
  ok_actions    = [aws_sns_topic.alerts[0].arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "rds_cpu" {
  count = var.enable_rds ? 1 : 0

  alarm_name          = "${local.project}-${local.env}-rds-cpu"
  alarm_description   = "RDS CPU utilization above 80 percent for 15 minutes"
  namespace           = "AWS/RDS"
  metric_name         = "CPUUtilization"
  statistic           = "Average"
  comparison_operator = "GreaterThanThreshold"
  threshold           = 80
  period              = 300
  evaluation_periods  = 3

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.wavis[0].id
  }

  alarm_actions = [aws_sns_topic.alerts[0].arn]
  ok_actions    = [aws_sns_topic.alerts[0].arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "rds_memory" {
  count = var.enable_rds ? 1 : 0

  alarm_name          = "${local.project}-${local.env}-rds-memory"
  alarm_description   = "RDS freeable memory below 128 MB for 10 minutes"
  namespace           = "AWS/RDS"
  metric_name         = "FreeableMemory"
  statistic           = "Average"
  comparison_operator = "LessThanThreshold"
  threshold           = 134217728
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.wavis[0].id
  }

  alarm_actions = [aws_sns_topic.alerts[0].arn]
  ok_actions    = [aws_sns_topic.alerts[0].arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "rds_connections" {
  count = var.enable_rds ? 1 : 0

  alarm_name          = "${local.project}-${local.env}-rds-connections"
  alarm_description   = "RDS database connections above threshold for 10 minutes"
  namespace           = "AWS/RDS"
  metric_name         = "DatabaseConnections"
  statistic           = "Average"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.rds_max_connections_threshold
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.wavis[0].id
  }

  alarm_actions = [aws_sns_topic.alerts[0].arn]
  ok_actions    = [aws_sns_topic.alerts[0].arn]

  tags = local.tags
}
