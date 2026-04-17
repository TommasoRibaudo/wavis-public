resource "aws_sns_topic" "alerts" {
  name = "${local.project}-${local.env}-alerts"

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-alerts"
  })
}

resource "aws_sns_topic_subscription" "email" {
  count = var.alert_email != "" ? 1 : 0

  topic_arn = aws_sns_topic.alerts.arn
  protocol  = "email"
  endpoint  = var.alert_email
}

resource "aws_cloudwatch_metric_alarm" "rds_cpu" {
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
    DBInstanceIdentifier = aws_db_instance.postgres.id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "rds_memory" {
  alarm_name          = "${local.project}-${local.env}-rds-memory"
  alarm_description   = "RDS freeable memory below 256 MB for 10 minutes"
  namespace           = "AWS/RDS"
  metric_name         = "FreeableMemory"
  statistic           = "Average"
  comparison_operator = "LessThanThreshold"
  threshold           = 268435456
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.postgres.id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "rds_connections" {
  alarm_name          = "${local.project}-${local.env}-rds-connections"
  alarm_description   = "RDS database connections above expected dev threshold"
  namespace           = "AWS/RDS"
  metric_name         = "DatabaseConnections"
  statistic           = "Average"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.rds_connections_alarm_threshold
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    DBInstanceIdentifier = aws_db_instance.postgres.id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "livekit_cpu" {
  alarm_name          = "${local.project}-${local.env}-livekit-cpu"
  alarm_description   = "LiveKit CPU utilization above threshold for 15 minutes"
  namespace           = "AWS/EC2"
  metric_name         = "CPUUtilization"
  statistic           = "Average"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.livekit_cpu_alarm_threshold
  period              = 300
  evaluation_periods  = 3

  dimensions = {
    InstanceId = local.livekit_instance_id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "livekit_cpu_credit_balance_low" {
  alarm_name          = "${local.project}-${local.env}-livekit-cpu-credits-low"
  alarm_description   = "LiveKit CPU credit balance is low on the burstable dev host"
  namespace           = "AWS/EC2"
  metric_name         = "CPUCreditBalance"
  statistic           = "Minimum"
  comparison_operator = "LessThanThreshold"
  threshold           = 30
  period              = 300
  evaluation_periods  = 2
  treat_missing_data  = "notBreaching"

  dimensions = {
    InstanceId = local.livekit_instance_id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "livekit_status_check" {
  alarm_name          = "${local.project}-${local.env}-livekit-status-check"
  alarm_description   = "LiveKit EC2 instance failed status checks"
  namespace           = "AWS/EC2"
  metric_name         = "StatusCheckFailed"
  statistic           = "Maximum"
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  period              = 60
  evaluation_periods  = 2

  dimensions = {
    InstanceId = local.livekit_instance_id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "livekit_network_out" {
  alarm_name          = "${local.project}-${local.env}-livekit-network-out"
  alarm_description   = "LiveKit NetworkOut above expected threshold for 10 minutes"
  namespace           = "AWS/EC2"
  metric_name         = "NetworkOut"
  statistic           = "Sum"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.livekit_network_out_alarm_threshold_bytes
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    InstanceId = local.livekit_instance_id
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "backend_memory_high" {
  alarm_name          = "${local.project}-${local.env}-backend-memory"
  alarm_description   = "Backend ECS memory utilization above threshold for 10 minutes"
  namespace           = "AWS/ECS"
  metric_name         = "MemoryUtilization"
  statistic           = "Average"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.backend_memory_alarm_threshold
  period              = 300
  evaluation_periods  = 2

  dimensions = {
    ClusterName = aws_ecs_cluster.backend.name
    ServiceName = aws_ecs_service.backend.name
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "backend_target_5xx" {
  alarm_name          = "${local.project}-${local.env}-backend-target-5xx"
  alarm_description   = "Backend target 5XX responses exceeded threshold over 5 minutes"
  namespace           = "AWS/ApplicationELB"
  metric_name         = "HTTPCode_Target_5XX_Count"
  statistic           = "Sum"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.backend_target_5xx_alarm_threshold
  period              = 300
  evaluation_periods  = 1

  dimensions = {
    LoadBalancer = aws_lb.backend.arn_suffix
    TargetGroup  = aws_lb_target_group.backend.arn_suffix
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "cloudfront_5xx" {
  alarm_name          = "${local.project}-${local.env}-cloudfront-5xx-rate"
  alarm_description   = "CloudFront 5XX error rate above threshold over 5 minutes"
  namespace           = "AWS/CloudFront"
  metric_name         = "5xxErrorRate"
  statistic           = "Average"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.cloudfront_5xx_error_rate_alarm_threshold
  period              = 300
  evaluation_periods  = 1

  dimensions = {
    DistributionId = aws_cloudfront_distribution.backend.id
    Region         = "Global"
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
  ok_actions    = [aws_sns_topic.alerts.arn]
}

resource "aws_cloudwatch_dashboard" "dev_ops" {
  dashboard_name = "${local.project}-${local.env}-ops"

  dashboard_body = jsonencode({
    widgets = [
      {
        type   = "text"
        x      = 0
        y      = 0
        width  = 24
        height = 2
        properties = {
          markdown = "# Wavis Dev ECS Operations\nTrack ECS backend health, edge errors, RDS pressure, and LiveKit host health from one dashboard."
        }
      },
      {
        type   = "metric"
        x      = 0
        y      = 2
        width  = 12
        height = 6
        properties = {
          title   = "Backend ECS Utilization"
          view    = "timeSeries"
          region  = var.region
          stacked = false
          metrics = [
            ["AWS/ECS", "CPUUtilization", "ClusterName", aws_ecs_cluster.backend.name, "ServiceName", aws_ecs_service.backend.name],
            [".", "MemoryUtilization", ".", ".", ".", "."]
          ]
        }
      },
      {
        type   = "metric"
        x      = 12
        y      = 2
        width  = 12
        height = 6
        properties = {
          title   = "ALB Target Health And Errors"
          view    = "timeSeries"
          region  = var.region
          stacked = false
          metrics = [
            ["AWS/ApplicationELB", "HealthyHostCount", "LoadBalancer", aws_lb.backend.arn_suffix, "TargetGroup", aws_lb_target_group.backend.arn_suffix],
            [".", "UnHealthyHostCount", ".", ".", ".", "."],
            [".", "HTTPCode_Target_5XX_Count", ".", ".", ".", "."]
          ]
        }
      },
      {
        type   = "metric"
        x      = 0
        y      = 8
        width  = 12
        height = 6
        properties = {
          title   = "CloudFront Errors And Requests"
          view    = "timeSeries"
          region  = "us-east-1"
          stacked = false
          metrics = [
            ["AWS/CloudFront", "Requests", "DistributionId", aws_cloudfront_distribution.backend.id, "Region", "Global"],
            [".", "4xxErrorRate", ".", ".", ".", "."],
            [".", "5xxErrorRate", ".", ".", ".", "."]
          ]
        }
      },
      {
        type   = "metric"
        x      = 12
        y      = 8
        width  = 12
        height = 6
        properties = {
          title   = "RDS Health"
          view    = "timeSeries"
          region  = var.region
          stacked = false
          metrics = [
            ["AWS/RDS", "CPUUtilization", "DBInstanceIdentifier", aws_db_instance.postgres.id],
            [".", "DatabaseConnections", ".", "."],
            [".", "FreeableMemory", ".", "."]
          ]
        }
      },
      {
        type   = "metric"
        x      = 0
        y      = 14
        width  = 12
        height = 6
        properties = {
          title   = "LiveKit Host Health"
          view    = "timeSeries"
          region  = var.region
          stacked = false
          metrics = [
            ["AWS/EC2", "CPUUtilization", "InstanceId", local.livekit_instance_id],
            [".", "StatusCheckFailed", ".", "."],
            [".", "NetworkOut", ".", "."]
          ]
        }
      },
      {
        type   = "metric"
        x      = 0
        y      = 20
        width  = 12
        height = 6
        properties = {
          title   = "LiveKit Burstable Credits"
          view    = "timeSeries"
          region  = var.region
          stacked = false
          metrics = [
            ["AWS/EC2", "CPUCreditBalance", "InstanceId", local.livekit_instance_id]
          ]
        }
      },
      {
        type   = "log"
        x      = 12
        y      = 14
        width  = 12
        height = 12
        properties = {
          title  = "Backend ECS Logs"
          region = var.region
          query  = "SOURCE '/aws/ecs/${local.project}-${local.env}-backend' | fields @timestamp, @message | sort @timestamp desc | limit 50"
          view   = "table"
        }
      }
    ]
  })
}
